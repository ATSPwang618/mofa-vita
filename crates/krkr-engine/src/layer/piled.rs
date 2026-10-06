//! Complete pending script paint, then capture the subtree through the same GPU
//! compositor as window presentation. No CPU frame copy or alternate blending.
use super::{
    bindings::{State, integer, layer_id, rectangle},
    tasks::Change,
    *,
};
use tjs_core::{NativeContinuation, NativeCx, NativeStep};

struct Copy {
    shared: Shared,
    destination: LayerId,
    source: LayerId,
    x: i32,
    y: i32,
    rectangle: Rect,
    pending: std::collections::VecDeque<LayerId>,
    capture: bool,
    resized: bool,
}
impl Trace for Copy {
    fn trace(&self, visit: &mut dyn FnMut(Value)) {
        let world = self.shared.borrow();
        for id in [self.destination, self.source]
            .into_iter()
            .chain(self.pending.iter().copied())
        {
            if let Some(r) = world.records.get(id) {
                r.owner.trace(visit);
            }
        }
        world.trace_transitions(visit);
    }
}
impl NativeContinuation for Copy {
    fn resume(mut self: Box<Self>, cx: &mut NativeCx<'_>, _: Value) -> NativeResult<NativeStep> {
        while let Some(id) = self.pending.pop_front() {
            let call = {
                let mut world = self.shared.borrow_mut();
                let key = world.names["onPaint"];
                world
                    .records
                    .get_mut(id)
                    .filter(|r| r.ready && !r.shutdown && r.call_on_paint)
                    .map(|r| {
                        r.call_on_paint = false;
                        (object(r.owner), key)
                    })
            };
            if let Some((object, key)) = call {
                return Ok(NativeStep::CallMember {
                    object,
                    key,
                    arguments: vec![],
                    continuation: self,
                });
            }
        }
        if self.capture && !self.resized {
            let (owner, size) = {
                let world = self.shared.borrow();
                (
                    world.record(self.destination)?.owner,
                    world.record(self.source)?.geometry.size(),
                )
            };
            let step = cx.heap_mut().with_native_state::<State, _>(owner, |s| {
                let geometry = s.read(|r| r.geometry)?;
                s.geometry(geometry, size)
            })??;
            self.resized = true;
            return Ok(tjs_bind::flow::then(step, self));
        }
        let command = {
            let world = self.shared.borrow();
            let dst = world.record(self.destination)?;
            let src = world.record(self.source)?;
            src.image()?;
            dst.image()?;
            let rectangle = if self.capture {
                src.geometry.size().rect()
            } else {
                self.rectangle
            };
            let clip = if self.capture {
                dst.geometry.image_size.rect()
            } else {
                dst.geometry.clip
            };
            // Display bounds can be empty while the layer still owns an image.
            // An empty transfer must not request a zero-sized GPU surface.
            if krkr_render::blit::region(
                src.geometry.size(),
                dst.geometry.image_size,
                clip,
                rectangle,
                self.x,
                self.y,
            )
            .is_none()
            {
                return Ok(NativeStep::Return(Value::Void));
            }
            let scene = world.subtree_scene(self.source)?;
            krkr_protocol::graphics::Command::PiledCopy {
                image: dst.image()?.clone(),
                scene,
                size: src.geometry.size(),
                rectangle,
                x: self.x,
                y: self.y,
                clip,
            }
        };
        super::tasks::request(&self.shared, self.destination, command, Change::None, None)
    }
}
impl State {
    pub(super) fn piled_copy(
        &self,
        cx: &mut NativeCx<'_>,
        args: &[Value],
    ) -> NativeResult<NativeStep> {
        if args.len() < 7 {
            return Err(NativeError::Message(
                "piledCopy requires destination, source layer and source rectangle",
            ));
        }
        let lease = self.lease()?;
        let source = if object_id(args[2])? == cx.this() {
            lease.id
        } else {
            layer_id(cx.heap_mut(), args[2])?
        };
        let pending = {
            let world = lease.shared.borrow();
            world.record(lease.id)?.image()?;
            world.record(source)?.image()?;
            world.nodes(Some(source)).into()
        };
        Ok(NativeStep::Continue(Box::new(Copy {
            shared: lease.shared.clone(),
            destination: lease.id,
            source,
            x: integer(cx, args[0])?,
            y: integer(cx, args[1])?,
            rectangle: rectangle(cx, &args[3..])?,
            pending,
            capture: false,
            resized: false,
        })))
    }
}
/// Capture a full composed subtree into a destination main image, retaining the
/// destination's display geometry and province plane. Used by draw devices.
pub(crate) fn capture(
    cx: &mut NativeCx<'_>,
    destination: Value,
    source: Value,
) -> NativeResult<NativeStep> {
    let (shared, destination) = cx
        .heap_mut()
        .with_native_state::<State, _>(object_id(destination)?, |s| {
            s.lease().map(|lease| (lease.shared.clone(), lease.id))
        })??;
    let source = layer_id(cx.heap_mut(), source)?;
    let pending = {
        let world = shared.borrow();
        world.record(destination)?.image()?;
        world.record(source)?.image()?;
        world.nodes(Some(source)).into()
    };
    Ok(NativeStep::Continue(Box::new(Copy {
        shared,
        destination,
        source,
        x: 0,
        y: 0,
        rectangle: Rect {
            left: 0,
            top: 0,
            width: 0,
            height: 0,
        },
        pending,
        capture: true,
        resized: false,
    })))
}
impl Layers {
    pub(super) fn subtree_scene(&self, root: LayerId) -> NativeResult<Scene> {
        let mut scene = Scene::default();
        let mut indices = HashMap::new();
        let mut roots = std::collections::VecDeque::from([root]);
        let mut visited = HashSet::new();
        while let Some(root_id) = roots.pop_front() {
            if indices.contains_key(&root_id) {
                continue;
            }
            let mut stack = vec![(root_id, None)];
            while let Some((id, parent)) = stack.pop() {
                if !visited.insert(id) {
                    continue;
                }
                let r = self.record(id)?;
                let g = r.geometry;
                let index = scene.nodes.len();
                indices.insert(id, index);
                scene.nodes.push(Node {
                    cache: r.cache.clone(),
                    parent,
                    visible: if parent.is_none() {
                        id == root
                    } else {
                        r.visible
                    },
                    image: r.image.clone().filter(|_| r.has_main),
                    neutral_color: r.neutral,
                    rectangle: Rect {
                        left: if parent.is_none() { 0 } else { g.left },
                        top: if parent.is_none() { 0 } else { g.top },
                        width: g.size().width,
                        height: g.size().height,
                    },
                    image_left: g.image_left,
                    image_top: g.image_top,
                    blend: r.blend,
                    opacity: if parent.is_none() { 255 } else { r.opacity },
                });
                stack.extend(r.children.iter().rev().map(|&child| (child, Some(index))));
                if let Some(source) = self.transition_source(id) {
                    roots.push_back(source);
                }
            }
        }
        scene.transitions = self.scene_transitions(&indices);
        Ok(scene)
    }
}
