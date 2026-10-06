//! Scalar transition semantics used by backend conformance tests and platforms
//! without a shader path. Work is per pixel; scheduling and buffers live outside.
use krkr_protocol::{
    graphics::DrawFace,
    transition::{Direction, Effect, Frame, Stay},
};

/// A scroll pixel selects exactly one input; transparent pixels still replace
/// the other input. This is a copy operation, not alpha-over compositing.
pub fn scroll_source(frame: Frame, x: i32, y: i32) -> (bool, i32, i32) {
    let Effect::Scroll { from, stay } = frame.effect else {
        unreachable!("scroll frame");
    };
    let horizontal = matches!(from, Direction::Left | Direction::Right);
    let extent = if horizontal {
        frame.size.width
    } else {
        frame.size.height
    } as i32;
    let phase = frame.phase.min(extent as u32) as i32;
    let sign = if matches!(from, Direction::Left | Direction::Top) {
        1
    } else {
        -1
    };
    let first = if stay == Stay::Destination {
        0
    } else {
        sign * phase
    };
    let second = if stay == Stay::Source {
        0
    } else {
        sign * (phase - extent)
    };
    let coordinate = if horizontal { x } else { y };
    let in_first = (0..extent).contains(&(coordinate - first));
    let in_second = (0..extent).contains(&(coordinate - second));
    let source = if stay == Stay::Source {
        !in_first
    } else {
        in_second
    };
    let offset = if source { second } else { first };
    (
        source,
        x - if horizontal { offset } else { 0 },
        y - if horizontal { 0 } else { offset },
    )
}

/// A rule byte either selects an input verbatim or blends with an integer
/// opacity. Keeping the 256-value mapping outside shaders avoids wide integer
/// division in ES2 fragment programs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RuleBlend {
    First,
    Second,
    Opacity(u8),
}
pub fn universal_opacity(phase: u32, vague: u32, rule: u8) -> RuleBlend {
    let (phase, vague, rule) = (i64::from(phase), i64::from(vague), i64::from(rule));
    if vague < 512 {
        if rule >= phase {
            return RuleBlend::First;
        }
        if rule < phase - vague {
            return RuleBlend::Second;
        }
    }
    let opacity = if rule < phase - vague {
        255
    } else if rule >= phase {
        0
    } else {
        (255 - (rule - (phase - vague)) * 255 / vague).clamp(0, 255) as u8
    };
    RuleBlend::Opacity(opacity)
}

/// Packed AARRGGBB; lookup is the shared TVP opacity table from blend::lookup_table.
pub fn blend(frame: Frame, first: u32, second: u32, rule: u8, lookup: &[u8]) -> u32 {
    let max = frame.effect.phases(frame.size);
    if frame.phase == 0 {
        return first;
    }
    if frame.phase >= max {
        return second;
    }
    let universal = matches!(frame.effect, Effect::Universal { .. });
    let opacity = match frame.effect {
        Effect::CrossFade => frame.phase as i32,
        Effect::Universal { vague } => match universal_opacity(frame.phase, vague, rule) {
            RuleBlend::First => return first,
            RuleBlend::Second => return second,
            RuleBlend::Opacity(opacity) => i32::from(opacity),
        },
        Effect::Scroll { .. } | Effect::Custom => unreachable!("effect uses its own pixel kernel"),
    };
    let mut factor = opacity;
    let a1 = (first >> 24) as i32;
    let a2 = (second >> 24) as i32;
    let alpha = match frame.face {
        DrawFace::Alpha => {
            let weight = opacity + i32::from(!universal && opacity > 127);
            let addr = (((a2 * weight) & 0xff00) + ((a1 * (256 - weight)) >> 8)) as usize;
            factor = i32::from(lookup[addr * 4 + 3]);
            a1 + (((a2 - a1) * weight) >> 8)
        }
        DrawFace::AddAlpha => a1 + (((a2 - a1) * opacity) >> 8),
        _ => 0,
    };
    let mut out = (alpha as u32) << 24;
    for shift in [0, 8, 16] {
        let a = ((first >> shift) & 255) as i32;
        let b = ((second >> shift) & 255) as i32;
        out |= ((a + (((b - a) * factor) >> 8)) as u32 & 255) << shift;
    }
    out
}
