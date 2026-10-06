#[derive(Debug, thiserror::Error)]
#[error("resource byte budget exceeded: requested={requested} used={used} limit={limit} bytes")]
pub struct BudgetError {
    pub requested: usize,
    pub used: usize,
    pub limit: usize,
}
type Result<T> = std::result::Result<T, BudgetError>;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

/// A permit follows the actual allocation, including in-flight GPU references.
/// Dropping a script handle alone does not release its resident-byte allowance.
#[derive(Clone, Debug)]
pub struct Budget(Arc<State>);
#[derive(Debug)]
struct State {
    limit: usize,
    used: AtomicUsize,
    parent: Option<Budget>,
    #[cfg(feature = "profiling")]
    profile_name: std::sync::OnceLock<&'static str>,
}
#[derive(Debug)]
pub struct Permit {
    budget: Budget,
    bytes: usize,
}
impl Budget {
    pub fn new(limit: usize) -> Self {
        Self(Arc::new(State {
            limit,
            used: AtomicUsize::new(0),
            parent: None,
            #[cfg(feature = "profiling")]
            profile_name: std::sync::OnceLock::new(),
        }))
    }
    /// A separately counted pool within this budget. Every reservation must
    /// fit both limits, and keeps both charges until its final permit drops.
    pub fn child(&self, limit: usize) -> Self {
        Self(Arc::new(State {
            limit,
            used: AtomicUsize::new(0),
            parent: Some(self.clone()),
            #[cfg(feature = "profiling")]
            profile_name: std::sync::OnceLock::new(),
        }))
    }
    pub fn used(&self) -> usize {
        self.0.used.load(Ordering::Relaxed)
    }
    /// Name a pool in performance captures; ordinary builds keep no telemetry.
    pub fn set_profile_name(&self, name: &'static str) {
        #[cfg(feature = "profiling")]
        {
            let _ = self.0.profile_name.set(name);
            crate::profile::counter(name, self.used() as u64);
        }
        #[cfg(not(feature = "profiling"))]
        let _ = name;
    }
    #[inline]
    fn record(&self, used: usize) {
        #[cfg(feature = "profiling")]
        if let Some(name) = self.0.profile_name.get() {
            crate::profile::counter(name, used as u64);
        }
        #[cfg(not(feature = "profiling"))]
        let _ = used;
    }
    pub fn limit(&self) -> usize {
        self.0.limit
    }
    pub fn available(&self) -> usize {
        let local = self.limit().saturating_sub(self.used());
        self.0
            .parent
            .as_ref()
            .map_or(local, |parent| local.min(parent.available()))
    }
    /// Capacity after the listed, distinct allocations are retired. This is
    /// only an admission estimate; their permits stay charged until dropped.
    /// Include sibling pools when checking an ancestor's shared limit.
    pub fn available_after_releasing<'a>(
        &self,
        permits: impl Iterator<Item = &'a Permit> + Clone,
    ) -> usize {
        let released = permits
            .clone()
            .filter(|permit| permit.charged_to(self))
            .fold(0usize, |bytes, permit| bytes.saturating_add(permit.bytes));
        let local = self
            .limit()
            .saturating_sub(self.used().saturating_sub(released));
        self.0.parent.as_ref().map_or(local, |parent| {
            local.min(parent.available_after_releasing(permits))
        })
    }
    pub fn reserve(&self, bytes: usize) -> Result<Permit> {
        self.charge(bytes)?;
        Ok(Permit {
            budget: self.clone(),
            bytes,
        })
    }
    fn charge(&self, bytes: usize) -> Result<()> {
        let previous = self
            .0
            .used
            .try_update(Ordering::Relaxed, Ordering::Relaxed, |used| {
                used.checked_add(bytes)
                    .filter(|&total| total <= self.0.limit)
            })
            .map_err(|used| BudgetError {
                requested: bytes,
                used,
                limit: self.0.limit,
            })?;
        if let Some(parent) = &self.0.parent
            && let Err(error) = parent.charge(bytes)
        {
            self.0.used.fetch_sub(bytes, Ordering::Relaxed);
            return Err(error);
        }
        self.record(previous + bytes);
        Ok(())
    }
    fn release(&self, bytes: usize) {
        let previous = self.0.used.fetch_sub(bytes, Ordering::Relaxed);
        if let Some(parent) = &self.0.parent {
            parent.release(bytes);
        }
        self.record(previous - bytes);
    }
}
impl Drop for Permit {
    fn drop(&mut self) {
        self.budget.release(self.bytes);
    }
}
impl Permit {
    fn charged_to(&self, budget: &Budget) -> bool {
        let mut owner = Some(&self.budget);
        while let Some(current) = owner {
            if Arc::ptr_eq(&current.0, &budget.0) {
                return true;
            }
            owner = current.0.parent.as_ref();
        }
        false
    }
    /// A cached allocation retains its original pool's charge when reused.
    pub fn belongs_to(&self, budget: &Budget) -> bool {
        Arc::ptr_eq(&self.budget.0, &budget.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn contended_reservations_cannot_exceed_the_limit() {
        let budget = Budget::new(8);
        let barrier = std::sync::Barrier::new(16);
        let admitted = std::sync::atomic::AtomicUsize::new(0);
        std::thread::scope(|scope| {
            for _ in 0..16 {
                scope.spawn(|| {
                    barrier.wait();
                    let permit = budget.reserve(1).ok();
                    if permit.is_some() {
                        admitted.fetch_add(1, Ordering::Relaxed);
                    }
                    // Hold every successful reservation until all attempts finish.
                    barrier.wait();
                    drop(permit);
                });
            }
        });
        assert_eq!(admitted.load(Ordering::Relaxed), 8);
        assert_eq!(budget.used(), 0);
    }
    #[test]
    fn reservations_follow_allocation_ownership_and_failed_growth_does_not_charge() {
        let budget = Budget::new(16);
        let allocation = Arc::new(budget.reserve(12).unwrap());
        let submitted = allocation.clone();
        drop(allocation);
        assert_eq!(budget.used(), 12);
        assert!(budget.reserve(5).is_err());
        assert!(budget.reserve(usize::MAX).is_err());
        drop(submitted);
        assert_eq!(budget.used(), 0);
        assert!(budget.reserve(16).is_ok());
        assert_eq!(budget.used(), 0);
    }
}
