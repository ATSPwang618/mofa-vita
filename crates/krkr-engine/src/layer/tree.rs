//! Ordered tree metadata shared by property setters and transition exchanges.
use super::*;

impl Layers {
    pub(super) fn is_primary(&self, id: LayerId) -> bool {
        self.records
            .get(id)
            .is_some_and(|r| self.primary.get(&r.window) == Some(&id))
    }
    pub(super) fn order(&self, id: LayerId, absolute: bool) -> NativeResult<i32> {
        let r = self.record(id)?;
        let Some(parent) = r.parent else {
            return Ok(0);
        };
        let parent = self.record(parent)?;
        Ok(if absolute && parent.absolute_order_mode {
            r.absolute_order
        } else {
            parent
                .children
                .iter()
                .position(|&child| child == id)
                .unwrap_or(0) as i32
        })
    }
    pub(super) fn set_order_mode(&mut self, id: LayerId, absolute: bool) -> NativeResult<()> {
        let r = self.record_mut(id)?;
        if r.absolute_order_mode == absolute {
            return Ok(());
        }
        r.absolute_order_mode = absolute;
        if absolute {
            for index in 0..self.records[id].children.len() {
                let child = self.records[id].children[index];
                self.records[child].absolute_order = index as i32;
            }
        }
        Ok(())
    }
    pub(super) fn set_order(
        &mut self,
        id: LayerId,
        index: i32,
        absolute: bool,
    ) -> NativeResult<()> {
        let r = self.record(id)?;
        let window = r.window;
        let parent = r.parent.ok_or(NativeError::Message(
            "layer without a parent cannot change order",
        ))?;
        self.set_order_mode(parent, absolute)?;
        let from = self.order(id, false)? as usize;
        let to = if absolute {
            let children = &self.records[parent].children;
            let to = children
                .iter()
                .position(|&child| self.records[child].absolute_order >= index)
                .unwrap_or(children.len());
            to - usize::from(from < to)
        } else {
            (index.max(0) as usize).min(self.records[parent].children.len() - 1)
        };
        let r = self.record_mut(parent)?;
        if from != to {
            r.children.remove(from);
            r.children.insert(to, id);
            r.children_dirty = true;
            self.changed(window);
        }
        if absolute {
            self.records[id].absolute_order = index;
        }
        Ok(())
    }
    pub(super) fn move_sibling(
        &mut self,
        id: LayerId,
        other: LayerId,
        before: bool,
    ) -> NativeResult<()> {
        let parent = self.record(id)?.parent.ok_or(NativeError::Message(
            "layer without a parent cannot change order",
        ))?;
        self.set_order_mode(parent, false)?;
        if id == other || self.record(other)?.parent != Some(parent) {
            return Err(NativeError::Message(
                "order move requires a different sibling",
            ));
        }
        let from = self.order(id, false)?;
        let to = self.order(other, false)?;
        self.set_order(
            id,
            to + i32::from(before && from > to) - i32::from(!before && from < to),
            false,
        )
    }
    /// Joining in absolute mode appends above the last sibling. Its rank is
    /// then restored explicitly by Exchange when the two modes agree.
    pub(super) fn joined_order(&mut self, id: LayerId, parent: LayerId) {
        let r = &self.records[parent];
        if r.absolute_order_mode && r.children.len() >= 2 {
            let previous = r.children[r.children.len() - 2];
            self.records[id].absolute_order = self.records[previous].absolute_order.wrapping_add(1);
        }
    }
}
