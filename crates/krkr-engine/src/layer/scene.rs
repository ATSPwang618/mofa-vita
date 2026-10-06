use super::*;
impl Layers {
    pub(crate) fn root_objects(&self, window: WindowId) -> Vec<Value> {
        self.root_ids(window)
            .into_iter()
            .map(|id| object(self.records[id].owner))
            .collect()
    }
    fn root_ids(&self, window: WindowId) -> Vec<LayerId> {
        let mut roots = self
            .records
            .iter()
            .filter(|(_, r)| r.window == window && r.parent.is_none() && r.ready && !r.shutdown)
            .map(|(id, r)| (r.creation_order, id))
            .collect::<Vec<_>>();
        roots.sort_by_key(|&(order, _)| order);
        roots.into_iter().map(|(_, id)| id).collect()
    }
    pub(super) fn input_primary(&self, window: WindowId) -> Option<LayerId> {
        if let Some(&index) = self.device_input.get(&window) {
            if index == 0 {
                return self
                    .records
                    .iter()
                    .filter(|(_, r)| {
                        r.window == window && r.parent.is_none() && r.ready && !r.shutdown
                    })
                    .min_by_key(|(_, r)| r.creation_order)
                    .map(|(id, _)| id);
            }
            self.root_ids(window).get(index).copied()
        } else {
            self.primary.get(&window).copied()
        }
    }
    pub(crate) fn set_device_input(&mut self, window: WindowId, index: Option<usize>) {
        let before = self.input_primary(window);
        if let Some(index) = index {
            self.device_input.insert(window, index);
        } else {
            self.device_input.remove(&window);
        }
        if before != self.input_primary(window) {
            self.release_capture(window);
            self.windows.borrow_mut().recheck_viewport(window);
        }
    }
    pub(super) fn set_geometry(&mut self, id: LayerId, geometry: Geometry) -> NativeResult<()> {
        let record = self.record_mut(id)?;
        if record.geometry == geometry {
            return Ok(());
        }
        let resized = record.geometry.size() != geometry.size();
        record.geometry = geometry;
        let window = record.window;
        self.changed(window);
        if resized && self.is_primary(id) {
            self.windows.borrow_mut().recheck_viewport(window);
        }
        Ok(())
    }
    pub(crate) fn invalidate_viewport(&mut self, window: WindowId) {
        self.dirty.insert(window);
    }
    pub(super) fn viewport(
        &self,
        window: WindowId,
    ) -> NativeResult<(krkr_protocol::viewport::Viewport, Size)> {
        let size = self
            .device_frames
            .get(&window)
            .map(|frame| frame.size)
            .unwrap_or_else(|| {
                self.primary
                    .get(&window)
                    .and_then(|id| self.records.get(*id))
                    .map_or(
                        Size {
                            width: 1,
                            height: 1,
                        },
                        |r| r.geometry.size(),
                    )
            });
        Ok((self.windows.borrow().viewport(window)?, size))
    }
    pub(super) fn input_point(
        &self,
        window: WindowId,
        point: (i32, i32),
    ) -> NativeResult<(i32, i32)> {
        let (viewport, size) = self.viewport(window)?;
        Ok(viewport.to_layer(size, point))
    }
    pub fn publish(&mut self) {
        if self.dirty.is_empty() {
            return;
        }
        let Ok(host) = self.host() else {
            return;
        };
        let mut dirty = std::mem::take(&mut self.dirty);
        self.publish_windows(host, dirty.drain());
        self.dirty = dirty;
    }
    pub(super) fn publish_window(&mut self, window: WindowId) {
        self.dirty.remove(&window);
        let Ok(host) = self.host() else {
            return;
        };
        self.publish_windows(host, std::iter::once(window));
    }
    fn publish_windows(&mut self, host: Client, dirty: impl Iterator<Item = WindowId>) {
        for window in dirty {
            if self.paint_active.contains(&window) {
                continue;
            }
            if !self.windows.borrow().contains(window) {
                self.device_input.remove(&window);
                self.device_frames.remove(&window);
                continue;
            }
            if let Some(frame) = self.device_frames.get(&window) {
                let scene = Scene {
                    viewport: self.windows.borrow().viewport(window).expect("live window"),
                    requires_op_seq: 0,
                    nodes: self.device_scene_nodes(window, frame),
                    transitions: Vec::new(),
                };
                if let Err(error) = host.publish(window, scene) {
                    self.failure.get_or_insert(error);
                }
                continue;
            }
            // Paint completion republishes this window. Retain the previous
            // host scene while its script prepares the next image.
            if self
                .records
                .values()
                .any(|r| r.window == window && r.paint_queued)
            {
                continue;
            }
            let primary = self.primary.get(&window).copied();
            // Hidden trees are needed by transition sources, but ordinary
            // frames only consume the visible primary tree. onPaint still
            // walks hidden pages independently of this presentation snapshot.
            let transitioning = self.has_transition(window);
            let mut nodes = Vec::new();
            let mut indices = transitioning.then(HashMap::new);
            let mut stack = Vec::new();
            if transitioning {
                stack.extend(self.records.iter().filter_map(|(id, r)| {
                    (r.window == window && r.parent.is_none() && Some(id) != primary)
                        .then_some((id, None))
                }));
            }
            if let Some(id) = primary {
                stack.push((id, None));
            }
            while let Some((id, parent)) = stack.pop() {
                let Some(r) = self.records.get(id).filter(|r| r.ready) else {
                    continue;
                };
                if !transitioning && (!r.visible || r.opacity == 0) {
                    continue;
                }
                let index = nodes.len();
                if let Some(indices) = &mut indices {
                    indices.insert(id, index);
                }
                let g = r.geometry;
                nodes.push(Node {
                    cache: r.cache.clone(),
                    parent,
                    visible: r.visible && (parent.is_some() || Some(id) == primary),
                    image: r.image.clone().filter(|_| r.has_main),
                    neutral_color: r.neutral,
                    rectangle: Rect {
                        left: g.left,
                        top: g.top,
                        width: g.size().width,
                        height: g.size().height,
                    },
                    image_left: g.image_left,
                    image_top: g.image_top,
                    blend: r.blend,
                    opacity: r.opacity,
                });
                stack.extend(r.children.iter().rev().map(|&id| (id, Some(index))));
            }
            let transitions = indices
                .as_ref()
                .map_or_else(Vec::new, |indices| self.scene_transitions(indices));
            self.append_movie_nodes(window, &mut nodes);
            // The channel bounds allocation capacity, not just node count.
            nodes.shrink_to_fit();
            if let Err(error) = host.publish(
                window,
                Scene {
                    viewport: self.windows.borrow().viewport(window).expect("live window"),
                    requires_op_seq: 0,
                    nodes,
                    transitions,
                },
            ) {
                self.failure.get_or_insert(error);
            }
        }
    }
    fn device_scene_nodes(&self, window: WindowId, frame: &device::Frame) -> Vec<Node> {
        let mut nodes = vec![Node {
            cache: None,
            parent: None,
            visible: true,
            image: Some(frame.image.clone()),
            neutral_color: 0,
            rectangle: frame.size.rect(),
            image_left: 0,
            image_top: 0,
            blend: Blend::Opaque,
            opacity: 255,
        }];
        self.append_movie_nodes(window, &mut nodes);
        nodes.shrink_to_fit();
        nodes
    }
    fn append_movie_nodes(&self, window: WindowId, nodes: &mut Vec<Node>) {
        let mut planes: Vec<_> = self
            .movies
            .values()
            .filter(|p| p.window == window && p.visible)
            .collect();
        planes.sort_unstable_by_key(|p| p.order);
        nodes.extend(planes.into_iter().map(|plane| Node {
            cache: None,
            parent: None,
            visible: true,
            image: Some(plane.image.clone()),
            neutral_color: 0,
            rectangle: plane.bounds,
            image_left: 0,
            image_top: 0,
            blend: Blend::Opaque,
            opacity: 255,
        }));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() -> (Shared, SlotMap<WindowId, ()>) {
        let mut runtime = tjs_runtime::Runtime::new();
        let windows = crate::window::install(
            &mut runtime.heap,
            crate::operations::Operations::new(32),
            Rc::new(RefCell::new(crate::events::Events::new(32))),
            Rc::new(tjs_runtime::clock::MonotonicClock::default()),
        )
        .unwrap();
        let layers = super::super::install(&mut runtime.heap, windows).unwrap();
        (layers, SlotMap::with_key())
    }

    fn image(layers: &mut Layers) -> ImageRef {
        ImageRef {
            id: layers.images.insert(()),
            lifetime: Arc::default(),
        }
    }

    #[test]
    fn device_frame_without_movies_keeps_its_single_opaque_node() {
        let (layers, mut windows) = setup();
        let window = windows.insert(());
        let mut layers = layers.borrow_mut();
        let frame = device::Frame {
            image: image(&mut layers),
            size: Size {
                width: 1024,
                height: 576,
            },
        };
        let nodes = layers.device_scene_nodes(window, &frame);
        assert_eq!(nodes.len(), 1);
        assert_eq!(nodes.capacity(), 1);
        let node = &nodes[0];
        assert_eq!(node.image.as_ref().unwrap().id, frame.image.id);
        assert_eq!(node.rectangle, frame.size.rect());
        assert_eq!(node.blend, Blend::Opaque);
        assert_eq!(node.opacity, 255);
        assert!(node.visible);
        assert!(node.parent.is_none());
    }

    #[test]
    fn device_frame_keeps_visible_movies_above_it_in_open_order() {
        let (layers, mut windows) = setup();
        let window = windows.insert(());
        let other = windows.insert(());
        let mut layers = layers.borrow_mut();
        let frame = device::Frame {
            image: image(&mut layers),
            size: Size {
                width: 1024,
                height: 576,
            },
        };
        let bounds = Rect {
            left: 17,
            top: -3,
            width: 800,
            height: 450,
        };
        let mut visible = Vec::new();
        for (order, movie_window, shown) in [
            (2, window, true),
            (3, window, false),
            (1, window, true),
            (0, other, true),
        ] {
            let image = image(&mut layers);
            if movie_window == window && shown {
                visible.push((order, image.id));
            }
            layers.movies.insert(
                image.id,
                video::Plane {
                    order,
                    window: movie_window,
                    image,
                    bounds,
                    visible: shown,
                },
            );
        }
        visible.sort_by_key(|&(order, _)| order);
        let nodes = layers.device_scene_nodes(window, &frame);
        assert_eq!(nodes.len(), 3);
        assert_eq!(nodes.capacity(), nodes.len());
        assert_eq!(nodes[0].image.as_ref().unwrap().id, frame.image.id);
        for (node, (_, image)) in nodes[1..].iter().zip(visible) {
            assert_eq!(node.image.as_ref().unwrap().id, image);
            assert_eq!(node.rectangle, bounds);
            assert!(node.visible);
            assert!(node.parent.is_none());
        }
        layers.movies.clear();
        assert_eq!(layers.device_scene_nodes(window, &frame).len(), 1);
    }
}
