//! Immutable asset-name lookup: indexed streams need no main-thread stat calls.
//! A validated fixed-profile index is authoritative, including absent names.
//! Custom distributions without that index retain bounded memoized filesystem
//! fallback; CPU sound workers own a separate resolver for those checks.
//! Audio open reports a physically absent/corrupt indexed file.
use std::{cell::RefCell, collections::HashMap, path::{Path, PathBuf}, sync::Arc};
use knife_perf::{Metric, Scope};
const MEMO_LIMIT: usize = 1024;
pub const EMBEDDED: Option<&str> = Some(include_str!("../data/vita_stream_index_540p.txt"));
#[derive(Clone)]
struct Index { names: Arc<[&'static str]> }
impl Index {
    fn parse(text: &'static str, expected: &str) -> Option<Self> {
        let mut rows = text.lines();
        if rows.next()? != format!("# knife-stream-index-v1 {expected}") { return None; }
        let names: Vec<_> = rows.collect();
        if names.is_empty() || names.len() > 32_768 || names.iter().any(|p|
            !p.starts_with("stream/") || p.len() > 240 || p.contains('\\')
            || p.split('/').any(|part| part.is_empty() || part == ".." || part == "."))
            || names.windows(2).any(|p| p[0] >= p[1]) { return None; }
        Some(Self { names: names.into() })
    }
    fn contains(&self, name: &str) -> bool { self.names.binary_search(&name).is_ok() }
}
pub struct StreamResolver {
    root: PathBuf, index: Option<Index>, memo: RefCell<HashMap<String, Option<PathBuf>>>,
}
impl StreamResolver {
    pub fn new(root: &Path, source: Option<&'static str>, manifest: &str) -> Self {
        Self { root: root.into(), index: source.and_then(|s| Index::parse(s, manifest)), memo: Default::default() }
    }
    /// Immutable index owners are shared; custom filesystem memoization belongs
    /// to this worker alone. RefCell is moved, never shared between threads.
    pub fn fork_for_worker(&self) -> Self {
        Self { root: self.root.clone(), index: self.index.clone(), memo: Default::default() }
    }
    pub fn resolve(&self, rel: &str, accelerated: bool) -> Option<PathBuf> {
        let total = Scope::new(Metric::StreamResolve);
        let original = self.root.join(rel);
        let ogg_rel = Path::new(rel).with_extension("ogg");
        let ogg = self.root.join(&ogg_rel);
        if accelerated {
            if let Some(index) = &self.index {
                let found = if ogg_rel.to_str().is_some_and(|p| index.contains(p)) { Some(ogg.clone()) }
                    else if index.contains(rel) { Some(original.clone()) } else { None };
                knife_perf::add(if found.is_some() { Metric::StreamIndexHit } else { Metric::StreamIndexMiss }, 0, 0);
                // A missing BFSAR-only SE must not scan the large stream
                // directory twice, nor stall streams that are already playing.
                return found;
            }
            if let Some(found) = self.memo.borrow().get(rel) { return found.clone(); }
        }
        let stat = |p: &Path| {
            let probe = Scope::new(Metric::StreamStat); let found = p.is_file();
            probe.finish_hot(rel, 0, u64::from(found)); found
        };
        // Each candidate is checked at most once. The caller uses this same
        // existence result, rather than stat'ing the resolved Ogg a second time.
        let result = if stat(&ogg) { Some(ogg) }
            else if original != ogg && stat(&original) { Some(original) } else { None };
        if accelerated {
            let mut memo = self.memo.borrow_mut();
            if memo.len() >= MEMO_LIMIT { memo.clear(); }
            memo.insert(rel.into(), result.clone());
        }
        total.finish_hot(rel, 0, u64::from(result.is_some()));
        result
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    const FIXTURE: &str = "# knife-stream-index-v1 test\nstream/A.ogg\nstream/B.opus\n";
    #[test]
    fn manifest_bound_index_resolves_without_reading_directories() {
        let r = StreamResolver::new(Path::new("/nonexistent-index-test"), Some(FIXTURE), "test");
        assert_eq!(r.resolve("stream/A.opus", true), Some(PathBuf::from("/nonexistent-index-test/stream/A.ogg")));
        assert_eq!(r.resolve("stream/B.opus", true), Some(PathBuf::from("/nonexistent-index-test/stream/B.opus")));
        assert!(Index::parse(FIXTURE, "wrong").is_none());
        assert!(Index::parse("# knife-stream-index-v1 test\nstream/B\nstream/A\n", "test").is_none());
        assert!(Index::parse("# knife-stream-index-v1 test\nstream/../A\n", "test").is_none());
    }
    #[test]
    fn custom_paths_use_bounded_positive_and_negative_memo() {
        let dir = std::env::temp_dir().join(format!("knife-stream-index-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("stream")).unwrap();
        let r = StreamResolver::new(&dir, None, "test");
        let p = dir.join("stream/C.ogg");std::fs::write(&p, b"test").unwrap();
        assert_eq!(r.resolve("stream/C.opus", true), Some(p.clone()));
        std::fs::remove_file(&p).unwrap();
        assert_eq!(r.resolve("stream/C.opus", true), Some(p));
        assert_eq!(r.resolve("stream/C.opus", false), None);
        assert_eq!(r.resolve("stream/absent.opus", true), None);
        for n in 0..MEMO_LIMIT + 2 { r.resolve(&format!("stream/missing-{n}"), true); }
        assert!(r.memo.borrow().len() <= MEMO_LIMIT);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
