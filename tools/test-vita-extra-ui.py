"""Exercise the actual Vita byte worker and atlas owners on the host.

The temporary harness imports production source; only the platform telemetry
sink and the renderer metadata type are replaced. It never executes GXM or
claims Vita timing. Real assets are required explicitly; nothing is skipped.
"""
from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path
import subprocess

ROOT = Path(__file__).resolve().parents[1]


def block(text, marker):
    start = text.index(marker)
    opening = text.index('{', start)
    depth = 1
    for end in range(opening + 1, len(text)):
        depth += (text[end] == '{') - (text[end] == '}')
        if depth == 0:
            return text[start:end + 1]
    raise ValueError('unterminated source block: ' + marker)


def run(assets, output):
    output.mkdir(parents=True, exist_ok=False)
    code = (ROOT/'knife/knife/src/assets.rs').read_text()
    byte_stage = code[code.index('#[derive(Default)]\npub struct ByteStage'):code.index('\npub struct Assets')]
    byte_stage = byte_stage.replace('#[cfg(target_os = "vita")]\n', '')
    cohort = block(code, 'fn vita_extra_source_names(')
    manifest = f'''[package]
name = "knife-extra-ui-host"
version = "0.0.0"
edition = "2024"
[workspace]
[dependencies]
knife-core = {{ path = {json.dumps(str(ROOT/'knife/knife-core'))} }}
knife-perf = {{ path = {json.dumps(str(ROOT/'knife/knife-perf'))} }}
anyhow = "1.0"
tracing = "0.1"
'''
    (output/'Cargo.toml').write_text(manifest)
    source = HARNESS.replace('@ROOT@', ROOT.as_posix()).replace('@STAGE@', byte_stage).replace('@COHORT@', cohort)
    (output/'src').mkdir()
    (output/'src/lib.rs').write_text(source)
    files = [ROOT/name for name in ('knife/knife/src/assets.rs', 'knife/knife/src/atlas_cache.rs',
        'knife/knife-core/src/format/archive.rs', 'knife/knife-core/src/format/compact.rs',
        'knife/knife/src/vita_atlas_index.rs', 'knife/knife/data/vita_atlas_index_540p.rs',
        'knife/knife-render/src/byte_view.rs')]
    before = {p.relative_to(ROOT).as_posix(): hashlib.sha256(p.read_bytes()).hexdigest() for p in files}
    env = os.environ.copy()
    env['KNIFE_TEST_UI_ASSETS'] = str(assets.resolve())
    env.setdefault('CARGO_TARGET_DIR', str(Path.home()/'.cache/knife-extra-ui-host'))
    command = ['cargo', '+nightly-2026-05-28', 'test', '--offline', '--manifest-path',
        str(output/'Cargo.toml'), '--', '--test-threads=1', '--nocapture']
    result = subprocess.run(command, env=env, stdout=subprocess.PIPE, stderr=subprocess.STDOUT)
    (output/'output.log').write_bytes(result.stdout)
    print(result.stdout.decode(errors='replace'))
    if result.returncode:
        raise SystemExit(result.returncode)
    assert before == {p.relative_to(ROOT).as_posix(): hashlib.sha256(p.read_bytes()).hexdigest() for p in files}
    receipt = dict(schema='knife-extra-ui-host-v1', result='PASS', command=command,
        assets=str(assets.resolve()), source_sha256=before,
        limitations=['Host filesystem and allocator; no Vita timings, GXM pixels or hardware run.',
            'Assertions cover the production file reader, byte-stage worker, cache ownership and embedded metadata.'])
    (output/'receipt.json').write_text(json.dumps(receipt, indent=2)+'\n')


HARNESS = r'''
#![allow(dead_code)]
extern crate self as knife_render;
extern crate self as knife_platform_vita;
use std::sync::{Arc, atomic::{AtomicBool, AtomicUsize, Ordering}};
use anyhow::Result;
use knife_core::format::{archive::{Archive, ArchiveLimits}, compact};
use knife_perf::{Gauge, Metric, Scope};
#[derive(Clone, Copy)]
pub struct IndexedGxtMetadata { pub layout:u32, pub format:u32, pub payload_bytes:usize }
pub mod image_worker { pub mod resource_state {
    pub enum Phase { Plan, ReadInflate, Commit, Idle, Stopped }
    pub fn source(_:Phase,_:u64,_:usize,_:u64) {}
} }
#[path="@ROOT@/knife/knife-render/src/byte_view.rs"] mod byte_view;
pub use byte_view::ByteView;
#[path="@ROOT@/knife/knife/src/atlas_cache.rs"] mod atlas_cache;
#[path="@ROOT@/knife/knife/src/vita_atlas_index.rs"] mod index;
@STAGE@
struct Assets;
impl Assets { @COHORT@ }
struct TrackingAllocator;
static TRACK:AtomicBool=AtomicBool::new(false);
static LARGEST:AtomicUsize=AtomicUsize::new(0);
unsafe impl std::alloc::GlobalAlloc for TrackingAllocator {
    unsafe fn alloc(&self, l:std::alloc::Layout)->*mut u8 {
        if TRACK.load(Ordering::Relaxed) { LARGEST.fetch_max(l.size(), Ordering::Relaxed); }
        unsafe { std::alloc::System.alloc(l) }
    }
    unsafe fn dealloc(&self,p:*mut u8,l:std::alloc::Layout) { unsafe {std::alloc::System.dealloc(p,l)} }
    unsafe fn realloc(&self,p:*mut u8,l:std::alloc::Layout,n:usize)->*mut u8 {
        if TRACK.load(Ordering::Relaxed) { LARGEST.fetch_max(n, Ordering::Relaxed); }
        unsafe {std::alloc::System.realloc(p,l,n)}
    }
}
#[global_allocator] static ALLOCATOR:TrackingAllocator=TrackingAllocator;
fn archive()->Arc<Archive> {
    Arc::new(Archive::open_file_with_limits(&std::path::PathBuf::from(
        std::env::var_os("KNIFE_TEST_UI_ASSETS").expect("real assets required")),"allui",ArchiveLimits::VITA).unwrap())
}
#[test]
fn all_real_extra_ui_members_use_bounded_file_reads_without_inflation() {
    let archive=archive(); let stage=Arc::new(ByteStage::default());
    let before=knife_core::io_metrics::snapshot();
    let mut count=0; let mut expected_bytes=0; let mut largest_member=0; let mut atlases=0;
    TRACK.store(true,Ordering::SeqCst);
    for atlas in index::ATLASES.iter().filter(|a| a.direct) {
        assert!(index::find(index::MANIFEST,atlas.index,&archive.entries()[atlas.index]).is_some());
        atlases+=1;
        for member in atlas.members {
            let key=byte_stage_key(&archive.name,atlas.index,member.offset,member.len);
            let bytes=stage.load_direct_bounded(key,archive.clone(),atlas.index,member.offset,member.len).unwrap();
            let actual=compact::parse(&bytes).unwrap();
            assert_eq!(actual.logical,member.logical); assert_eq!(actual.size,member.physical);
            assert_eq!(actual.encoding as u32,member.encoding); assert_eq!(bytes.len(),member.len);
            largest_member=largest_member.max(member.len); expected_bytes+=member.len;count+=1;
            assert_eq!(stage.bytes.load(Ordering::Relaxed),0);
        }
    }
    TRACK.store(false,Ordering::SeqCst);
    let after=knife_core::io_metrics::snapshot();
    assert_eq!(atlases,4); assert_eq!(count,675);
    assert_eq!(after.inflate_calls,before.inflate_calls);
    let card_bytes=after.card_read_bytes-before.card_read_bytes;
    // One small envelope read plus one selected member, never the entire KATL.
    assert!(card_bytes<=(expected_bytes+count*64) as u64);
    assert!(LARGEST.load(Ordering::Relaxed)<=largest_member+4096);
    println!("UI_VERIFY atlases={atlases} members={count} member_bytes={expected_bytes} card_bytes={card_bytes} inflations=0 max_member={largest_member} max_allocation={}",LARGEST.load(Ordering::Relaxed));
}
#[test]
fn raw_only_worker_returns_the_actual_error_and_can_retry() {
    let archive=archive(); let stage=Arc::new(ByteStage::default());
    let atlas=index::ATLASES.iter().find(|a| !a.direct).unwrap();
    let error=stage.load_direct_bounded(1,archive.clone(),atlas.index,0,4).unwrap_err().to_string();
    assert!(error.contains("still NX compressed"),"{error}");
    assert!(error.contains("offset=0 len=4 direct=true"),"{error}");
    let raw=index::ATLASES.iter().find(|a| a.direct).unwrap();
    assert_eq!(&*stage.load_direct_bounded(1,archive.clone(),raw.index,0,4).unwrap(),b"KATL");
    let error=stage.load_direct_bounded(2,archive.clone(),raw.index,usize::MAX,4).unwrap_err().to_string();
    assert!(error.contains("overflow"),"{error}");
}
#[test]
fn page_cache_teardown_keeps_live_views_and_releases_idle_owners() {
    let archive=archive();let cache=atlas_cache::AtlasCache::default();
    let atlas=index::ATLASES.iter().find(|a| a.direct).unwrap();
    let owner=ByteView::owned(vec![42;4096]); let pin=owner.range(0,64).unwrap();
    assert!(cache.seed(&archive,atlas.index,owner));
    assert_eq!(cache.discard_idle(&archive,atlas.index),0);
    assert_eq!(&*pin,&[42;64]);drop(pin);
    let started=std::time::Instant::now();
    loop { if cache.discard_idle(&archive,atlas.index)==4096 {break;}
        assert!(started.elapsed()<std::time::Duration::from_secs(1));std::thread::yield_now(); }
    assert_eq!(cache.discard_idle(&archive,atlas.index),0);
}
#[test]
fn gallery_cohort_never_requires_the_whole_cg_atlas() {
    assert_eq!(Assets::vita_extra_source_names(38),&["EXTRA_PARTS"]);
    assert_eq!(Assets::vita_extra_source_names(39),&["SOUND_PARTS"]);
    assert_eq!(Assets::vita_extra_source_names(40),&["ARCHIVE_PARTS","ARCHTHUMB_PARTS"]);
}
'''


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--assets', required=True, type=Path)
    parser.add_argument('--output', required=True, type=Path)
    args = parser.parse_args()
    run(args.assets, args.output)
