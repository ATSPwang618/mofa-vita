//! Exact reusable pre-alter kernels. Encoder boundaries are built and checked on
//! the running target's original srgb_byte implementation, never shipped from host libm.
use super::*;
use std::{collections::VecDeque, sync::{Arc, Mutex, OnceLock, TryLockError}};
use knife_perf::{Metric, Scope, Gauge};
const CAP: usize = 4 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct GradeKey {
    pub build: u8, pub mode: i32, pub bright: u32, pub contrast: u32,
    pub raw: bool, pub srgb: bool, pub native_encoded: bool, pub row: Option<[u32; 20]>,
}
impl GradeKey {
    pub fn new(bake: &Bake, srgb: bool) -> Self {
        Self::in_color_domain(bake, srgb, native_encoded_color())
    }
    fn in_color_domain(bake: &Bake, srgb: bool, native_encoded: bool) -> Self {
        let build = if crate::assets::witch() { 2 } else if crate::assets::overseas() { 1 } else { 0 };
        let table = if build == 2 { &WITCH_TABLE } else { &TABLE };
        Self { build, mode: bake.grade, bright: bake.bright.to_bits(), contrast: bake.contrast.to_bits(),
            // Native UNORM grades consume encoded bytes directly. Container
            // sRGB metadata never changes these pixels and must not split an
            // otherwise identical graded-result/kernel key.
            raw: bake.raw, srgb: srgb && !native_encoded, native_encoded,
            row: usize::try_from(bake.grade).ok().and_then(|i| table.get(i)).map(|r| r.map(f32::to_bits)) }
    }
    fn label(&self) -> String {
        let mut table = 0xcbf29ce484222325u64;
        for word in self.row.iter().flatten() { table = (table ^ u64::from(*word)).wrapping_mul(0x100000001b3); }
        format!("b{}:g{}:r{}:s{}:n{}:br{:08x}:ct{:08x}:t{:016x}", self.build, self.mode,
            u8::from(self.raw), u8::from(self.srgb), u8::from(self.native_encoded), self.bright, self.contrast, table)
    }
}

/// Largest positive float admitted here is 1.0. Every 2^18 consecutive float bit
/// patterns share a tiny bucket, followed by exact comparisons at byte thresholds.
struct Encoder { thresholds: [u32; 256], bucket: [u8; (0x3f800000 >> 18) + 1] }
impl Encoder {
    fn new() -> Option<Self> {
        let mut thresholds = [0u32; 256];
        for byte in 1..=255usize {
            let (mut low, mut high) = (0u32, 1.0f32.to_bits());
            while low < high {
                let mid = low + (high - low) / 2;
                if srgb_byte(f32::from_bits(mid)) as usize >= byte { high = mid; } else { low = mid + 1; }
            }
            thresholds[byte] = low;
        }
        // Fail closed if the target libm/rounding does not meet the transition
        // contract. All neighboring representable values are checked on target.
        for byte in 1..=255usize {
            let at = thresholds[byte];
            if at == 0 || srgb_byte(f32::from_bits(at - 1)) as usize >= byte
                || (srgb_byte(f32::from_bits(at)) as usize) < byte { return None; }
            for bits in at.saturating_sub(4)..=at.saturating_add(4).min(1.0f32.to_bits()) {
                let expected = srgb_byte(f32::from_bits(bits));
                let got = thresholds.partition_point(|t| *t <= bits).saturating_sub(1) as u8;
                if got != expected { return None; }
            }
        }
        let mut bucket = [0u8; (0x3f800000 >> 18) + 1];
        for (i, byte) in bucket.iter_mut().enumerate() {
            let bits = (i as u32) << 18;
            *byte = thresholds.partition_point(|t| *t <= bits).saturating_sub(1) as u8;
        }
        Some(Self { thresholds, bucket })
    }
    fn encode(&self, level: f32) -> u8 {
        // Preserve Rust's old clamped NaN/negative/infinite conversion semantics.
        if level.is_nan() || level <= 0.0 { return 0; }
        if level >= 1.0 { return 255; }
        let bits = level.to_bits();
        let mut byte = self.bucket[(bits >> 18) as usize] as usize;
        while byte < 255 && bits >= self.thresholds[byte + 1] { byte += 1; }
        byte as u8
    }
}
fn encoder() -> Option<&'static Encoder> {
    static ENCODER: OnceLock<Option<Encoder>> = OnceLock::new();
    ENCODER.get_or_init(Encoder::new).as_ref()
}

/// Brightness and contrast run strictly after `channels`. Cache those exact
/// f32 channel results, so a new tone does not repeat 65,281 gamma/matrix jobs.
/// Input domain and grade-table bits remain part of the identity. No table is
/// generated on another machine or approximated with interpolation.
#[derive(Clone, PartialEq, Eq)]
struct GrayBaseKey { build: u8, mode: i32, row: Option<[u32; 20]> }
impl From<&GradeKey> for GrayBaseKey {
    fn from(key: &GradeKey) -> Self { Self { build: key.build, mode: key.mode, row: key.row } }
}
struct GrayBase { key: GrayBaseKey, values: Vec<[f32; 3]>, bytes: usize }
#[derive(Default)]
struct GrayBases { entries: VecDeque<Arc<GrayBase>>, bytes: usize }
fn gray_bases() -> &'static Mutex<GrayBases> {
    static BASES: OnceLock<Mutex<GrayBases>> = OnceLock::new();
    BASES.get_or_init(Mutex::default)
}
fn trim_gray_bases(cache: &mut GrayBases, need: usize) -> usize {
    let before = cache.bytes;
    let mut i = 0;
    while i < cache.entries.len() && before - cache.bytes < need {
        if Arc::strong_count(&cache.entries[i]) == 1 {
            cache.bytes -= cache.entries.remove(i).unwrap().bytes;
        } else { i += 1; }
    }
    knife_perf::gauge_set(Gauge::GradeBaseBytes, cache.bytes as u64);
    before - cache.bytes
}
fn gray_base(key: &GradeKey, params: &Params) -> Option<Arc<GrayBase>> {
    // Called only by encoded-input grey kernels. Those share the integer
    // 54R+183G+19B grid, independent of sRGB metadata/raw and the final tone.
    debug_assert!(key.native_encoded && params.grey);
    let key = GrayBaseKey::from(key);
    let mut cache = gray_bases().lock().unwrap_or_else(|e| e.into_inner());
    if let Some(i) = cache.entries.iter().position(|entry| entry.key == key) {
        let entry = cache.entries.remove(i).unwrap();
        cache.entries.push_back(entry.clone());
        knife_perf::add(Metric::GradeBaseHit, 0, 0);
        return Some(entry);
    }
    let needed = std::mem::size_of::<GrayBase>() + (255 * 256 + 1) * std::mem::size_of::<[f32; 3]>();
    let cap = knife_perf::memory_budget::CPU_GRADE_BASE_BYTES;
    let deficit = cache.bytes.saturating_add(needed).saturating_sub(cap);
    trim_gray_bases(&mut cache, deficit);
    if cache.bytes.saturating_add(needed) > cap { return None; }
    let probe = Scope::new(Metric::GradeBaseBuild);
    let values: Vec<_> = (0..=255 * 256).map(|k| channels(params, [k as f32 / 256.0; 3])).collect();
    let bytes = std::mem::size_of::<GrayBase>() + values.capacity() * std::mem::size_of::<[f32; 3]>();
    let entry = Arc::new(GrayBase { key, values, bytes });
    cache.bytes += bytes;
    cache.entries.push_back(entry.clone());
    knife_perf::gauge_set(Gauge::GradeBaseBytes, cache.bytes as u64);
    probe.finish(bytes as u64, 0);
    Some(entry)
}

enum Lut { Gray(Vec<[f32; 3]>), GrayEncoded(Vec<[u8; 3]>), Channels([[u8; 3]; 256]) }
struct Kernel { key: GradeKey, pre: [f32; 256], lut: Lut, bytes: usize }
impl Kernel {
    fn build(key: GradeKey) -> Self {
        let bake = Bake { grade: key.mode, bright: f32::from_bits(key.bright), contrast: f32::from_bits(key.contrast), raw: key.raw };
        let pre = std::array::from_fn(|x| input_level(x, bake.raw, key.srgb, key.native_encoded));
        let p = key.row.map(|row| params_entry(&row.map(f32::from_bits))).filter(|_| key.mode != 0);
        let last = |v| match &p { Some(p) => channels(p, v).map(|x| bake.tone(x)), None => v.map(|x| bake.tone(x)) };
        let lut = if p.as_ref().is_some_and(|p| p.grey) {
            if key.native_encoded {
                // Encoded byte input makes 54R+183G+19B an exact integer. All
                // 65,281 possible values map directly to final UNORM8, avoiding
                // per-pixel floating interpolation and representation encoding.
                let base = gray_base(&key, p.as_ref().unwrap());
                Lut::GrayEncoded(match base {
                    Some(base) => base.values.iter().map(|v| v.map(|x| unorm_byte(bake.tone(x)))).collect(),
                    None => (0..=255 * 256).map(|k| last([k as f32 / 256.0; 3]).map(unorm_byte)).collect(),
                })
            } else { Lut::Gray((0..=255 * 256).map(|k| last([k as f32 / 256.0; 3])).collect()) }
        } else { Lut::Channels(std::array::from_fn(|i| last([pre[i]; 3]).map(|l| output_byte(l, key.native_encoded)))) };
        let bytes = std::mem::size_of::<Self>() + match &lut {
            Lut::Gray(v) => v.capacity() * std::mem::size_of::<[f32; 3]>(),
            Lut::GrayEncoded(v) => v.capacity() * std::mem::size_of::<[u8; 3]>(), Lut::Channels(_) => 0 };
        Self { key, pre, lut, bytes }
    }
    fn apply(&self, pixels: &mut [u8], encoder: Option<&Encoder>) {
        let scope = Scope::new(Metric::GradePixel);
        match &self.lut {
            Lut::Gray(lut) => for q in pixels.as_chunks_mut::<4>().0 {
                let encoder = encoder.expect("legacy grade encoder checked before pixel loop");
                // Keep the original floating operation order, interpolation, alpha.
                let g = (self.pre[q[0] as usize] * 54.0 + self.pre[q[1] as usize] * 183.0 + self.pre[q[2] as usize] * 19.0) / 256.0 * 256.0;
                let k = (g as usize).min(lut.len() - 2);
                let f = g - k as f32;
                for c in 0..3 { q[c] = encoder.encode(lut[k][c] + (lut[k + 1][c] - lut[k][c]) * f); }
            },
            Lut::GrayEncoded(lut) => for q in pixels.as_chunks_mut::<4>().0 {
                let index = q[0] as usize * 54 + q[1] as usize * 183 + q[2] as usize * 19;
                q[..3].copy_from_slice(&lut[index]);
            },
            Lut::Channels(lut) => for q in pixels.as_chunks_mut::<4>().0 {
                for c in 0..3 { q[c] = lut[q[c] as usize][c]; }
            },
        }
        if knife_perf::enabled() { scope.finish_hot(&self.key.label(), pixels.len() as u64, 0); }
    }
}
#[derive(Default)]
struct Cache { entries: VecDeque<Arc<Kernel>>, bytes: usize }
fn cache() -> &'static Mutex<Cache> { static CACHE: OnceLock<Mutex<Cache>> = OnceLock::new(); CACHE.get_or_init(Mutex::default) }
fn trim_lru_cache(cache:&mut Cache, need:usize)->usize {
    let before=cache.bytes;let mut i=0;
    while i<cache.entries.len() && before-cache.bytes<need {
        if Arc::strong_count(&cache.entries[i])==1 {let old=cache.entries.remove(i).unwrap();cache.bytes-=old.bytes;}
        else{i+=1;}
    }
    knife_perf::gauge_set(Gauge::GradeLutBytes,cache.bytes as u64);before-cache.bytes
}
pub(super) fn trim_lru(need:usize) -> usize {
    if need==0 {return 0;}
    let freed = match cache().try_lock() {
        Ok(mut cache) => trim_lru_cache(&mut cache, need),
        Err(TryLockError::Poisoned(e)) => trim_lru_cache(&mut e.into_inner(), need),
        Err(_) => 0,
    };
    if freed >= need { return freed; }
    // Never wait on the CPU decoder while the UI is reclaiming memory.
    let extra = match gray_bases().try_lock() {
        Ok(mut bases) => trim_gray_bases(&mut bases, need - freed),
        Err(TryLockError::Poisoned(e)) => trim_gray_bases(&mut e.into_inner(), need - freed),
        Err(_) => 0,
    };
    freed + extra
}

pub(super) fn apply(bake: &Bake, pixels: &mut [u8], srgb: bool) -> bool {
    let key = GradeKey::new(bake, srgb);
    let mut cache = cache().lock().unwrap_or_else(|e| e.into_inner());
    let kernel = if let Some(i) = cache.entries.iter().position(|e| e.key == key) {
        knife_perf::add(Metric::GradeLutHit, 0, 0);
        let kernel = cache.entries.remove(i).unwrap();
        cache.entries.push_back(kernel.clone()); kernel
    } else {
        knife_perf::add(Metric::GradeLutMiss, 0, 0);
        let build = Scope::new(Metric::GradeLutBuild);
        if !key.native_encoded && encoder().is_none() {
            knife_perf::add(Metric::GradeLutMiss, 0, 1); return false;
        }
        // The largest possible kernel is known before building. Keep cached
        // pinned kernels charged; fall back rather than overfill this cache.
        let gray = key.mode != 0 && key.row.is_some_and(|row| params_entry(&row.map(f32::from_bits)).grey);
        let needed = std::mem::size_of::<Kernel>() + if gray {
            (255 * 256 + 1) * if key.native_encoded { std::mem::size_of::<[u8; 3]>() }
                else { std::mem::size_of::<[f32; 3]>() }
        } else { 0 };
        while cache.bytes.saturating_add(needed) > CAP {
            let Some(i) = cache.entries.iter().position(|e| Arc::strong_count(e) == 1) else { return false; };
            let evicted = cache.entries.remove(i).unwrap(); cache.bytes -= evicted.bytes;
        }
        let kernel = Arc::new(Kernel::build(key));
        cache.bytes += kernel.bytes;
        cache.entries.push_back(kernel.clone());
        knife_perf::gauge_set(Gauge::GradeLutBytes, cache.bytes as u64);
        if knife_perf::enabled() { build.finish_hot(&kernel.key.label(), kernel.bytes as u64, 0); }
        kernel
    };
    drop(cache);
    let encoder = if kernel.key.native_encoded { None } else {
        let Some(encoder) = encoder() else { return false; }; Some(encoder)
    };
    kernel.apply(pixels, encoder);
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn pressure_trim_stops_at_need_and_keeps_live_kernel() {
        let key=GradeKey::in_color_domain(&Bake::NONE,false,true);
        let a=Arc::new(Kernel::build(key.clone()));
        let pinned=a.clone();let charge=a.bytes;
        let b=Arc::new(Kernel::build(key.clone()));let c=Arc::new(Kernel::build(key));
        let mut cache=Cache {entries:VecDeque::from([a,b,c]),bytes:3*charge};
        assert_eq!(trim_lru_cache(&mut cache,0),0);
        assert_eq!(trim_lru_cache(&mut cache,1),charge);
        assert_eq!(cache.entries.len(),2);assert_eq!(cache.bytes,2*charge);
        assert!(Arc::ptr_eq(&cache.entries[0],&pinned));
        assert_eq!(trim_lru_cache(&mut cache,usize::MAX),charge);
        assert_eq!(cache.bytes,charge);drop(pinned);
        assert_eq!(trim_lru_cache(&mut cache,1),charge);assert_eq!(cache.bytes,0);
    }
    #[test]
    fn native_srgb_metadata_reuses_key_and_keeps_exact_rgb_and_alpha() {
        let mut input = Vec::new();
        for byte in 0..=255u8 { input.extend_from_slice(&[byte, byte.wrapping_mul(37), 255 - byte, byte]); }
        for raw in [false, true] { for mode in [0, 2, 4, 19, 55] {
            let bake = Bake { grade: mode, bright: 0.91, contrast: 1.17, raw };
            let a = GradeKey::in_color_domain(&bake, false, true);
            let b = GradeKey::in_color_domain(&bake, true, true);
            assert_eq!(a, b, "Vita metadata must not duplicate a native grade");
            assert!(!a.srgb);
            let mut out_a = input.clone();
            let mut out_b = input.clone();
            Kernel::build(a).apply(&mut out_a, None);
            Kernel::build(b).apply(&mut out_b, None);
            assert_eq!(out_a, out_b);
            for (before, after) in input.as_chunks::<4>().0.iter().zip(out_a.as_chunks::<4>().0) {
                assert_eq!(before[3], after[3]);
            }
        } }
    }
    #[test]
    fn desktop_srgb_metadata_remains_a_distinct_color_domain() {
        let bake = Bake { grade: 0, bright: 0.91, contrast: 1.17, raw: false };
        let a = GradeKey::in_color_domain(&bake, false, false);
        let b = GradeKey::in_color_domain(&bake, true, false);
        assert_ne!(a, b);
        let input = [12, 63, 127, 79, 188, 222, 250, 183];
        let mut out_a = input;
        let mut out_b = input;
        let encoder = encoder().unwrap();
        Kernel::build(a).apply(&mut out_a, Some(encoder));
        Kernel::build(b).apply(&mut out_b, Some(encoder));
        assert_ne!(out_a, out_b, "desktop input sRGB decoding must remain effective");
        assert_eq!([out_a[3], out_a[7], out_b[3], out_b[7]], [79, 183, 79, 183]);
    }
    #[test]
    fn encoded_kernels_match_direct_grade_without_srgb_or_alter() {
        let mut input=Vec::new(); let mut seed=9u32;
        for _ in 0..4096 {seed=seed.wrapping_mul(1664525).wrapping_add(1013904223);input.extend_from_slice(&seed.to_le_bytes());}
        for mode in [0,2,3,4,19,55] {
            let bake=Bake{grade:mode,bright:1.13,contrast:0.62,raw:false};
            let mut key=GradeKey::new(&bake,true);key.native_encoded=true;
            let p=key.row.map(|r|params_entry(&r.map(f32::from_bits))).filter(|_|mode!=0);
            let mut reference=input.clone();
            for q in reference.as_chunks_mut::<4>().0 {
                let mut v=[q[0] as f32,q[1] as f32,q[2] as f32];
                if p.as_ref().is_some_and(|p|p.grey) {v=[(v[0]*54.0+v[1]*183.0+v[2]*19.0)/256.0;3];}
                if let Some(p)=&p {v=channels(p,v);}
                q[..3].copy_from_slice(&v.map(|v|unorm_byte(bake.tone(v))));
            }
            let kernel=Kernel::build(key);let mut out=input.clone();kernel.apply(&mut out,None);
            assert_eq!(out,reference,"native encoded grade {mode}");
            for (before,after) in input.as_chunks::<4>().0.iter().zip(out.as_chunks::<4>().0) {assert_eq!(before[3],after[3]);}
        }
    }
    #[test]
    fn real_native_dxt_images_grade_exactly_and_keep_original_alpha() {
        let Some(root)=std::env::var_os("KNIFE_NATIVE_GXT_TEST_FIXTURES")else{return};
        for name in ["IMG1207_60_60","IMG0181_01_01","IMG1205_02_02"] {
            let file=std::fs::read(std::path::Path::new(&root).join(format!("{name}.gxt"))).unwrap();
            let (_,_,input)=knife_core::format::gxt_decode::rgba8(&file,4*960*1080).unwrap();
            for mode in [2,4] {
                let bake=Bake{grade:mode,bright:0.9,contrast:1.2,raw:false};
                let mut key=GradeKey::new(&bake,false);key.native_encoded=true;
                let p=key.row.map(|r|params_entry(&r.map(f32::from_bits))).unwrap();
                let mut actual=input.clone();Kernel::build(key).apply(&mut actual,None);
                for (before,after) in input.as_chunks::<4>().0.iter().zip(actual.as_chunks::<4>().0) {
                    let mut v=[before[0]as f32,before[1]as f32,before[2]as f32];
                    if p.grey {v=[(v[0]*54.0+v[1]*183.0+v[2]*19.0)/256.0;3];}
                    let expected=channels(&p,v).map(|v|unorm_byte(bake.tone(v)));
                    assert_eq!(after[..3],expected,"{name} grade{mode}");assert_eq!(before[3],after[3]);
                }
            }
        }
    }
    #[test]
    fn target_formula_encoder_matches_edges_and_dense_random_floats() {
        let e = encoder().expect("host old encoder must have monotone transitions");
        for byte in 1..=255 { for bits in e.thresholds[byte]-8..=e.thresholds[byte]+8 {
            assert_eq!(e.encode(f32::from_bits(bits)), srgb_byte(f32::from_bits(bits)));
        } }
        let mut seed=7u32;
        for _ in 0..200000 { seed=seed.wrapping_mul(1664525).wrapping_add(1013904223);
            let l=f32::from_bits(seed);assert_eq!(e.encode(l),srgb_byte(l)); }
    }
    #[test]
    fn cached_kernels_are_byte_exact_against_original_loop() {
        let mut seed=11u32;let mut input=Vec::new();
        for _ in 0..8192 { seed=seed.wrapping_mul(1664525).wrapping_add(1013904223);input.extend_from_slice(&seed.to_le_bytes()); }
        for raw in [false,true] { for srgb in [false,true] { for grade in [0,2,3,4,19,55] {
            let bake=Bake { grade,bright:1.13,contrast:0.62,raw };
            let mut reference=input.clone();bake.apply_uncached(&mut reference,srgb);
            for _ in 0..2 { let mut output=input.clone();assert!(apply(&bake,&mut output,srgb));assert_eq!(output,reference,"grade {grade} raw {raw} srgb {srgb}"); }
        } } }
    }
}
