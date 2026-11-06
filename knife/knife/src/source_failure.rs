//! Retry only transient allocation failures, once per source generation.
use std::fmt;

#[derive(Debug)]
pub struct SourceBudget;
impl fmt::Display for SourceBudget {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("shared source pins leave insufficient output budget")
    }
}
impl std::error::Error for SourceBudget {}

pub struct Failure {
    pub message: String,
    pub retryable: bool,
    pub retried: bool,
    pub pending: bool,
}
impl Failure {
    pub fn record(error: &anyhow::Error, previous: Option<&Self>) -> Self {
        Self {
            message: format!("{error:#}"),
            retryable: error.chain().any(|cause| cause.is::<std::collections::TryReserveError>()
                || cause.is::<SourceBudget>()),
            retried: previous.is_some_and(|old| old.retried),
            pending: false,
        }
    }
    pub fn begin_retry(&mut self, demand: bool) -> bool {
        if !demand || !self.retryable || self.retried { return false; }
        self.retried = true;
        self.pending = true;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn allocation_context_survives_and_retry_is_bounded() {
        let error = Vec::<u8>::new().try_reserve(usize::MAX).unwrap_err();
        let error = anyhow::Error::new(error).context("allocating bounded NX output");
        let mut first = Failure::record(&error, None);
        assert!(first.retryable);
        assert!(!first.begin_retry(false));
        assert!(first.begin_retry(true));
        assert!(!first.begin_retry(true));
        let mut second = Failure::record(&error, Some(&first));
        assert!(!second.pending);
        assert!(!second.begin_retry(true));
    }
    #[test]
    fn corruption_is_not_retried_but_pin_pressure_is() {
        let mut corrupt = Failure::record(&anyhow::anyhow!("invalid NX checksum"), None);
        assert!(!corrupt.begin_retry(true));
        let mut budget = Failure::record(&SourceBudget.into(), None);
        assert!(budget.begin_retry(true));
    }
}
