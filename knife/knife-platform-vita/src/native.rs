use std::{ffi::CString, fs::File, io::Write, ptr::NonNull, time::Duration};
use crate::{AbiLayout, IoRequest, data_path};

unsafe extern "C" {
    fn kv_abi_layout(layout: *mut AbiLayout);
    fn kv_alloc(length: u32) -> *mut u8;
    fn kv_free(buffer: *mut u8);
    fn kv_init_data_dir() -> i32;
    fn kv_read_range(path: *const std::ffi::c_char, request: *mut IoRequest) -> i32;
    fn kv_clock_us() -> u64;
    fn kv_runtime_memory(report: *mut RuntimeMemory);
    fn kv_join_worker(input: u32, result: *mut i32) -> i32;
    fn kv_probe_memory_round_trip(pool: u32, report: *mut MemoryRoundTrip) -> i32;
}

#[repr(C)]
#[derive(Debug, Default)]
struct MemoryRoundTrip {
    pool: u32,
    bytes: u32,
    status: [i32; 5],
    before: [u32; 3],
    during: [u32; 3],
    after: [u32; 3],
}
const _: () = assert!(std::mem::size_of::<MemoryRoundTrip>() == 64);

#[repr(C)]
#[derive(Debug, Default, Clone, Copy)]
pub struct RuntimeMemory {
    pub newlib_heap_bytes: u32,
    pub libc_heap_bytes: u32,
    pub main_stack_bytes: u32,
    pub main_priority: u32,
}

/// Heap and main-thread stack sizes the loader actually saw, read from the same
/// symbols it consumes. A zero value means the constant was not linked.
pub fn runtime_memory() -> RuntimeMemory {
    let mut report = RuntimeMemory::default();
    // SAFETY: the C side writes exactly this repr(C) struct.
    unsafe { kv_runtime_memory(&mut report) };
    report
}

/// A C allocation never enters Rust's allocator. It cannot escape the owner.
struct NativeBuffer { pointer: NonNull<u8>, length: u32 }
impl NativeBuffer {
    fn new(length: u32) -> Result<Self, String> {
        if length == 0 { return Err("zero native allocation".into()); }
        // SAFETY: SDK malloc accepts this length; the unique pointer is freed in Drop.
        let pointer = NonNull::new(unsafe { kv_alloc(length) }).ok_or("native allocation failed")?;
        Ok(Self { pointer, length })
    }
    fn bytes(&self) -> &[u8] {
        // SAFETY: the entire allocation is initialized before reading in the probe.
        unsafe { std::slice::from_raw_parts(self.pointer.as_ptr(), self.length as usize) }
    }
}
impl Drop for NativeBuffer {
    fn drop(&mut self) {
        // SAFETY: kv_alloc owns the allocation, and this is its only owner.
        unsafe { kv_free(self.pointer.as_ptr()) };
    }
}

fn sdk_ok(result: i32, operation: &str) -> Result<(), String> {
    if result < 0 { Err(format!("{operation}: SDK error {result:#010x}")) } else { Ok(()) }
}

pub(crate) fn init_data_dir() -> Result<(), String> {
    // SAFETY: this SDK wrapper has no pointer arguments.
    sdk_ok(unsafe { kv_init_data_dir() }, "create data directory")
}

/// Runtime evidence is written in a fresh, bounded log; a compile is not a pass.
pub fn run_platform_probe() -> Result<(), String> {
    // SAFETY: no pointers or borrowed arguments cross this SDK wrapper.
    sdk_ok(unsafe { kv_init_data_dir() }, "create data directory")?;
    let path = data_path("platform-probe.log")?;
    let mut log = File::create(path).map_err(|e| e.to_string())?;
    let result = probe_checks(&mut log);
    writeln!(log, "RESULT {}", if result.is_ok() { "PASS" } else { "FAIL" }).map_err(|e| e.to_string())?;
    if let Err(ref error) = result { writeln!(log, "{error}").map_err(|e| e.to_string())?; }
    log.sync_all().map_err(|e| e.to_string())?;
    result
}

fn probe_checks(log: &mut File) -> Result<(), String> {
    macro_rules! record {
        ($($arg:tt)*) => { writeln!(log, $($arg)*).map_err(|e| e.to_string())?; log.flush().map_err(|e| e.to_string())?; };
    }
    record!("knife Vita DEV platform probe v2; process pointer width {}", usize::BITS);
    let mut native = AbiLayout::default();
    // SAFETY: a correctly aligned, fully allocated report is writable for this call.
    unsafe { kv_abi_layout(&mut native) };
    AbiLayout::rust().validate(&native)?;
    if native.pointer_bits != 32 { return Err("runtime is not the 32-bit Vita target".into()); }
    record!("PASS Rust/C ABI {native:?}");

    let buffer = NativeBuffer::new(64)?;
    // SAFETY: pointer is uniquely owned and length is the allocated size.
    unsafe { std::ptr::write_bytes(buffer.pointer.as_ptr(), 0xa5, buffer.length as usize) };
    if buffer.bytes().iter().any(|&v| v != 0xa5) { return Err("C heap round trip mismatch".into()); }
    record!("PASS C allocation / Rust write / C release ownership");

    let fixture = data_path("platform-probe.io")?;
    {
        let mut file = File::create(&fixture).map_err(|e| e.to_string())?;
        file.write_all(b"prefix--seek/read-range--suffix").map_err(|e| e.to_string())?;
        file.sync_all().map_err(|e| e.to_string())?;
    }
    let c_path = CString::new(fixture).map_err(|e| e.to_string())?;
    let mut request = IoRequest { offset: 8, buffer: buffer.pointer.as_ptr(), length: 15, actual: 0, status: 0 };
    // SAFETY: C path and buffer outlive the synchronous call; length fits allocation.
    sdk_ok(unsafe { kv_read_range(c_path.as_ptr(), &mut request) }, "file range read")?;
    if request.actual != 15 || &buffer.bytes()[..15] != b"seek/read-range" {
        return Err(format!("file range mismatch: actual {}", request.actual));
    }
    record!("PASS Rust file write + SDK 64-bit seek/read at offset 8");

    // SAFETY: this wrapper has no arguments and returns the process monotonic clock.
    let start = unsafe { kv_clock_us() };
    let rust_start = std::time::Instant::now();
    std::thread::sleep(Duration::from_millis(5));
    let end = unsafe { kv_clock_us() };
    if end <= start || rust_start.elapsed().is_zero() { return Err("monotonic clock did not advance".into()); }
    record!("PASS SDK/Rust monotonic clock: {} us", end - start);
    let value = std::thread::spawn(|| (123_u32 * 17) ^ 0x4567).join().map_err(|_| "Rust worker panicked")?;
    if value != ((123 * 17) ^ 0x4567) { return Err("Rust join result mismatch".into()); }
    let mut result = -1;
    // SAFETY: synchronous join writes an initialized i32 result before returning.
    sdk_ok(unsafe { kv_join_worker(123, &mut result) }, "SDK worker join and exit-status query")?;
    if result != (123 ^ 0x5a5a) { return Err(format!("SDK join result mismatch: {result}")); }
    record!("PASS Rust joined worker and SDK joined worker");

    // Consecutive SDK queries stay inside C, without intervening Rust allocations
    // or log writes. Check all pools and repeat to expose cumulative drift.
    let mut memory_failures = Vec::new();
    for (pool, name) in ["USER_RW", "CDRAM", "PHYCONT"].iter().enumerate() {
        for round in 1..=2 {
            let mut report = MemoryRoundTrip::default();
            // SAFETY: C writes a fixed-size report, owns and releases its memblock,
            // and retains no pointer into this stack value after returning.
            let status = unsafe { kv_probe_memory_round_trip(pool as u32, &mut report) };
            record!("MEMORY pool={name} round={round} {report:?} status={status}");
            let allocated = i64::from(report.before[pool]) - i64::from(report.during[pool]);
            let reclaimed = i64::from(report.after[pool]) - i64::from(report.during[pool]);
            let other_pools_unchanged = (0..3).filter(|&i| i != pool)
                .all(|i| report.before[i] == report.during[i] && report.before[i] == report.after[i]);
            if status < 0 || allocated != i64::from(report.bytes)
                || reclaimed != i64::from(report.bytes) || report.before != report.after
                || !other_pools_unchanged
            {
                memory_failures.push(format!("{name} round {round}: allocated {allocated}, reclaimed {reclaimed}, expected {} bytes; SDK {status}", report.bytes));
            }
        }
    }
    if !memory_failures.is_empty() {
        return Err(format!("SDK memory allocation/reclamation mismatch: {}", memory_failures.join("; ")));
    }
    record!("PASS SDK USER_RW/CDRAM/PHYCONT allocation and complete reclamation, two rounds each");
    Ok(())
}
