//! Bounded immutable, fully graded CPU results. GPU residency can disappear
//! without forcing another codec/grade pass for the same source and full Bake.
use std::{collections::VecDeque, sync::{Arc, Condvar, Mutex, OnceLock, TryLockError}, time::{Duration,Instant}};
use anyhow::{Result, Context};
use knife_core::format::archive::Archive;
use knife_render::PixelView;
use knife_perf::{Metric, Gauge};
use crate::{assets::Pixels, grade::{Bake, GradeKey}};
const CAP: usize = knife_perf::memory_budget::CPU_GRADED_BYTES;
#[derive(Clone)]
pub struct ResultPixels { pub width:u32,pub height:u32,pub logical:Option<[u32;2]>,pub pixels:PixelView,pub rebuild_us:u64,pub archive:Arc<Archive>,pub index:usize,pub encoded_len:usize }
impl ResultPixels {
    pub fn into_pixels(self)->Pixels {
        let px=Pixels::SharedBaked(self.width,self.height,self.pixels,self.rebuild_us);
        if let Some(logical)=self.logical {Pixels::Scaled(Box::new(px),logical)}else{px}
    }
}
#[derive(Clone,PartialEq,Eq)]
struct Key { archive:usize,index:usize,name:String,strip:Option<(usize,usize)>,grade:GradeKey }
struct Entry { key:Key, _archive:Arc<Archive>, result:ResultPixels }
struct Active { key:Key, reserved:usize }
#[derive(Default)]
struct State { entries:VecDeque<Entry>,active:Vec<Active>,bytes:usize }
#[derive(Default)]
struct Cache { state:Mutex<State>, wake:Condvar }
fn cache()->&'static Cache {static CACHE:OnceLock<Cache>=OnceLock::new();CACHE.get_or_init(Cache::default)}
/// Oldest unpinned complete results only; active reservations stay charged.
/// Maintenance never waits for a worker to release this metadata lock.
pub fn trim_lru(need:usize)->usize {
    if need==0 {return 0;}
    let mut state=match cache().state.try_lock() {Ok(s)=>s,Err(TryLockError::Poisoned(e))=>e.into_inner(),Err(_)=>return 0};
    let before=state.bytes;let mut i=0;
    while i<state.entries.len() && before-state.bytes<need {
        if state.entries[i].result.pixels.is_unpinned() {
            let old=state.entries.remove(i).unwrap();state.bytes-=old.result.pixels.allocation_len();
            knife_perf::add(Metric::GradeResultEvict,old.result.pixels.allocation_len() as u64,old.result.rebuild_us);
        }else{i+=1;}
    }
    let reserved:usize=state.active.iter().map(|a|a.reserved).sum();
    knife_perf::gauge_set(Gauge::GradeResultBytes,(state.bytes+reserved)as u64);before-state.bytes
}
fn enabled(grade:Bake)->bool { grade.cpu() && knife_perf::optimizations::enabled(knife_perf::optimizations::GRADE_CACHE) }
fn key(archive:&Arc<Archive>,index:usize,name:&str,bake:Bake,strip:Option<(usize,usize)>,srgb:bool)->Key {
    Key { archive:Arc::as_ptr(archive) as usize,index,name:name.to_string(),strip,grade:GradeKey::new(&bake,srgb) }
}
fn take_hit(state:&mut State,key:&Key)->Option<ResultPixels> {
    let i=state.entries.iter().position(|entry|entry.key==*key)?;
    let entry=state.entries.remove(i).unwrap();let result=entry.result.clone();state.entries.push_back(entry);
    knife_perf::add(Metric::GradeResultHit,result.pixels.len() as u64, result.rebuild_us);
    Some(result)
}
/// Before archive I/O on a new logical image demand. The retained archive Arc
/// prevents identity reuse. The source is immutable for the Assets lifetime.
pub fn lookup_source(archive:&Arc<Archive>,index:usize,name:&str,bake:Bake,strip:Option<(usize,usize)>)->Option<ResultPixels> {
    if !enabled(bake) {return None;}
    let mut state=cache().state.lock().unwrap_or_else(|e|e.into_inner());
    source_hit(&mut state,archive,index,name,bake,strip)
}
pub enum SourceLookup { Ready(ResultPixels), Missing, Pending }
/// The VM owner may reuse a completed result without waiting on cache metadata
/// owned by a decoder/maintenance sweep. A busy lock is Pending, not a cache miss
/// that would unnecessarily enqueue archive I/O. No reservation is disturbed.
pub fn try_lookup_source(archive:&Arc<Archive>,index:usize,name:&str,bake:Bake,
    strip:Option<(usize,usize)>)->SourceLookup {
    if !enabled(bake) {return SourceLookup::Missing;}
    let mut state=match cache().state.try_lock() {
        Ok(state)=>state,Err(TryLockError::Poisoned(error))=>error.into_inner(),Err(_)=>return SourceLookup::Pending,
    };
    source_hit(&mut state,archive,index,name,bake,strip).map_or(SourceLookup::Missing,SourceLookup::Ready)
}
fn source_hit(state:&mut State,archive:&Arc<Archive>,index:usize,name:&str,bake:Bake,
    strip:Option<(usize,usize)>)->Option<ResultPixels> {
    let hit=state.entries.iter().find(|entry|entry.key.name==name && entry.key.strip==strip
        && entry.key.index==index && Arc::as_ptr(archive) as usize==entry.key.archive
        && entry.key.grade==GradeKey::new(&bake,entry.key.grade.srgb)).map(|entry|entry.key.clone());
    hit.and_then(|key|take_hit(state,&key))
}
fn result_from(pixels:Pixels,rebuild_us:u64,archive:Arc<Archive>,index:usize,encoded_len:usize)->Result<ResultPixels> {
    let (pixels,logical)=match pixels {Pixels::Scaled(p,l)=>(*p,Some(l)),p=>(p,None)};
    let (width,height,pixels)=match pixels {
        Pixels::Native {width,height,owner,range,baked:true,..}=>{
            let allocation=owner.byte_len() as usize;
            let view=PixelView::owned(owner.freeze(),allocation).range(range.start,range.len()).context("graded native range")?;
            (width,height,view)
        },
        Pixels::Baked(w,h,bytes)=>{let charge=bytes.capacity();(w,h,PixelView::owned(bytes,charge))},
        _=>anyhow::bail!("graded cache requires fully baked immutable pixels"),
    };
    Ok(ResultPixels {width,height,logical,pixels,rebuild_us,archive,index,encoded_len})
}
/// `need` includes the entire C owner before strip views, not just visible rows.
/// Duplicate workers join the exact key. Unrelated keys may decode concurrently
/// only after their combined reserved+retained charge fits this cache's cap.
pub fn build(archive:Arc<Archive>,index:usize,name:&str,bake:Bake,strip:Option<(usize,usize)>,srgb:bool,
    need:usize, encoded_len:usize, decode:impl FnOnce()->Result<Pixels>)->Result<Pixels> {
    if !enabled(bake) || need>CAP {return decode();}
    let key=key(&archive,index,name,bake,strip,srgb);
    let cache=cache();let deadline=Instant::now()+Duration::from_secs(10);
    let mut state=loop {match cache.state.try_lock() {
        Ok(state)=>break state,Err(TryLockError::Poisoned(error))=>break error.into_inner(),Err(_)=>{
            anyhow::ensure!(Instant::now()<deadline,"grade cache mutex deadline (10s): {name}; active owner retained");
            std::thread::sleep(Duration::from_millis(1));
        }
    }};
    loop {
        if let Some(hit)=take_hit(&mut state,&key){return Ok(hit.into_pixels());}
        if !state.active.iter().any(|active|active.key==key){break;}
        let now=Instant::now();
        anyhow::ensure!(now<deadline,"grade key wait deadline (10s): {name}; active decode retained");
        let (next,_)=cache.wake.wait_timeout(state,deadline.saturating_duration_since(now))
            .unwrap_or_else(|error|error.into_inner());
        state=next;
    }
    let reserved:usize=state.active.iter().map(|a|a.reserved).sum();
    while state.bytes.saturating_add(reserved).saturating_add(need)>CAP {
        let Some(i)=state.entries.iter().position(|e|e.result.pixels.is_unpinned()) else {
            // Preserve ordinary bounded decode behavior if callers pin the full
            // optional cache. Never evict a live owner or silently exceed its cap.
            drop(state);return decode();
        };
        let old=state.entries.remove(i).unwrap();state.bytes-=old.result.pixels.allocation_len();
        knife_perf::add(Metric::GradeResultEvict,old.result.pixels.allocation_len() as u64,old.result.rebuild_us);
    }
    knife_perf::add(Metric::GradeResultMiss,need as u64,0);
    state.active.push(Active{key:key.clone(),reserved:need});
    knife_perf::gauge_set(Gauge::GradeResultBytes,(state.bytes+reserved+need) as u64);
    drop(state);
    let started=std::time::Instant::now();
    let decoded=std::panic::catch_unwind(std::panic::AssertUnwindSafe(decode));
    let rebuilt=started.elapsed().as_micros().min(u64::MAX as u128) as u64;
    let result=match decoded {Ok(r)=>r.and_then(|p|result_from(p,rebuilt,archive.clone(),index,encoded_len)),Err(error)=>{
        let mut state=cache.state.lock().unwrap_or_else(|e|e.into_inner());state.active.retain(|a|a.key!=key);
        let reserved:usize=state.active.iter().map(|a|a.reserved).sum();
        knife_perf::gauge_set(Gauge::GradeResultBytes,(state.bytes+reserved) as u64);
        cache.wake.notify_all();drop(state);std::panic::resume_unwind(error)
    }};
    let mut state=cache.state.lock().unwrap_or_else(|e|e.into_inner());
    state.active.retain(|a|a.key!=key);
    let reserved:usize=state.active.iter().map(|a|a.reserved).sum();
    if let Ok(result)=&result {
        if result.pixels.allocation_len()<=need && state.bytes+reserved+result.pixels.allocation_len()<=CAP {
            state.bytes+=result.pixels.allocation_len();state.entries.push_back(Entry{key,_archive:archive,result:result.clone()});
        }
    }
    knife_perf::gauge_set(Gauge::GradeResultBytes,(state.bytes+reserved) as u64);
    cache.wake.notify_all();result.map(ResultPixels::into_pixels)
}
