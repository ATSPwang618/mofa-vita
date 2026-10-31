//! Best-effort, lock-free resource progress for the independent watchdog.
//! Phases are parent scopes: `ReadInflate` includes the archive file operation
//! and NX inflation; it must not be reported as a measured file-lock wait.
use std::sync::{OnceLock, atomic::{AtomicU32, AtomicU64, Ordering}};
use std::time::Instant;

#[repr(u32)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase { Idle=0, Plan=1, ReadInflate=2, Commit=3, Decode=4, Stopped=5,
    DemandSource=6, DemandDecode=7, DemandRegister=8, DemandFailed=9 }
impl Phase {
    fn from_u32(value:u32)->Self {match value {1=>Self::Plan,2=>Self::ReadInflate,3=>Self::Commit,
        4=>Self::Decode,5=>Self::Stopped,6=>Self::DemandSource,7=>Self::DemandDecode,
        8=>Self::DemandRegister,9=>Self::DemandFailed,_=>Self::Idle}}
    pub fn name(self)->&'static str {match self {Self::Idle=>"idle",Self::Plan=>"plan",
        Self::ReadInflate=>"read_inflate",Self::Commit=>"commit",Self::Decode=>"decode",
        Self::Stopped=>"stopped",Self::DemandSource=>"demand_source",Self::DemandDecode=>"demand_decode",
        Self::DemandRegister=>"demand_register",Self::DemandFailed=>"demand_failed"}}
}
#[derive(Clone, Copy, Debug)]
pub struct WorkerSnapshot {pub phase:Phase,pub key:u64,pub archive:u64,pub index:u32,
    pub generation:u64,pub age_ms:u64}
#[derive(Clone, Copy, Debug)]
pub struct Snapshot {pub source:WorkerSnapshot,pub image:WorkerSnapshot,pub demand:WorkerSnapshot}
struct Slot {phase:AtomicU32,key:AtomicU64,archive:AtomicU64,index:AtomicU32,
    generation:AtomicU64,started:AtomicU64}
impl Slot {
    const fn new()->Self {Self {phase:AtomicU32::new(0),key:AtomicU64::new(0),archive:AtomicU64::new(0),
        index:AtomicU32::new(u32::MAX),generation:AtomicU64::new(0),started:AtomicU64::new(0)}}
    fn set(&self,phase:Phase,key:u64,archive:u64,index:usize,generation:u64) {
        self.key.store(key,Ordering::Relaxed);self.archive.store(archive,Ordering::Relaxed);
        self.index.store(u32::try_from(index).unwrap_or(u32::MAX),Ordering::Relaxed);
        self.generation.store(generation,Ordering::Relaxed);self.started.store(now_ms(),Ordering::Relaxed);
        self.phase.store(phase as u32,Ordering::Release);
    }
    fn read(&self,now:u64)->WorkerSnapshot {let phase=Phase::from_u32(self.phase.load(Ordering::Acquire));
        WorkerSnapshot {phase,key:self.key.load(Ordering::Relaxed),archive:self.archive.load(Ordering::Relaxed),
            index:self.index.load(Ordering::Relaxed),generation:self.generation.load(Ordering::Relaxed),
            age_ms:if phase==Phase::Idle {0}else{now.saturating_sub(self.started.load(Ordering::Relaxed))}}}
}
static SOURCE:Slot=Slot::new();static IMAGE:Slot=Slot::new();static DEMAND:Slot=Slot::new();
fn now_ms()->u64 {static START:OnceLock<Instant>=OnceLock::new();START.get_or_init(Instant::now).elapsed().as_millis() as u64}
pub fn key(name:&str)->u64 {name.bytes().fold(0xcbf29ce484222325,|v,b|(v^u64::from(b)).wrapping_mul(0x100000001b3))}
pub fn source(phase:Phase,archive:u64,index:usize,generation:u64) {SOURCE.set(phase,archive.rotate_left(17)^index as u64,archive,index,generation);}
pub fn image(phase:Phase,name:&str,archive:u64,index:usize,generation:u64) {IMAGE.set(phase,key(name),archive,index,generation);}
pub fn image_location(archive:u64,index:usize) {IMAGE.archive.store(archive,Ordering::Relaxed);IMAGE.index.store(index as u32,Ordering::Relaxed);}
pub fn image_phase(phase:Phase) {IMAGE.started.store(now_ms(),Ordering::Relaxed);IMAGE.phase.store(phase as u32,Ordering::Release);}
pub fn demand(phase:Phase,name:&str,archive:u64,index:usize,generation:u64) {
    // Repeated VM polls must preserve the age of the unchanged wait.
    let key=key(name);
    if DEMAND.phase.load(Ordering::Relaxed)==phase as u32 && DEMAND.key.load(Ordering::Relaxed)==key
        && DEMAND.generation.load(Ordering::Relaxed)==generation {return;}
    DEMAND.set(phase,key,archive,index,generation);
}
pub fn snapshot()->Snapshot {let now=now_ms();Snapshot {source:SOURCE.read(now),image:IMAGE.read(now),demand:DEMAND.read(now)}}
