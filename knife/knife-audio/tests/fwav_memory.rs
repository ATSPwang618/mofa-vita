//! Isolated allocator regression: long effects must not need full-size channel
//! scratch allocations, and failure to allocate PCM must return an error.
use std::{alloc::{GlobalAlloc, Layout, System}, sync::atomic::{AtomicUsize, Ordering}};
use knife_audio::{bfsar::decode_fwav, codec::decode_dsp};

struct MeasuredAllocator;
static TRACK_FROM: AtomicUsize = AtomicUsize::new(0);
static REQUESTED: AtomicUsize = AtomicUsize::new(0);
static DENY_FROM: AtomicUsize = AtomicUsize::new(0);
fn admitted(bytes: usize) -> bool {
    let threshold = TRACK_FROM.load(Ordering::Relaxed);
    if threshold != 0 && bytes >= threshold { REQUESTED.fetch_add(bytes, Ordering::Relaxed); }
    let deny = DENY_FROM.load(Ordering::Relaxed);
    deny == 0 || bytes < deny
}
unsafe impl GlobalAlloc for MeasuredAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if admitted(layout.size()) { unsafe { System.alloc(layout) } } else { std::ptr::null_mut() }
    }
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        if admitted(layout.size()) { unsafe { System.alloc_zeroed(layout) } } else { std::ptr::null_mut() }
    }
    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, bytes: usize) -> *mut u8 {
        if admitted(bytes) { unsafe { System.realloc(pointer, layout, bytes) } } else { std::ptr::null_mut() }
    }
    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) { unsafe { System.dealloc(pointer, layout) } }
}
#[global_allocator]
static ALLOCATOR: MeasuredAllocator = MeasuredAllocator;

fn put16(bytes: &mut [u8], at: usize, value: u16) { bytes[at..at + 2].copy_from_slice(&value.to_le_bytes()); }
fn put32(bytes: &mut [u8], at: usize, value: usize) { bytes[at..at + 4].copy_from_slice(&(value as u32).to_le_bytes()); }

fn fixture(encoding: u8, channels: usize, samples: usize) -> (Vec<u8>, Vec<[f32; 2]>) {
    let channel_bytes = match encoding { 0 => samples, 1 => samples * 2, _ => samples.div_ceil(14) * 8 };
    let table = 0x40 + 8 + 20;
    let descriptors = table + 4 + channels * 8;
    let data = descriptors + channels * 56;
    let mut bytes = vec![0; data + 8 + channel_bytes * channels];
    bytes[..4].copy_from_slice(b"FWAV");
    put16(&mut bytes, 0x10, 2);
    put16(&mut bytes, 0x14, 0x7000); put32(&mut bytes, 0x18, 0x40); put32(&mut bytes, 0x1c, data - 0x40);
    put16(&mut bytes, 0x20, 0x7001); put32(&mut bytes, 0x24, data); put32(&mut bytes, 0x28, 8 + channel_bytes * channels);
    bytes[0x40..0x44].copy_from_slice(b"INFO");
    bytes[data..data + 4].copy_from_slice(b"DATA");
    let body = 0x48;
    bytes[body] = encoding; bytes[body + 1] = 1;
    put32(&mut bytes, body + 4, 48_000); put32(&mut bytes, body + 8, 17); put32(&mut bytes, body + 12, samples);
    put32(&mut bytes, table, channels);
    let mut expected = Vec::new();
    for channel in 0..channels {
        let descriptor = descriptors + channel * 56;
        put16(&mut bytes, table + 4 + channel * 8, 1);
        put32(&mut bytes, table + 8 + channel * 8, descriptor - table);
        put32(&mut bytes, descriptor + 4, channel * channel_bytes);
        put32(&mut bytes, descriptor + 12, 16);
        let coefs = [1536i16, 256, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        for (i, value) in coefs.iter().enumerate() { put16(&mut bytes, descriptor + 16 + i * 2, *value as u16); }
        put16(&mut bytes, descriptor + 50, 30); put16(&mut bytes, descriptor + 52, (-20i16) as u16);
        let payload = &mut bytes[data + 8 + channel * channel_bytes..data + 8 + (channel + 1) * channel_bytes];
        let decoded = match encoding {
            0 => { for (i, x) in payload.iter_mut().enumerate() { *x = ((i + channel * 7) as u8).wrapping_mul(37); }
                payload.iter().map(|x| *x as i8 as f32 / 128.0).collect::<Vec<_>>() },
            1 => { for (i, x) in payload.chunks_exact_mut(2).enumerate() { x.copy_from_slice(&((i as i16).wrapping_mul(7919)).to_le_bytes()); }
                payload.chunks_exact(2).map(|x| i16::from_le_bytes([x[0], x[1]]) as f32 / 32768.0).collect() },
            _ => { for (i, x) in payload.iter_mut().enumerate() { *x = if i % 8 == 0 { 2 } else { (i as u8).wrapping_mul(17) }; }
                decode_dsp(payload, samples, &coefs, (30, -20)) },
        };
        expected.push(decoded);
    }
    let frames = (0..samples).map(|i| if channels == 1 {
        [expected[0][i] * std::f32::consts::FRAC_1_SQRT_2; 2]
    } else { [expected[0][i], expected[1][i]] }).collect();
    (bytes, frames)
}

#[test]
fn fwav_uses_one_pcm_allocation_preserves_samples_and_reports_allocation_failure() {
    let samples = 4097;
    for encoding in [0, 1, 2] {
        for channels in [1, 2] {
            let (bytes, expected) = fixture(encoding, channels, samples);
            REQUESTED.store(0, Ordering::Relaxed);
            TRACK_FROM.store(samples * 4, Ordering::Relaxed);
            let decoded = decode_fwav(&bytes);
            TRACK_FROM.store(0, Ordering::Relaxed);
            let pcm = decoded.unwrap();
            assert_eq!(pcm.frames, expected, "encoding {encoding}, channels {channels}");
            assert_eq!(pcm.loop_range, Some((17, samples)));
            assert_eq!(pcm.sample_rate, 48_000);
            assert_eq!(pcm.mono, channels == 1);
            assert_eq!(REQUESTED.load(Ordering::Relaxed), samples * 8,
                "FWAV must allocate only the final interleaved PCM, not full channel scratch");
        }
    }
    let (bytes, _) = fixture(2, 2, samples);
    DENY_FROM.store(samples * 8, Ordering::Relaxed);
    let decoded = decode_fwav(&bytes);
    DENY_FROM.store(0, Ordering::Relaxed);
    assert!(decoded.unwrap_err().chain().any(|error| error.is::<std::collections::TryReserveError>()),
        "PCM allocation failure must stay recoverable instead of aborting the Vita process");
}
