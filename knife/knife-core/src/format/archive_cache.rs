//! Process-wide, bounded caches for immutable archive inputs. No decoded images,
//! failed reads, or partially validated NX streams are admitted here.
use std::{collections::VecDeque, sync::{Mutex, OnceLock, atomic::{AtomicU64, Ordering}}};
use knife_perf::{Gauge, Metric, Scope};
use super::{ArchiveReadPlan, super::nx::NxKind};

const HEADERS: usize = 64;
const TINY_BYTES: usize = 64 * 1024;
const TINY_ITEM: usize = 4 * 1024;
const TINY_ITEMS: usize = 64;
static NEXT: AtomicU64 = AtomicU64::new(1);
pub(super) fn instance() -> u64 {
    NEXT.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |v| v.checked_add(1))
        .expect("archive instance identities exhausted")
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Key { pub instance: u64, pub offset: u64, pub stored: u16, pub unpacked: u16 }
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Plan { pub sizes: ArchiveReadPlan, pub kind: Option<NxKind> }
struct Tiny { key: Key, bytes: Box<[u8]> }
#[derive(Default)]
struct Cache {
    headers: VecDeque<(Key, Plan)>,
    tiny: VecDeque<Tiny>,
    bytes: usize,
}
impl Cache {
    fn header(&mut self, key: Key) -> Option<Plan> {
        let index = self.headers.iter().position(|(k,_)| *k==key)?;
        let entry = self.headers.remove(index)?;
        self.headers.push_back(entry);
        Some(entry.1)
    }
    fn store_header(&mut self, key: Key, plan: Plan) {
        if let Some(index)=self.headers.iter().position(|(k,_)| *k==key) { self.headers.remove(index); }
        while self.headers.len()>=HEADERS { self.headers.pop_front(); }
        self.headers.push_back((key,plan));
    }
    fn tiny(&mut self, key: Key) -> Option<Vec<u8>> {
        let index=self.tiny.iter().position(|v| v.key==key)?;
        let entry=self.tiny.remove(index)?;
        // Copy is intentional and bounded to 4 KiB; callers keep existing Cow
        // semantics. No Arc/pin can outlive this cache or escape its 64 KiB cap.
        let probe=Scope::new(Metric::ArchiveTinyHit);
        let out=entry.bytes.to_vec();
        probe.finish(out.len() as u64,1);
        self.tiny.push_back(entry); Some(out)
    }
    fn store_tiny(&mut self, key: Key, bytes: &[u8]) {
        if bytes.is_empty() || bytes.len()>TINY_ITEM || self.tiny.iter().any(|v| v.key==key) { return; }
        while self.bytes+bytes.len()>TINY_BYTES || self.tiny.len()>=TINY_ITEMS {
            let Some(old)=self.tiny.pop_front() else { break };
            self.bytes-=old.bytes.len();
            knife_perf::add(Metric::ArchiveTinyEvict,old.bytes.len() as u64,1);
        }
        let probe=Scope::new(Metric::ArchiveTinyStore);
        // Box<[u8]> charges exactly the complete retained input, with no hidden
        // spare Vec capacity. Metadata is separately bounded to 64+64 records.
        let bytes: Box<[u8]>=bytes.into(); self.bytes+=bytes.len();
        probe.finish(bytes.len() as u64,1);
        self.tiny.push_back(Tiny{key,bytes}); self.gauge();
    }
    fn invalidate(&mut self, key: Key) {
        self.headers.retain(|(k,_)| *k!=key);
        if let Some(i)=self.tiny.iter().position(|v| v.key==key) {
            let old=self.tiny.remove(i).unwrap(); self.bytes-=old.bytes.len(); self.gauge();
        }
    }
    fn gauge(&self) { knife_perf::gauge_set(Gauge::ArchiveTinyBytes,self.bytes as u64); }
}
fn global() -> &'static Mutex<Cache> { static CACHE: OnceLock<Mutex<Cache>>=OnceLock::new(); CACHE.get_or_init(Mutex::default) }
fn enabled() -> bool { knife_perf::optimizations::enabled(knife_perf::optimizations::RESOURCE_IO_CACHE) }
pub(super) fn header(key: Key) -> Option<Plan> {
    if !enabled() { return None; }
    let plan=global().lock().ok().and_then(|mut c| c.header(key));
    knife_perf::add(if plan.is_some(){Metric::ArchiveHeaderHit}else{Metric::ArchiveHeaderMiss},0,1);
    plan
}
pub(super) fn store_header(key: Key, plan: Plan) {
    if enabled() { if let Ok(mut c)=global().lock(){c.store_header(key,plan);} }
}
pub(super) fn tiny(key: Key, expected: usize) -> Option<Vec<u8>> {
    if !enabled() || expected>TINY_ITEM {return None;}
    let out=global().lock().ok().and_then(|mut c|c.tiny(key));
    if let Some(bytes)=out {
        if bytes.len()==expected {return Some(bytes);}
        invalidate(key);
    }
    None
}
pub(super) fn store_tiny(key: Key, bytes: &[u8]) {
    if enabled(){if let Ok(mut c)=global().lock(){c.store_tiny(key,bytes);}}
}
pub(super) fn invalidate(key: Key) { if let Ok(mut c)=global().lock(){c.invalidate(key);} }

#[cfg(test)]
mod tests {
    use super::*;
    fn key(i:u64)->Key{Key{instance:i,offset:0,stored:1,unpacked:1}}
    fn plan()->Plan{Plan{sizes:ArchiveReadPlan{stored_bytes:32,decoded_bytes:100},kind:Some(NxKind::Zlib)}}
    #[test]
    fn global_byte_limit_counts_all_instances_and_copies_do_not_pin() {
        let mut c=Cache::default();
        for i in 0..17 {c.store_tiny(key(i),&[i as u8;4096]);}
        assert_eq!(c.bytes,TINY_BYTES);assert_eq!(c.tiny.len(),16);
        assert!(c.tiny(key(0)).is_none());
        let copy=c.tiny(key(1)).unwrap();
        for i in 17..40 {c.store_tiny(key(i),&[i as u8;4096]);}
        assert_eq!(copy,[1;4096]);assert!(c.tiny(key(1)).is_none());assert_eq!(c.bytes,TINY_BYTES);
        c.store_tiny(key(99),&[9;4097]);assert!(c.tiny(key(99)).is_none());
    }
    #[test]
    fn metadata_and_tiny_count_lru_are_bounded_and_identity_is_not_pointer_based() {
        let mut c=Cache::default();
        for i in 0..64 {c.store_header(key(i),plan());c.store_tiny(key(i),&[i as u8]);}
        assert!(c.header(key(0)).is_some());assert!(c.tiny(key(0)).is_some());
        c.store_header(key(64),plan());c.store_tiny(key(64),&[64]);
        assert!(c.header(key(1)).is_none());assert!(c.tiny(key(1)).is_none());
        assert!(c.header(key(0)).is_some());assert_eq!(c.bytes,64);
        c.invalidate(key(0));assert!(c.header(key(0)).is_none());assert_eq!(c.bytes,63);
        assert_ne!(instance(),instance());
    }
}
