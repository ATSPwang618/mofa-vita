//! Only a dispatch's proven source-independent stage setters may defer baking.
//! The owner flushes before every other opcode and every actual dispatch exit.
const LAYERS: usize = 40;

#[cfg(any(target_os = "vita", test))]
use {crate::grade::Bake, knife_core::vm::parse::{Command, Op}};

#[derive(Default)]
pub(super) struct Batch {
    active: bool,
    mask: u64,
    order: Vec<u8>,
}

pub(super) fn tone(command: &knife_core::vm::parse::Command) -> (usize, f32) {
    let value = command.int_or(1, 0) as f32;
    match command.name.as_str() {
        "SNX2" => (0, 1.0 + value / 255.0),
        _ => (1, 1.0 + value * 0.01),
    }
}

pub(super) fn sprd_images(command: &knife_core::vm::parse::Command) -> Vec<String> {
    let Some(base) = command.arg(1) else { return Vec::new(); };
    let [a0, b0, a1, b1] = [2, 3, 4, 5].map(|index| command.int_or(index, 0));
    if a0 > a1 || b0 > b1 { return Vec::new(); }
    let a_values = [a0, a1];
    let b_values = [b0, b1];
    let mut images = Vec::with_capacity(4);
    for &a in &a_values[..1 + usize::from(a0 < a1)] {
        for &b in &b_values[..1 + usize::from(b0 < b1)] {
            let image = if a | b == 0 || base.len() + 6 > 35 { base.to_owned() }
                else { format!("{base}_{:02}_{:02}", a.rem_euclid(100), b.rem_euclid(100)) };
            images.push(image);
        }
    }
    images
}

#[cfg(any(target_os = "vita", test))]
pub(super) struct LookaheadLayer {
    pub name: String,
    pub bake: Bake,
    /// STEC and SNX values of a pending layer whose texture can still change.
    pub recipe: Option<(i32, [f32; 2])>,
}

#[cfg(any(target_os = "vita", test))]
fn push_image(wanted: &mut Vec<(String, Bake)>, name: &str, bake: Bake) {
    if wanted.len() < 32 && !wanted.iter().any(|(old, old_bake)| old == name && *old_bake == bake) {
        wanted.push((name.to_owned(), bake));
    }
}

#[cfg(any(target_os = "vita", test))]
pub(super) struct TransitionPlan {
    pub items: Vec<(String, Bake)>,
    pub reuse: Vec<(String, Bake)>,
    first_presentation: usize,
    through: usize,
}

#[cfg(any(target_os = "vita", test))]
impl TransitionPlan {
    pub(super) fn immediate(&self) -> &[(String, Bake)] {
        &self.items[..self.first_presentation]
    }
}

/// A carried binding may no longer appear in the lookahead once its STMA has
/// run. Keep it through the next presentation, bounded also by update frames.
#[cfg(any(target_os = "vita", test))]
#[derive(Default)]
pub(super) struct TransitionReuse(Vec<((String, Bake), usize, u64)>);

#[cfg(any(target_os = "vita", test))]
impl TransitionReuse {
    pub(super) fn advance(&mut self, pc: usize, frame: u64, plan: Option<&TransitionPlan>) {
        self.0.retain(|(_, through, expires)| pc <= *through && frame < *expires);
        let Some(plan) = plan else { return; };
        let through = pc.saturating_add(plan.through).saturating_add(2);
        let expires = frame.saturating_add(120);
        for key in &plan.reuse {
            if let Some((_, old_through, old_expiry)) = self.0.iter_mut().find(|(old, _, _)| old == key) {
                *old_through = (*old_through).max(through);
                *old_expiry = expires;
            } else if self.0.len() < 4 {
                self.0.push((key.clone(), through, expires));
            }
        }
    }

    pub(super) fn keys(&self) -> impl Iterator<Item = &(String, Bake)> {
        self.0.iter().map(|(key, _, _)| key)
    }
}

#[cfg(any(target_os = "vita", test))]
pub(super) fn image_lookahead(
    commands: &[Command], layers: [Option<LookaheadLayer>; LAYERS], coalesce: bool,
    exists: impl Fn(&str) -> bool,
) -> Vec<(String, Bake)> {
    plan_images(commands, layers, coalesce, exists, None).items
}

#[cfg(any(target_os = "vita", test))]
pub(super) fn transition_lookahead(
    commands: &[Command], layers: [Option<LookaheadLayer>; LAYERS], coalesce: bool,
    exists: impl Fn(&str) -> bool, motion: impl Fn(&str, i32, f32) -> Option<String>,
) -> TransitionPlan {
    plan_images(commands, layers, coalesce, exists, Some(&motion))
}

#[cfg(any(target_os = "vita", test))]
fn plan_images(
    commands: &[Command], mut layers: [Option<LookaheadLayer>; LAYERS], coalesce: bool,
    exists: impl Fn(&str) -> bool, motion: Option<&dyn Fn(&str, i32, f32) -> Option<String>>,
) -> TransitionPlan {
    let mut wanted = Vec::with_capacity(32);
    let mut reuse = Vec::new();
    let mut carried = layers.each_ref().map(Option::is_some);
    let mut boundaries = 0;
    let mut wait_ms = 0i32;
    let mut through = 0;
    let mut first_presentation = None;
    let mut batch = Batch::default();
    let materialize = |layers: &mut [Option<LookaheadLayer>; LAYERS], batch: &mut Batch, wanted: &mut Vec<_>| {
        for slot in batch.take() {
            if let Some(layer) = &mut layers[usize::from(slot)]
                && let Some((mode, [bright, contrast])) = layer.recipe {
                let bake = Bake::new(mode, bright, contrast);
                if bake != layer.bake {
                    push_image(wanted, &layer.name, bake);
                    layer.bake = bake;
                }
            }
        }
    };
    for (offset, command) in commands.iter().take(192).enumerate() {
        through = offset;
        let name = command.name.as_str();
        if !coalesce || !Batch::allows(name) {
            materialize(&mut layers, &mut batch, &mut wanted);
        }
        if motion.is_some() {
            // Messages and short visual waits have no control-flow edge. Stop
            // before a real branch, stage substitution, or longer hold.
            if matches!(command.op, Op::Message) || matches!(name, "FADS" | "WTTM" | "BTTM") {
                if first_presentation.is_none() {
                    if name == "FADS" {
                        // Dispatch may already have passed an STCH while the
                        // previous stage is still on screen. Its bound source
                        // belongs to this cut until the presentation commits.
                        for layer in layers.iter().flatten().filter(|layer| layer.recipe.is_some()) {
                            push_image(&mut wanted, &layer.name, layer.bake);
                        }
                    }
                    if !wanted.is_empty() { first_presentation = Some(wanted.len()); }
                }
                let ms = match name { "FADS" => command.int_or(1, 0),
                    "WTTM" | "BTTM" => command.int_or(0, 0), _ => 0 }.max(0);
                if boundaries == 5 || ms > 1500 || wait_ms.saturating_add(ms) > 2000 { break; }
                boundaries += 1;
                wait_ms += ms;
                for (layer, carried) in layers.iter_mut().zip(&mut carried) {
                    *carried = layer.is_some();
                    if let Some(layer) = layer { layer.recipe = None; }
                }
                continue;
            }
            if name == "SGMA" { break; }
            if name == "SPRD" || name == "SPFC" {
                let explicit = if name == "SPRD" { sprd_images(command) }
                    else { command.arg(1).map(str::to_owned).into_iter().collect() };
                for image in explicit {
                    let image = image.to_ascii_uppercase();
                    if exists(&image) { push_image(&mut wanted, &image, Bake::NONE); }
                }
                continue;
            }
        }
        // Follow one straight-line interval to the next presentation/wait, end or branch.
        if matches!(command.op, Op::Message) || matches!(name,
            "JUMP" | "GOTO" | "GOTS" | "IF__" | "IFSC" | "IFP4" | "IFLA" | "IFUR"
            | "SELA" | "SELD" | "SELS" | "END_" | "RTM_" | "RTMN" | "RDED" | "TTCT"
            | "FADS" | "WTTM" | "BTTM" | "WTIN" | "STCF" | "CLO2" | "CLO3" | "CLO4" | "CLO5"
            | "TOMP" | "TOMW" | "MSAD" | "SEWI") {
            break;
        }
        if name == "CKDP" { break; } // replaces five stage layers
        if !matches!(name, "STCC" | "STCH" | "SCH2" | "STEC" | "SNX2" | "SNX3" | "STMA") { continue; }
        let layer = command.int_or(0, if name == "STCC" { 0 } else { -1 });
        if name == "STCC" && layer < 0 { layers.fill_with(|| None); carried.fill(false); continue; }
        let Ok(slot) = usize::try_from(layer) else { continue; };
        if slot >= LAYERS { continue; }
        match name {
            "STCC" => { layers[slot] = None; carried[slot] = false; }
            "STCH" | "SCH2" => {
                let Some(base) = command.arg(1).map(str::to_ascii_uppercase) else { continue; };
                if matches!(base.as_str(), "BLACK" | "WHITE") { layers[slot] = None; continue; }
                let image = match (command.int(9), command.int(10)) {
                    (Some(a), Some(b)) if (a, b) != (0, 0) => {
                        let variant = format!("{base}_{a:02}_{b:02}");
                        if exists(&variant) { variant } else { base }
                    }
                    _ => base,
                };
                if !exists(&image) { continue; }
                carried[slot] = false;
                if let Some(source) = command.int(18).filter(|value| *value >= 0) {
                    let bake = layers.get(source as usize).and_then(Option::as_ref).or(layers[slot].as_ref())
                        .map_or(Bake::NONE, |src| src.bake);
                    push_image(&mut wanted, &image, bake);
                    layers[slot] = Some(LookaheadLayer { name: image, bake, recipe: None });
                } else {
                    push_image(&mut wanted, &image, Bake::NONE);
                    if layers[slot].as_ref().is_none_or(|old| old.recipe.is_some() || old.name != image) {
                        layers[slot] = Some(LookaheadLayer { name: image, bake: Bake::NONE, recipe: Some((0, [1.0; 2])) });
                    }
                }
            }
            "STMA" => {
                let id = command.int_or(1, 0);
                if id > 0 && let Some(motion) = motion && let Some(layer) = &mut layers[slot] {
                    if let Some(variant) = motion(&layer.name, id, command.int_or(3, 0) as f32)
                        && exists(&variant)
                    {
                        push_image(&mut wanted, &variant, layer.bake);
                        layer.name = variant;
                    } else if carried[slot] {
                        push_image(&mut reuse, &layer.name, layer.bake);
                    }
                }
            }
            "STEC" | "SNX2" | "SNX3" => {
                if let Some((mode, snx)) = layers[slot].as_mut().and_then(|layer| layer.recipe.as_mut()) {
                    if name == "STEC" {
                        let value = command.int_or(1, 0);
                        if !(0..=82).contains(&value) { continue; }
                        *mode = value;
                    } else {
                        let (channel, value) = tone(command);
                        snx[channel] = value;
                    }
                    batch.defer(layer);
                }
            }
            _ => {}
        }
    }
    materialize(&mut layers, &mut batch, &mut wanted);
    let first_presentation = first_presentation.unwrap_or(wanted.len());
    TransitionPlan { items: wanted, reuse, first_presentation, through }
}
impl Batch {
    pub(super) fn allows(name: &str) -> bool {
        matches!(name, "STEC" | "SNX2" | "SNX3" | "STBR" | "STZ4" | "STRT" | "SNX4")
    }
    pub(super) fn set_active(&mut self, active: bool) { self.active = active; }
    pub(super) fn active(&self) -> bool { self.active }
    /// None means immediate; Some reports whether this layer was already dirty.
    pub(super) fn queue(&mut self, layer: i32) -> Option<bool> {
        if !self.active || !(0..LAYERS as i32).contains(&layer) { return None; }
        Some(self.defer(layer))
    }
    /// Async materialization also collects setters invoked outside a coalesced
    /// script run (e.g. the five clock layers), until the next real barrier.
    pub(super) fn defer(&mut self, layer: i32) -> bool {
        if !(0..LAYERS as i32).contains(&layer) { return false; }
        let bit = 1u64 << layer;
        let already = self.mask & bit != 0;
        if !already { self.mask |= bit; self.order.push(layer as u8); }
        already
    }
    /// Disable before materializing so flushes cannot requeue themselves. The
    /// first-dirty order preserves load order across distinct layer images.
    pub(super) fn take(&mut self) -> Vec<u8> {
        self.active = false;
        self.mask = 0;
        std::mem::take(&mut self.order)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_proven_setters_coalesce_and_source_reads_are_barriers() {
        for name in ["STEC", "SNX2", "SNX3", "STBR", "STZ4", "STRT", "SNX4"] { assert!(Batch::allows(name)); }
        for name in ["STCH", "SCH2", "STMA", "STCP", "FADS", "SGMA", "CLO3", "JUMP", "NOP_", "ZM00000", "ZY00000"] { assert!(!Batch::allows(name)); }
        let mut batch = Batch::default();
        assert_eq!(batch.queue(0), None);
        batch.set_active(true);
        assert_eq!(batch.queue(21), Some(false));
        assert_eq!(batch.queue(21), Some(true));
        assert_eq!(batch.queue(21), Some(true));
        assert_eq!(batch.queue(4), Some(false));
        assert_eq!(batch.queue(-1), None);
        assert_eq!(batch.queue(40), None);
        assert_eq!(batch.take(), [21, 4]);
        assert_eq!(batch.queue(21), None);
        assert!(batch.take().is_empty());
    }
    #[test]
    fn frame_or_budget_exit_and_reverting_parameters_leave_final_state() {
        let mut batch=Batch::default(); let mut actual=[0;40]; let mut wanted=actual;
        let mut builds=0;
        // STEC, SNX2, SNX3 before a use: only the final texture is observable.
        batch.set_active(true);
        for v in [2,7,9] { wanted[21]=v; batch.queue(21); }
        for li in batch.take() { actual[li as usize]=wanted[li as usize]; builds+=1; }
        assert_eq!(actual[21],9); assert_eq!(builds,1);
        // A later dispatch that restores the already materialized Bake does no work.
        batch.set_active(true);
        for v in [3,9] { wanted[21]=v; batch.queue(21); }
        for li in batch.take() { if actual[li as usize]!=wanted[li as usize] { builds+=1; } }
        assert_eq!(builds,1);
        // At a real budget/frame boundary, the pending state is materialized;
        // the next dispatch cannot overwrite it across an observable boundary.
        batch.set_active(true); wanted[2]=5; batch.queue(2);
        for li in batch.take() { actual[li as usize]=wanted[li as usize]; }
        assert_eq!(actual[2],5); assert_eq!(batch.queue(2),None);
    }

}
