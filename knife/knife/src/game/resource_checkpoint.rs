//! CPU-only recovery point. Publishing requires a successful game frame; a
//! failed/pending frame must not replace the last recoverable story snapshot.
use std::sync::Arc;

pub(super) struct Checkpoint<T> {
    candidate: Option<Arc<T>>,
    committed: Option<Arc<T>>,
    attempted: bool,
    pub notice: &'static str,
}

impl<T> Default for Checkpoint<T> {
    fn default() -> Self {
        Self { candidate: None, committed: None, attempted: false,
            notice: "正在保护最近的有效进度，请稍候。" }
    }
}

impl<T> Checkpoint<T> {
    pub fn stage(&mut self, point: Arc<T>) { self.candidate = Some(point); }

    pub fn presented(&mut self, ready: bool) {
        if ready { self.committed = self.candidate.clone(); }
    }

    pub fn point(&self) -> Option<&Arc<T>> { self.committed.as_ref() }

    pub fn begin_fault(&mut self) -> bool {
        if self.attempted { return false; }
        self.attempted = true;
        true
    }

    pub fn clear_fault(&mut self) {
        self.attempted = false;
        self.notice = "正在保护最近的有效进度，请稍候。";
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failed_or_pending_frame_keeps_previous_point() {
        let mut c = Checkpoint::default();
        let a = Arc::new((79, 800));
        c.stage(a.clone());
        c.presented(true);
        c.stage(Arc::new((79, 825)));
        c.presented(false);
        assert!(Arc::ptr_eq(c.point().unwrap(), &a));
        assert!(c.begin_fault());
        assert!(!c.begin_fault()); // no repeated writes at 60 Hz
        c.clear_fault();
        c.presented(true);
        assert_eq!(**c.point().unwrap(), (79, 825));
        assert!(c.begin_fault());
    }

    #[test]
    fn first_frame_failure_cannot_publish_unrendered_load() {
        let mut c = Checkpoint::default();
        c.stage(Arc::new(42));
        c.presented(false);
        assert!(c.point().is_none());
        assert!(c.begin_fault());
    }

    #[test]
    fn reset_does_not_reuse_another_playthrough() {
        let mut c = Checkpoint::default();
        c.stage(Arc::new(42));
        c.presented(true);
        c = Checkpoint::default();
        c.presented(true);
        assert!(c.point().is_none());
    }
}
