//! Fixed-input GPU replay of the normal Stage draw list. Raw captures stay in
//! memory; diagnostics contain bounded timings, byte differences and hashes.

const WARM_FRAMES: usize = 30;
const MEASURE_FRAMES: usize = 120;
const ABBA: [bool; 4] = [false, true, true, false];

fn replay_toggle(profile: u64) -> (&'static str, u64) {
    use knife_perf::optimizations::{PS_ALL_OPERATIONS, PS_FULL_MATERIAL, PS_PLAIN_MATERIAL, PS_PLAIN_OP7, PS_CHAIN_OP7};
    if profile & (PS_PLAIN_OP7 | PS_CHAIN_OP7) == (PS_PLAIN_OP7 | PS_CHAIN_OP7) { ("ps-chain-op7",PS_CHAIN_OP7) }
    else if profile & PS_PLAIN_OP7 != 0 { ("ps-plain-op7",PS_PLAIN_OP7) }
    else if profile & PS_ALL_OPERATIONS != 0 { ("ps-all-operations",PS_ALL_OPERATIONS) }
    else if profile & PS_FULL_MATERIAL != 0 { ("ps-full-material", PS_FULL_MATERIAL) }
    else { ("ps-plain-material", PS_PLAIN_MATERIAL) }
}

/// Legacy plain candidates are Stage quads; complete-source candidates include
/// every Stage/Pixel/Mask quad and final group Composite. These are input
/// opportunities only: native program deltas establish actual selection.
fn input_candidates(draws: impl IntoIterator<Item = (bool, Option<u32>, bool)>) -> ([usize; 3], [usize; 3]) {
    let mut plain = [0; 3];
    let mut full = [0; 3];
    for (stage, operation, plain_source) in draws {
        let slot = match operation { Some(1) => 0, Some(6) => 1, Some(13) => 2, _ => continue };
        full[slot] += 1;
        if stage && plain_source { plain[slot] += 1; }
    }
    (plain, full)
}

#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
struct ByteComparison {
    bytes: usize,
    differing_bytes: usize,
    differing_pixels: usize,
    maximum_delta: u8,
    absolute_delta: u64,
    first_byte: Option<usize>,
}

fn compare_rgba(reference: &[u8], actual: &[u8]) -> Result<ByteComparison, &'static str> {
    if reference.len() != actual.len() || reference.len() % 4 != 0 {
        return Err("RGBA8 capture lengths differ or are not whole pixels");
    }
    let mut out = ByteComparison { bytes: reference.len(), ..ByteComparison::default() };
    for (pixel, (a, b)) in reference.chunks_exact(4).zip(actual.chunks_exact(4)).enumerate() {
        let mut changed = false;
        for channel in 0..4 {
            let delta = a[channel].abs_diff(b[channel]);
            if delta != 0 {
                out.differing_bytes += 1;
                changed = true;
                out.first_byte.get_or_insert(pixel * 4 + channel);
            }
            out.maximum_delta = out.maximum_delta.max(delta);
            out.absolute_delta += u64::from(delta);
        }
        out.differing_pixels += usize::from(changed);
    }
    Ok(out)
}

fn capture_hash(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |hash, byte|
        (hash ^ u64::from(*byte)).wrapping_mul(0x100_0000_01b3))
}

/// The first write of each persistent group surface must explicitly clear it.
/// A composite needs both producers; no previous production pixels are borrowed.
fn independent_groups(steps: impl IntoIterator<Item = (u8, bool)>) -> bool {
    let mut initialized = [false; 2];
    for (kind, clear) in steps {
        match kind {
            0 | 1 => {
                let slot = kind as usize;
                if !initialized[slot] && !clear { return false; }
                initialized[slot] = true;
            }
            2 if !initialized[0] || !initialized[1] => return false,
            _ => {}
        }
    }
    true
}

#[cfg(target_os = "vita")]
pub(crate) use runtime::*;

#[cfg(target_os = "vita")]
mod runtime {
    use super::*;
    use crate::{game::Game, stage::Draw};
    use anyhow::{Context, Result, ensure};
    use knife_render::{Blend, CANVAS_H, CANVAS_W, Quad, RenderTarget, Renderer, Shade, TexRef};
    use std::{sync::Arc, time::Instant};

    const MAX_DRAWS: usize = 512;

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub(crate) struct Origin {
        pub scene: usize,
        pub pc: usize,
        pub update_frame: u64,
        pub frame_ms_bits: u32,
        pub time_bits: u32,
    }

    pub(crate) struct FrozenStage {
        pub draws: Vec<Draw>,
        pub backdrop: u8,
        origin: Origin,
        physical: [u32; 2],
        sources: Vec<(TexRef, (usize, u64))>,
        groups: bool,
        plain_candidates: [usize; 3],
        full_candidates: [usize; 3],
        all_candidates: [usize; 24],
        recipe_signature: u64,
    }

    #[derive(Default)]
    pub(crate) struct CaptureState {
        armed: bool,
        ready: Option<Result<FrozenStage>>,
    }

    impl CaptureState {
        pub fn arm(&mut self) -> Result<()> {
            ensure!(!self.armed && self.ready.is_none(), "GPU replay capture already pending");
            self.armed = true;
            Ok(())
        }

        pub fn take(&mut self) -> Option<Result<FrozenStage>> { self.ready.take() }

        /// Called once from the ordinary render_stage, before producer reuse.
        pub fn capture(&mut self, draws: &[Draw], white: &TexRef, origin: Origin,
            backdrop: u8, physical: [u32; 2])
        {
            if !std::mem::take(&mut self.armed) { return; }
            self.ready = Some(FrozenStage::capture(draws, white, origin, backdrop, physical));
        }
    }

    impl FrozenStage {
        fn capture(draws: &[Draw], white: &TexRef, origin: Origin,
            backdrop: u8, physical: [u32; 2]) -> Result<Self>
        {
            ensure!(!draws.is_empty(), "current Stage has no draws to replay");
            ensure!(draws.len() <= MAX_DRAWS, "Stage replay exceeds bounded draw capacity");
            ensure!(independent_groups(draws.iter().filter_map(|draw| match draw {
                Draw::Stage(_) => None,
                Draw::Pixel { clear, .. } => Some((0,*clear)),
                Draw::Mask { clear, .. } => Some((1,*clear)),
                Draw::Composite { .. } => Some((2,false)),
            })), "Stage replay needs previous-frame group pixels; capture rejected");
            let mut sources: Vec<(TexRef, (usize,u64))> = Vec::new();
            let mut all_candidates=[0usize;24];
            for draw in draws {
                let blend=match draw {Draw::Stage(q)|Draw::Pixel{quad:q,..}|Draw::Mask{quad:q,..}=>q.blend,Draw::Composite{blend,..}=>*blend};
                if let Blend::Ps(op)=blend {all_candidates[op as usize-1]+=1;}
            }
            let (plain_candidates, full_candidates) = input_candidates(draws.iter().map(|draw| {
                let (stage, blend, plain) = match draw {
                    Draw::Stage(q) => (true,q.blend,plain_source(q)),
                    Draw::Pixel { quad:q, .. } | Draw::Mask { quad:q, .. } => (false,q.blend,plain_source(q)),
                    Draw::Composite { blend, .. } => (false,*blend,false),
                };
                (stage, if let Blend::Ps(op) = blend { Some(op as u32) } else { None }, plain)
            }));
            for draw in draws {
                let quad = match draw {
                    Draw::Stage(q) | Draw::Pixel { quad: q, .. } | Draw::Mask { quad: q, .. } => q,
                    Draw::Composite { .. } => continue,
                };
                for source in std::iter::once(&quad.tex).chain(quad.rule.as_ref().map(|m| &m.tex)) {
                    ensure!(source.vita_immutable_stage_source() || Arc::ptr_eq(source, white),
                        "Stage replay contains a movie, mutable target, or unverified writable source");
                    let identity = source.render_identity();
                    if !sources.iter().any(|(_, id)| id.0 == identity.0) {
                        sources.push((source.clone(), identity));
                    }
                }
            }
            let recipe_signature = draws.iter().fold(0xcbf2_9ce4_8422_2325u64, |hash, draw| {
                let (kind,value) = match draw {
                    Draw::Stage(q) => (0,q.render_signature()),
                    Draw::Pixel { quad,clear } => (1+u64::from(*clear),quad.render_signature()),
                    Draw::Mask { quad,clear } => (3+u64::from(*clear),quad.render_signature()),
                    Draw::Composite { blend,alpha } => {
                        use std::hash::{Hash,Hasher};
                        let mut h = std::collections::hash_map::DefaultHasher::new();
                        blend.hash(&mut h); alpha.to_bits().hash(&mut h); (5,h.finish())
                    }
                };
                ((hash ^ kind).wrapping_mul(0x100_0000_01b3) ^ value).wrapping_mul(0x100_0000_01b3)
            });
            Ok(Self { draws: draws.to_vec(), backdrop, origin, physical, sources, recipe_signature,
                groups: draws.iter().any(|d| !matches!(d,Draw::Stage(_))), plain_candidates, full_candidates, all_candidates })
        }

        fn validate_sources(&self) -> Result<()> {
            ensure!(self.sources.iter().all(|(source, identity)| source.render_identity() == *identity),
                "frozen Stage source pixels changed during GPU replay");
            Ok(())
        }
    }

    fn plain_source(q: &Quad) -> bool {
        matches!(q.shade, Shade::Sprite | Shade::Fill | Shade::NoAlpha)
            && q.rule.is_none() && q.soft.is_none() && !q.masked
            && q.haze.iter().flatten().all(|v| *v == 0.0)
    }

    /// Separate targets own every replay write. The Game method temporarily
    /// lends them to the same production render_stage_draws implementation.
    pub(crate) struct ReplayTargets {
        pub stage: RenderTarget,
        pub scratch: Option<RenderTarget>,
        pub groups: Option<[RenderTarget; 2]>,
    }

    impl ReplayTargets {
        fn new(r: &Renderer, frozen: &FrozenStage) -> Result<Self> {
            let stage = r.try_create_scene_target(CANVAS_W, CANVAS_H, "GPU replay stage")?;
            ensure!(stage.tex.physical_size() == frozen.physical,
                "GPU replay and production Stage dimensions differ");
            let scratch = frozen.groups.then(||
                r.try_create_scene_target(CANVAS_W,CANVAS_H,"GPU replay group scratch")).transpose()?;
            let groups = if frozen.groups {
                Some([r.try_create_scene_target(CANVAS_W,CANVAS_H,"GPU replay group pixel")?,
                    r.try_create_scene_target(CANVAS_W,CANVAS_H,"GPU replay group mask")?])
            } else { None };
            Ok(Self { stage, scratch, groups })
        }
    }

    #[derive(Default)]
    struct Samples(Vec<u64>);

    impl Samples {
        fn values_json(&self) -> String {
            let mut out = String::from("[");
            for (index, value) in self.0.iter().enumerate() {
                if index != 0 { out.push(','); }
                out.push_str(&value.to_string());
            }
            out.push(']');
            out
        }

        fn json(&self) -> String {
            let mut values = self.0.clone(); values.sort_unstable();
            let count = values.len();
            let sum = values.iter().sum::<u64>();
            format!("{{\"n\":{count},\"total_us\":{sum},\"mean_us\":{:.3},\"min_us\":{},\"p50_us\":{},\"p95_us\":{},\"p99_us\":{},\"max_us\":{}}}",
                sum as f64 / count.max(1) as f64, values.first().copied().unwrap_or(0),
                values.get(count.saturating_sub(1)/2).copied().unwrap_or(0),
                values.get(count.saturating_sub(1)*95/100).copied().unwrap_or(0),
                values.get(count.saturating_sub(1)*99/100).copied().unwrap_or(0),
                values.last().copied().unwrap_or(0))
        }
    }

    struct Block {
        candidate: bool,
        mask: u64,
        record: Samples,
        completion: Samples,
        capture_us: u64,
        hash: u64,
        comparison: ByteComparison,
        work_before: String,
        work_after: String,
    }

    pub(crate) struct Report {
        origin: Origin,
        physical: [u32; 2],
        draws: usize,
        sources: usize,
        plain_candidates: [usize; 3],
        full_candidates: [usize; 3],
        all_candidates: [usize; 24],
        recipe_signature: u64,
        baseline_mask: u64,
        toggle: &'static str,
        toggle_mask: u64,
        blocks: Vec<Block>,
    }

    impl Report {
        /// Five bounded JSON lines; no full-size regular diagnostic image file.
        pub fn json_lines(&self) -> Vec<String> {
            let exact = self.blocks.iter().all(|b| b.comparison.differing_bytes == 0);
            let mut lines = vec![format!(concat!(
                "{{\"type\":\"gpu_replay\",\"schema\":1,\"origin\":\"current_production_stage\",",
                "\"scene\":{},\"pc\":{},\"update_frame\":{},\"frame_ms_bits\":{},\"time_bits\":{},",
                "\"draws\":{},\"sources\":{},\"physical\":[{},{}],\"plain_input_candidates\":[{},{},{}],",
                "\"all_input_candidates\":{},",
                "\"full_input_candidates\":[{},{},{}],\"full_input_candidate_scope\":\"Stage,Pixel,Mask,Composite\",",
                "\"recipe_signature_in_process\":\"{:016x}\",",
                "\"baseline_mask\":{},\"toggle\":\"{}\",\"toggle_mask\":{},\"order\":\"ABBA\",",
                "\"warm_per_block\":{},\"measure_per_block\":{},\"exact_equal\":{},",
                "\"readback\":\"native_linear_rgba8_after_gpu_and_transfer_finish\",",
                "\"timing_scope\":\"record CPU; submit+GPU completion; readback separately\",",
                "\"scope\":\"frozen current Stage only; no story/UI/animation progression; fresh local memoization per frame; final measured output of each block compared; no global script or visual equivalence claim\"}}"),
                self.origin.scene,self.origin.pc,self.origin.update_frame,self.origin.frame_ms_bits,self.origin.time_bits,
                self.draws,self.sources,self.physical[0],self.physical[1],self.plain_candidates[0],self.plain_candidates[1],self.plain_candidates[2],
                format!("[{}]",self.all_candidates.iter().map(usize::to_string).collect::<Vec<_>>().join(",")),
                self.full_candidates[0],self.full_candidates[1],self.full_candidates[2],
                self.recipe_signature,self.baseline_mask,self.toggle,self.toggle_mask,WARM_FRAMES,MEASURE_FRAMES,exact)];
            for (index, block) in self.blocks.iter().enumerate() {
                let c = block.comparison;
                lines.push(format!(concat!(
                    "{{\"type\":\"gpu_replay_block\",\"index\":{},\"candidate\":{},\"mask\":{},",
                    "\"record\":{},\"submit_complete\":{},\"submit_complete_us\":{},\"capture_us\":{},\"fnv1a64\":\"{:016x}\",",
                    "\"reference\":\"A0\",\"bytes\":{},\"differing_bytes\":{},\"differing_pixels\":{},",
                    "\"maximum_channel_delta\":{},\"absolute_channel_delta\":{},\"first_byte\":{},",
                    "\"work_counter_scope\":\"warm+measure+capture\",\"executed_work_before\":{},\"executed_work_after\":{}}}"),
                    index,block.candidate,block.mask,block.record.json(),block.completion.json(),block.completion.values_json(),block.capture_us,block.hash,
                    c.bytes,c.differing_bytes,c.differing_pixels,c.maximum_delta,c.absolute_delta,
                    c.first_byte.map_or_else(|| "null".into(),|n| n.to_string()),block.work_before,block.work_after));
            }
            lines
        }
    }

    fn select(mask: u64) {
        knife_perf::optimizations::set(mask);
        knife_platform_vita::audit::set_options(mask);
    }

    /// The caller invokes this after a successful ordinary frame submission and
    /// does not update Game/Ui or save while the synchronous replay runs.
    pub(crate) fn run(game: &mut Game, r: &mut Renderer, frozen: FrozenStage,
        mut progress: impl FnMut(usize,usize)) -> Result<Report>
    {
        let saved_mask = knife_perf::optimizations::mask();
        ensure!(saved_mask & knife_perf::optimizations::PS_FETCH != 0,
            "GPU replay requires the existing ps-fetch profile");
        ensure!(game.vita_gpu_replay_origin() == frozen.origin,
            "Game clock or script changed after Stage capture");
        let production_targets = game.vita_gpu_replay_production_targets();
        let saved_hidden = r.hidden;
        let fence = r.movie_gpu_fence();
        fence.finish().context("draining production GPU work before replay")?;
        let mut targets = ReplayTargets::new(r,&frozen).context("allocating isolated GPU replay targets")?;
        r.hidden = true;
        let result = run_inner(game,r,&frozen,&mut targets,&mut progress);
        // Drain before private owners disappear, including the error path. The
        // native owner retains uncertain submissions if a completion fails.
        let drain = fence.finish().context("draining GPU replay before restore");
        r.hidden = saved_hidden;
        select(saved_mask);
        ensure!(game.vita_gpu_replay_production_targets() == production_targets,
            "GPU replay changed a production target owner or pixel generation");
        let report = match (result,drain) {
            (Ok(report),Ok(())) => report,
            (Err(error),Ok(())) | (Ok(_),Err(error)) => return Err(error),
            (Err(error),Err(drain)) => return Err(error.context(format!("GPU replay final drain also failed: {drain:#}"))),
        };
        ensure!(game.vita_gpu_replay_origin() == frozen.origin,
            "GPU replay changed a Game clock or script position");
        Ok(report)
    }

    fn run_inner(game: &mut Game, r: &mut Renderer, frozen: &FrozenStage, targets: &mut ReplayTargets,
        progress: &mut impl FnMut(usize,usize)) -> Result<Report>
    {
        let profile = knife_perf::optimizations::mask();
        let (toggle, toggle_mask) = replay_toggle(profile);
        let baseline_mask = profile & !toggle_mask;
        let fence = r.movie_gpu_fence();
        let mut reference: Option<Vec<u8>> = None;
        let mut blocks = Vec::with_capacity(ABBA.len());
        for (block_index, candidate) in ABBA.into_iter().enumerate() {
            let mask = baseline_mask | if candidate { toggle_mask } else { 0 };
            select(mask);
            let mut block = Block { candidate, mask, record: Samples::default(), completion: Samples::default(),
                capture_us: 0, hash: 0, comparison: ByteComparison::default(),
                work_before: r.vita_work_stats_json(), work_after: String::new() };
            for frame in 0..WARM_FRAMES+MEASURE_FRAMES {
                progress(block_index,frame);
                frozen.validate_sources()?;
                let started = Instant::now();
                let mut encoder = r.encoder();
                game.vita_gpu_replay_record(r,&mut encoder,frozen,targets);
                let recorded = started.elapsed().as_micros().min(u64::MAX as u128) as u64;
                let submitted = Instant::now();
                r.try_present(encoder,&targets.stage).context("submitting fixed Stage replay")?;
                fence.finish().context("waiting for fixed Stage GPU completion")?;
                let complete = submitted.elapsed().as_micros().min(u64::MAX as u128) as u64;
                if frame >= WARM_FRAMES {
                    block.record.0.push(recorded);
                    block.completion.0.push(complete);
                }
            }
            let started = Instant::now();
            let actual = r.try_read_target(&targets.stage).context("reading completed fixed Stage RGBA8")?;
            block.capture_us = started.elapsed().as_micros().min(u64::MAX as u128) as u64;
            block.hash = capture_hash(&actual);
            block.comparison = compare_rgba(reference.as_deref().unwrap_or(&actual),&actual)
                .map_err(anyhow::Error::msg)?;
            if reference.is_none() { reference = Some(actual); }
            block.work_after = r.vita_work_stats_json();
            blocks.push(block);
        }
        // All target writes and reads have completed before these owners drop.
        fence.finish()?;
        Ok(Report { origin:frozen.origin, physical:frozen.physical, draws:frozen.draws.len(),
            sources:frozen.sources.len(), plain_candidates:frozen.plain_candidates, full_candidates:frozen.full_candidates, all_candidates:frozen.all_candidates,
            recipe_signature:frozen.recipe_signature, baseline_mask, toggle, toggle_mask, blocks })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_rgba_comparison_counts_pixels_channels_and_first_difference() {
        let a = [0, 255, 18, 9, 20, 30, 40, 50];
        let b = [0, 253, 18, 10, 20, 30, 42, 50];
        assert_eq!(compare_rgba(&a, &b).unwrap(), ByteComparison {
            bytes: 8, differing_bytes: 3, differing_pixels: 2, maximum_delta: 2,
            absolute_delta: 5, first_byte: Some(1),
        });
        assert_eq!(compare_rgba(&a, &a).unwrap().differing_bytes, 0);
        assert!(compare_rgba(&a, &b[..4]).is_err());
        assert!(compare_rgba(&a[..3], &b[..3]).is_err());
        assert_ne!(capture_hash(&a), capture_hash(&b));
        assert_eq!(capture_hash(&[]), 0xcbf2_9ce4_8422_2325);
    }

    #[test]
    fn inherited_group_pixels_cannot_become_a_fixed_fixture() {
        assert!(independent_groups([]));
        assert!(independent_groups([(0,true),(1,true),(2,false),(0,false),(2,false)]));
        assert!(!independent_groups([(0,false),(1,true),(2,false)]));
        assert!(!independent_groups([(0,true),(2,false),(1,true)]));
        assert!(!independent_groups([(2,false)]));
    }

    #[test]
    fn schedule_has_independent_warmup_and_equal_measured_abba_blocks() {
        assert_eq!(ABBA, [false,true,true,false]);
        assert_eq!(WARM_FRAMES, 30);
        assert_eq!(MEASURE_FRAMES, 120);
        assert_eq!(ABBA.len() * (WARM_FRAMES + MEASURE_FRAMES), 600);
    }

    #[test]
    fn all_operations_replay_takes_priority_and_keeps_previous_options() {
        use knife_perf::optimizations::{PS_ALL_OPERATIONS,PS_FULL_MATERIAL,PS_PLAIN_MATERIAL};
        let profile=PS_ALL_OPERATIONS|PS_FULL_MATERIAL|PS_PLAIN_MATERIAL;
        assert_eq!(replay_toggle(profile),("ps-all-operations",PS_ALL_OPERATIONS));
        let baseline=profile & !PS_ALL_OPERATIONS;
        assert_eq!(baseline,PS_FULL_MATERIAL|PS_PLAIN_MATERIAL);
    }
    #[test]
    fn op7_replay_changes_only_one_new_bit() {
        use knife_perf::optimizations::{HARDWARE_TEST,PS_PLAIN_OP7,PS_CHAIN_OP7};
        assert_eq!(replay_toggle(HARDWARE_TEST),("ps-chain-op7",PS_CHAIN_OP7));
        assert_eq!(replay_toggle(HARDWARE_TEST & !PS_CHAIN_OP7),("ps-plain-op7",PS_PLAIN_OP7));
        for profile in [HARDWARE_TEST,HARDWARE_TEST & !PS_CHAIN_OP7] {
            let (_,bit)=replay_toggle(profile);let baseline=profile & !bit;
            for candidate in ABBA {assert_eq!((baseline | if candidate {bit} else {0}) & !bit,profile & !bit);}
        }
    }

    #[test]
    fn full_profile_takes_priority_and_abba_changes_only_its_bit() {
        use knife_perf::optimizations::{PS_FETCH, PS_FULL_MATERIAL, PS_PLAIN_MATERIAL};
        for profile in [PS_FETCH | PS_PLAIN_MATERIAL | PS_FULL_MATERIAL,
            PS_FETCH | PS_FULL_MATERIAL, PS_FETCH | PS_PLAIN_MATERIAL, PS_FETCH] {
            let (name, bit) = replay_toggle(profile);
            let full = profile & PS_FULL_MATERIAL != 0;
            assert_eq!(name, if full { "ps-full-material" } else { "ps-plain-material" });
            assert_eq!(bit, if full { PS_FULL_MATERIAL } else { PS_PLAIN_MATERIAL });
            let baseline = profile & !bit;
            for candidate in ABBA {
                let selected = baseline | if candidate { bit } else { 0 };
                assert_eq!(selected & !bit, profile & !bit);
                assert_eq!(selected & bit != 0, candidate);
            }
        }
    }

    #[test]
    fn complete_candidates_include_all_producers_and_group_composites() {
        // Stage plain/complex, Pixel, Mask and Composite, including an operation
        // outside this candidate family and a non-PS draw.
        let (plain,full) = input_candidates([
            (true,Some(1),true), (true,Some(6),false), (true,Some(13),true),
            (false,Some(1),true), (false,Some(6),false), (false,Some(13),false),
            (false,Some(1),false), (true,Some(7),true), (true,None,true),
        ]);
        assert_eq!(plain,[1,0,1]);
        assert_eq!(full,[3,2,2]);
        assert_eq!(input_candidates([]),([0;3],[0;3]));
    }
}
