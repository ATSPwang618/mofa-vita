/// Return whether a target-ring spare contains exactly the memoized output.
///
/// Both the native owner identity and its content generation matter: the same
/// allocation may have been written since the memo was recorded.
pub fn can_restore(output: Option<(usize, u64)>, spare: Option<(usize, u64)>) -> bool {
    matches!((output, spare), (Some(output), Some(spare)) if output == spare)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn restore_requires_the_same_output_identity_and_generation() {
        assert!(can_restore(Some((11, 7)), Some((11, 7))));
        assert!(!can_restore(Some((11, 7)), Some((11, 8))));
        assert!(!can_restore(Some((11, 7)), Some((12, 7))));
        assert!(!can_restore(None, Some((11, 7))));
        assert!(!can_restore(Some((11, 7)), None));
    }
}
