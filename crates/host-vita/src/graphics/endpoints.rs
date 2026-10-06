use super::*;
use krkr_protocol::graphics::{Node, Size};

pub(super) struct Endpoint {
    nodes: Vec<Node>,
    logical: Size,
    physical: Size,
    image: Image,
}

fn same_nodes(a: &[Node], b: &[Node]) -> bool {
    a.len() == b.len()
        && a.iter().zip(b).all(|(a, b)| {
            a.parent == b.parent
                && a.visible == b.visible
                && a.opacity == b.opacity
                && a.rectangle == b.rectangle
                && a.blend == b.blend
                && a.image_left == b.image_left
                && a.image_top == b.image_top
                && a.neutral_color == b.neutral_color
                && a.image.as_ref().map(|r| r.id) == b.image.as_ref().map(|r| r.id)
        })
}

fn isolate(scene: &Scene, root: usize, size: Size) -> Result<Scene, String> {
    let mut result = Scene::default();
    let mut indices = vec![None; scene.nodes.len()];
    for (index, node) in scene.nodes.iter().enumerate() {
        let parent = node.parent.and_then(|p| indices[p]);
        if index != root && parent.is_none() {
            continue;
        }
        let mut node = node.clone();
        node.parent = parent;
        if index == root {
            node.parent = None;
            node.rectangle = size.rect();
            node.visible = true;
            node.opacity = 255;
        }
        indices[index] = Some(result.nodes.len());
        result.nodes.push(node);
    }
    residency::prune_scene_images(&mut result)?;
    Ok(result)
}

impl Graphics {
    pub(super) fn invalidate_endpoints(&mut self, command: &Command) {
        if self.endpoints.iter().all(Option::is_none) {
            return;
        }
        use Command::*;
        let target = match command {
            Pixel { .. }
            | ReadImage { .. }
            | ReadRegion { .. }
            | ReadProvince { .. }
            | ReadHitPlane { .. } => return,
            Create { image, .. } => *image,
            PreparedDraw(batch) => batch.image().id,
            Sprites { image, .. }
            | Scanlines { image, .. }
            | Warp { image, .. }
            | SnapshotMain { image, .. }
            | ComposeScene { image, .. }
            | Meshes { image, .. }
            | CopyPixels { image, .. }
            | Perspective { image, .. }
            | WrappedCopy { image, .. }
            | PiledCopy { image, .. }
            | Adjust { image, .. }
            | Text { image, .. }
            | Transform { image, .. }
            | Color { image, .. }
            | Operate { image, .. }
            | EnableImage { image, .. }
            | CreateProvince { image, .. }
            | Assign { image, .. }
            | AssignBitmap { image, .. }
            | Independ { image, .. }
            | BeginUpload { image, .. }
            | PrepareUpload { image, .. }
            | LoadCompressed { image, .. }
            | Upload { image, .. }
            | UploadScaled { image, .. }
            | UploadYuv { image, .. }
            | CopyYuv { image, .. }
            | PatchPixels { image, .. }
            | PatchRegion { image, .. }
            | Resize { image, .. }
            | Fill { image, .. }
            | Copy { image, .. } => image.id,
        };
        for endpoint in &mut self.endpoints {
            if endpoint.as_ref().is_some_and(|cached| {
                cached
                    .nodes
                    .iter()
                    .any(|node| node.image.as_ref().is_some_and(|image| image.id == target))
            }) {
                *endpoint = None;
            }
        }
    }

    /// Admit the two trees separately; only their composed pixels need to be
    /// pinned together for a transition. Writes invalidate the affected tree.
    pub(super) fn prepare_endpoints(
        &mut self,
        scene: &mut Scene,
        logical: Size,
        physical: Size,
    ) -> Result<HashMap<ImageId, Image>, String> {
        let mut prepared = HashMap::new();
        if scene.transitions.len() != 1 || !scene.transitions[0].with_children {
            self.endpoints = [None, None];
            return Ok(prepared);
        }
        let transition = scene.transitions[0].clone();
        // Ancestor endpoints can recurse through the active transition. Keep
        // those on the renderer's recursion-aware path.
        let ancestor = |root: usize, mut child: usize| {
            while let Some(parent) = scene.nodes[child].parent {
                if parent == root {
                    return true;
                }
                child = parent;
            }
            false
        };
        if transition.destination == transition.source
            || ancestor(transition.destination, transition.source)
            || ancestor(transition.source, transition.destination)
        {
            self.endpoints = [None, None];
            return Ok(prepared);
        }
        let first_only = transition.frame.phase == 0 && transition.custom.is_none();
        let last_only =
            transition.frame.phase >= transition.frame.effect.phases(transition.frame.size);
        let roots = [transition.destination, transition.source];
        let mut bitmaps = [None, None];
        for (slot, root) in roots.into_iter().enumerate() {
            if (slot == 0 && last_only && !first_only) || (slot == 1 && first_only) {
                self.endpoints[slot] = None;
                continue;
            }
            let endpoint = isolate(scene, root, transition.frame.size)?;
            let hit = self.endpoints[slot].as_ref().is_some_and(|cached| {
                cached.logical == logical
                    && cached.physical == physical
                    && same_nodes(&cached.nodes, &endpoint.nodes)
            });
            if !hit {
                self.endpoints[slot] = None;
                let needed = residency::scene_images(&endpoint);
                self.ensure_images(&needed)
                    .map_err(|e| format!("transition endpoint {slot}: {e}"))?;
                // Restoring inputs consumes the headroom reserved before
                // capture. Admit nested groups again with only this endpoint
                // protected, so the other tree can yield its storage.
                self.prepare_scene_inner(physical, Some(&endpoint))?;
                let images = needed
                    .iter()
                    .map(|id| {
                        self.images
                            .get(id)
                            .map(|image| (*id, image.shared()))
                            .ok_or_else(|| "transition image has been released".to_string())
                    })
                    .collect::<Result<HashMap<_, _>, _>>()?;
                let image = self
                    .gpu
                    .scene_endpoint(logical, physical, &endpoint, &images)
                    .map_err(|e| {
                        krkr_protocol::profile::marker("graphics.endpoint_failure", || {
                            let mut inventory: Vec<_> = self.images.iter().collect();
                            inventory.sort_by_key(|(_, image)| {
                                std::cmp::Reverse(image.resident_bytes())
                            });
                            format!(
                                "error={e}; staging={} scratch={} protected={needed:?}; nodes={:?}; images={inventory:?}",
                                self.gpu.staging.used(),
                                self.gpu.scratch.used(),
                                endpoint.nodes,
                            )
                        });
                        e.to_string()
                    })?;
                self.endpoints[slot] = Some(Endpoint {
                    nodes: endpoint.nodes,
                    logical,
                    physical,
                    image,
                });
            }
            bitmaps[slot] = Some(self.endpoints[slot].as_ref().unwrap().image.shared());
        }
        // Temporary IDs live only in this snapshot; skip every engine-owned ID.
        let mut ids = slotmap::SlotMap::<ImageId, ()>::with_key();
        let mut reference = |image: Image| {
            let id = loop {
                let id = ids.insert(());
                if !self.lifetimes.contains_key(&id) {
                    break id;
                }
            };
            prepared.insert(id, image);
            ImageRef {
                id,
                lifetime: Arc::default(),
            }
        };
        let destination = &mut scene.nodes[transition.destination];
        destination.image = bitmaps[0].take().map(&mut reference);
        destination.image_left = 0;
        destination.image_top = 0;
        destination.cache = None;
        let source = scene.nodes.len();
        scene.nodes.push(Node {
            cache: None,
            visible: false,
            parent: None,
            image: bitmaps[1].take().map(reference),
            neutral_color: 0,
            rectangle: transition.frame.size.rect(),
            image_left: 0,
            image_top: 0,
            blend: scene.nodes[transition.source].blend,
            opacity: 255,
        });
        for node in &mut scene.nodes {
            if node.parent == Some(transition.destination) {
                node.visible = false;
            }
        }
        scene.transitions[0].source = source;
        scene.transitions[0].with_children = false;
        residency::prune_scene_images(scene)?;
        Ok(prepared)
    }
}
