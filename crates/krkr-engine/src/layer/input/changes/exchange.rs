//! Exchange carries children; Swap leaves children in their structural places.
//! Each Part dispatches the existing input continuations before severing links.
use super::*;
use std::collections::VecDeque;

#[derive(Clone, Copy)]
struct Position {
    parent: Option<LayerId>,
    primary: bool,
    absolute: bool,
    order: i32,
}
impl Position {
    fn get(world: &Layers, id: LayerId) -> NativeResult<Self> {
        let parent = world.record(id)?.parent;
        Ok(Self {
            parent,
            primary: world.is_primary(id),
            absolute: parent.is_none_or(|id| world.records[id].absolute_order_mode),
            order: world.order(id, true)?,
        })
    }
}
#[derive(Clone, Copy)]
enum Action {
    DetachPrimary(LayerId, u8),
    AttachPrimary(LayerId),
    Part(LayerId, u8),
    Split,
    Join(LayerId, Option<LayerId>),
    Rank(LayerId, i32),
    Reorder,
}
struct Exchange {
    shared: Shared,
    window: WindowId,
    a: LayerId,
    b: LayerId,
    a_position: Position,
    b_position: Position,
    // First item is the bridge to detach, second its new parent.
    bridge: Option<(LayerId, LayerId)>,
    keep_children: bool,
    actions: VecDeque<Action>,
}
fn bridge(world: &Layers, child: LayerId, ancestor: LayerId) -> Option<LayerId> {
    let mut at = child;
    while let Some(parent) = world.records.get(at)?.parent {
        if parent == ancestor {
            return Some(at);
        }
        at = parent;
    }
    None
}
pub(super) fn start(
    shared: Shared,
    a: LayerId,
    b: LayerId,
    keep_children: bool,
) -> NativeResult<NativeStep> {
    let world = shared.borrow();
    let a_position = Position::get(&world, a)?;
    let b_position = Position::get(&world, b)?;
    let bridge = bridge(&world, a, b)
        .filter(|&id| id != a)
        .map(|id| (id, a))
        .or_else(|| bridge(&world, b, a).filter(|&id| id != b).map(|id| (id, b)));
    let mut actions = VecDeque::new();
    for (id, position) in [(a, a_position), (b, b_position)] {
        if position.primary {
            actions.push_back(Action::DetachPrimary(id, 0));
        }
    }
    actions.extend([Action::Part(a, 0), Action::Part(b, 0)]);
    if let Some((id, _)) = bridge {
        actions.push_back(Action::Part(id, 0));
    }
    actions.push_back(Action::Split);
    let window = world.record(a)?.window;
    drop(world);
    Ok(NativeStep::Continue(Box::new(Exchange {
        shared,
        window,
        a,
        b,
        a_position,
        b_position,
        bridge,
        keep_children,
        actions,
    })))
}
impl Trace for Exchange {
    fn trace(&self, visit: &mut dyn FnMut(Value)) {
        let world = self.shared.borrow();
        for id in [
            Some(self.a),
            Some(self.b),
            self.a_position.parent,
            self.b_position.parent,
        ] {
            trace_layer(&world, id, visit);
        }
        if let Some((a, b)) = self.bridge {
            trace_layer(&world, Some(a), visit);
            trace_layer(&world, Some(b), visit);
        }
        for action in &self.actions {
            match *action {
                Action::Join(id, parent) => {
                    trace_layer(&world, Some(id), visit);
                    trace_layer(&world, parent, visit);
                }
                Action::Part(id, _)
                | Action::Rank(id, _)
                | Action::DetachPrimary(id, _)
                | Action::AttachPrimary(id) => trace_layer(&world, Some(id), visit),
                _ => {}
            }
        }
    }
}
impl NativeContinuation for Exchange {
    fn resume(mut self: Box<Self>, _: &mut NativeCx<'_>, _: Value) -> NativeResult<NativeStep> {
        let shared = self.shared.clone();
        let Some(action) = self.actions.pop_front() else {
            return Ok(NativeStep::Return(Value::Void));
        };
        match action {
            Action::DetachPrimary(id, 0) => {
                self.actions.push_front(Action::DetachPrimary(id, 1));
                return Ok(focus::set(shared, self.window, None, false, self));
            }
            Action::DetachPrimary(id, 1) => {
                let mut world = shared.borrow_mut();
                world.release_capture(self.window);
                let hover = world
                    .input
                    .get_mut(&self.window)
                    .and_then(|s| s.hover.take());
                drop(world);
                self.actions.push_front(Action::DetachPrimary(id, 2));
                return Ok(send(&shared, hover, "onMouseLeave", vec![], self));
            }
            Action::DetachPrimary(id, 2) => {
                self.actions.push_front(Action::DetachPrimary(id, 3));
                return Ok(blur(shared, self.window, id, self));
            }
            Action::DetachPrimary(_, _) => {
                let mut world = shared.borrow_mut();
                world.primary.remove(&self.window);
                world.changed(self.window);
            }
            Action::AttachPrimary(id) => {
                let mut world = shared.borrow_mut();
                if let Some(old) = world.primary.get(&self.window).copied() {
                    self.actions.push_front(action);
                    self.actions.push_front(Action::DetachPrimary(old, 0));
                } else {
                    let r = world.record_mut(id)?;
                    r.visible = true;
                    r.opacity = 255;
                    world.primary.insert(self.window, id);
                    world.windows.borrow_mut().recheck_viewport(self.window);
                    // The manager identity covers detached layers as well.
                    for r in world
                        .records
                        .values_mut()
                        .filter(|r| r.window == self.window)
                    {
                        r.root = id;
                    }
                    world.changed(self.window);
                }
            }
            Action::Part(id, 0) => {
                self.actions.push_front(Action::Part(id, 1));
                return Ok(blur(shared, self.window, id, self));
            }
            Action::Part(id, 1) => {
                let mut world = shared.borrow_mut();
                world.release_capture_tree(id);
                let has_parent = world.record(id)?.parent.is_some();
                drop(world);
                if has_parent {
                    self.actions.push_front(Action::Part(id, 2));
                    return Ok(blur(shared, self.window, id, self));
                }
            }
            Action::Part(id, _) => {
                shared.borrow_mut().attach(id, None)?;
            }
            Action::Split => {
                let world = shared.borrow();
                let mut children = Vec::new();
                if self.keep_children {
                    for (id, parent) in [(self.a, self.b), (self.b, self.a)] {
                        for &child in &world.record(id)?.children {
                            children.push((child, parent, world.order(child, true)?));
                            self.actions.push_back(Action::Part(child, 0));
                        }
                    }
                }
                let a_parent = self
                    .b_position
                    .parent
                    .map(|id| if id == self.a { self.b } else { id });
                let b_parent = self
                    .a_position
                    .parent
                    .map(|id| if id == self.b { self.a } else { id });
                self.actions.extend([
                    Action::Join(self.a, a_parent),
                    Action::Join(self.b, b_parent),
                ]);
                for &(id, parent, _) in &children {
                    self.actions.push_back(Action::Join(id, Some(parent)));
                }
                if world.record(self.a)?.absolute_order_mode
                    && world.record(self.b)?.absolute_order_mode
                {
                    for (id, _, rank) in children {
                        self.actions.push_back(Action::Rank(id, rank));
                    }
                }
                if let Some((id, parent)) = self.bridge {
                    self.actions.push_back(Action::Join(id, Some(parent)));
                }
                if self.a_position.primary {
                    self.actions.push_back(Action::AttachPrimary(self.b));
                }
                if self.b_position.primary {
                    self.actions.push_back(Action::AttachPrimary(self.a));
                }
                self.actions.push_back(Action::Reorder);
            }
            Action::Join(id, parent) => {
                let mut world = shared.borrow_mut();
                if world.record(id)?.parent.is_some() {
                    self.actions.push_front(action);
                    self.actions.push_front(Action::Part(id, 0));
                } else {
                    world.validate_join(id, parent)?;
                    world.attach(id, parent)?;
                }
            }
            Action::Rank(id, rank) => {
                shared.borrow_mut().set_order(id, rank, true)?;
            }
            Action::Reorder => {
                let mut world = shared.borrow_mut();
                let a_parent = world.record(self.a)?.parent;
                let b_parent = world.record(self.b)?.parent;
                if a_parent == b_parent
                    && a_parent.is_some_and(|id| !world.records[id].absolute_order_mode)
                    && !self.a_position.absolute
                    && !self.b_position.absolute
                {
                    let mut moves = [
                        (self.b, self.a_position.order),
                        (self.a, self.b_position.order),
                    ];
                    moves.sort_by_key(|&(_, order)| order);
                    for (id, order) in moves {
                        world.set_order(id, order, false)?;
                    }
                } else {
                    for (id, parent, old) in [
                        (self.b, b_parent, self.b_position),
                        (self.a, a_parent, self.a_position),
                    ] {
                        if parent
                            .is_some_and(|id| world.records[id].absolute_order_mode == old.absolute)
                        {
                            world.set_order(id, old.order, old.absolute)?;
                        }
                    }
                }
            }
        }
        Ok(NativeStep::Continue(self))
    }
}
