use super::*;
impl Layers {
    pub(super) fn advance_paint(&mut self, drawing: &HashSet<WindowId>) -> HashSet<WindowId> {
        // A completed BeforeCompletion pass must reach publish before another
        // pass can start, even if onPaint itself requested the next repaint.
        let finished = std::mem::take(&mut self.paint_finished);
        let mut painting: HashSet<_> = self
            .records
            .values()
            .filter(|r| r.paint_queued)
            .map(|r| r.window)
            .collect();
        if !self
            .records
            .values()
            .any(|r| r.call_on_paint && !r.paint_queued && drawing.contains(&r.window))
        {
            return painting;
        }
        // BeforeCompletion walks the attached tree in child order, including
        // hidden back pages. Detached transition sources also need their paint.
        let mut roots: Vec<_> = self
            .primary
            .iter()
            .filter_map(|(window, &id)| drawing.contains(window).then_some(id))
            .collect();
        roots.extend(self.transitions.values().filter_map(|a| {
            drawing
                .contains(&self.records[a.destination].window)
                .then_some(a.source)
        }));
        let mut batches: HashMap<WindowId, Vec<LayerId>> = HashMap::new();
        let mut seen = HashSet::new();
        for root in roots {
            for id in self.nodes(Some(root)) {
                let r = &self.records[id];
                if !seen.insert(id)
                    || !r.ready
                    || r.shutdown
                    || painting.contains(&r.window)
                    || self.paint_active.contains(&r.window)
                    || finished.contains(&r.window)
                {
                    continue;
                }
                batches.entry(r.window).or_default().push(id);
            }
        }
        for (window, ids) in batches {
            // Check flags when each node is visited, since a parent's onPaint
            // can invalidate a later child in the same completion pass.
            if !ids.iter().any(|id| self.records[*id].call_on_paint) {
                continue;
            }
            if self
                .windows
                .borrow_mut()
                .post_paint(window, ids.clone())
                .is_ok()
            {
                for id in ids {
                    self.records[id].paint_queued = true;
                }
                painting.insert(window);
            }
        }
        painting
    }
}
pub(crate) fn callback(shared: Shared, ids: Vec<LayerId>) -> Box<dyn NativeContinuation> {
    Box::new(Paint {
        shared,
        ids,
        next: 0,
        active: None,
    })
}
struct Paint {
    shared: Shared,
    ids: Vec<LayerId>,
    next: usize,
    active: Option<WindowId>,
}
impl Trace for Paint {
    fn trace(&self, visit: &mut dyn FnMut(Value)) {
        let world = self.shared.borrow();
        for id in &self.ids {
            if let Some(r) = world.records.get(*id) {
                r.owner.trace(visit);
            }
        }
    }
}
impl Drop for Paint {
    fn drop(&mut self) {
        let mut world = self.shared.borrow_mut();
        if let Some(window) = self.active {
            world.paint_active.remove(&window);
        }
        for id in &self.ids {
            if let Some(r) = world.records.get_mut(*id) {
                r.paint_queued = false;
                let window = r.window;
                world.paint_finished.insert(window);
                world.changed(window);
            }
        }
    }
}
impl NativeContinuation for Paint {
    fn resume(mut self: Box<Self>, _: &mut NativeCx<'_>, _: Value) -> NativeResult<NativeStep> {
        let shared = self.shared.clone();
        let mut world = shared.borrow_mut();
        if self.active.is_none()
            && let Some(window) = self
                .ids
                .iter()
                .find_map(|id| world.records.get(*id).map(|r| r.window))
        {
            if !world.paint_active.insert(window) {
                drop(world);
                return Ok(NativeStep::Return(Value::Void));
            }
            self.active = Some(window);
        }
        let key = world.names["onPaint"];
        while let Some(&id) = self.ids.get(self.next) {
            self.next += 1;
            let Some(r) = world.records.get_mut(id) else {
                continue;
            };
            if r.shutdown || !r.call_on_paint {
                continue;
            }
            r.call_on_paint = false;
            let owner = object(r.owner);
            drop(world);
            // One resumable callback preserves parent/child order without
            // timer events splitting the pass at every individual layer.
            return Ok(NativeStep::CallMember {
                object: owner,
                key,
                arguments: vec![],
                continuation: self,
            });
        }
        drop(world);
        Ok(NativeStep::Return(Value::Void))
    }
}
