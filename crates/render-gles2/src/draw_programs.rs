//! Draw programs are specialized before GLSL reaches the Series5XT compiler.
//! The bounded LRU owns linked executables; no general sampling/blend dispatcher.
use crate::{Result, device::Device, drawing::Draw, shader::Program};
use std::{cell::RefCell, rc::Rc};

use crate::draw_source::{Key, Kind, Sampling};
impl Key {
    fn from_draw(draw: &Draw) -> Self {
        let kind = match draw.kind {
            0. | 6. | 7. | 8. | 9. => Kind::Raw,
            10. | 11. => Kind::PremultipliedDisplay,
            3. => Kind::Fill,
            2. => Kind::Solid,
            4. => Kind::Glyph,
            _ => Kind::Blend,
        };
        if kind == Kind::Glyph {
            return Self {
                kind,
                ..Self::raw()
            };
        }
        let sampling = if matches!(draw.kind, 2. | 3. | 5.) {
            Sampling::Constant
        } else if draw.kind == 7. {
            Sampling::Wrapped
        } else {
            match &draw.sampling {
                None => Sampling::Nearest,
                Some(sample) if sample.display && sample.sharpen => Sampling::SharpenedDisplay,
                Some(sample) if sample.display => Sampling::Display,
                Some(sample) if sample.scale.is_some() && sample.linear => Sampling::LogicalLinear,
                Some(sample) if sample.scale.is_some() && sample.clear => Sampling::LogicalAffine,
                Some(sample) if sample.scale.is_some() => Sampling::Logical,
                Some(sample) if sample.linear => Sampling::Linear,
                Some(_) => Sampling::Affine,
            }
        };
        Self {
            kind,
            sampling,
            mode: if kind == Kind::Blend || kind == Kind::Raw && sampling == Sampling::Display {
                draw.operation[0] as u8
            } else {
                0
            },
            face: if matches!(kind, Kind::Solid | Kind::Blend) {
                draw.operation[1] as u8
            } else {
                0
            },
            clear: matches!(
                sampling,
                Sampling::Affine
                    | Sampling::Linear
                    | Sampling::LogicalAffine
                    | Sampling::LogicalLinear
            ) && draw.sampling.as_ref().is_some_and(|s| s.clear),
            constant_backdrop: false,
            covered: false,
        }
    }
}
pub(crate) struct Programs {
    device: Rc<Device>,
    raw: Rc<Program>,
    cache: RefCell<Vec<(Key, Rc<Program>)>>,
    colors: RefCell<[Option<Rc<Program>>; 3]>,
}
impl Programs {
    pub fn new(device: Rc<Device>) -> Result<Self> {
        let raw = Rc::new(Self::compile(device.clone(), Key::raw())?);
        Ok(Self {
            device,
            raw,
            cache: RefCell::new(Vec::new()),
            colors: RefCell::new([None, None, None]),
        })
    }
    pub fn raw(&self) -> &Program {
        &self.raw
    }
    pub fn colors(&self, face: u8) -> Result<Rc<Program>> {
        let index = match face {
            0 => 0,
            1 => 1,
            4 => 2,
            _ => return Err(crate::Error::Message("invalid color batch face")),
        };
        let mut colors = self.colors.borrow_mut();
        let slot = &mut colors[index];
        if slot.is_none() {
            *slot = Some(Rc::new(Program::new(
                self.device.clone(),
                include_str!("color_batch.vert"),
                &crate::draw_source::color_batch(face),
            )?));
        }
        Ok(slot.as_ref().unwrap().clone())
    }
    pub fn select(&self, draw: &Draw) -> Result<Rc<Program>> {
        self.select_backdrop(draw, false)
    }
    pub fn select_backdrop(&self, draw: &Draw, constant: bool) -> Result<Rc<Program>> {
        self.select_covered(draw, constant, false)
    }
    pub fn select_covered(
        &self,
        draw: &Draw,
        constant: bool,
        covered: bool,
    ) -> Result<Rc<Program>> {
        let mut key = Key::from_draw(draw);
        key.constant_backdrop = constant;
        key.covered = covered;
        if key == Key::raw() {
            return Ok(self.raw.clone());
        }
        let mut cache = self.cache.borrow_mut();
        if let Some(index) = cache.iter().rposition(|(old, _)| *old == key) {
            let entry = cache.remove(index);
            let program = entry.1.clone();
            cache.push(entry);
            return Ok(program);
        }
        // Evict before compiling: the linker also needs temporary device memory.
        if cache.len() == 24 {
            cache.remove(0);
        }
        let program = Rc::new(Self::compile(self.device.clone(), key)?);
        cache.push((key, program.clone()));
        Ok(program)
    }
    fn compile(device: Rc<Device>, key: Key) -> Result<Program> {
        let result = Program::new(
            device,
            include_str!("quad.vert"),
            &crate::draw_source::fragment(key),
        );
        result.map_err(|error| crate::Error::Backend(format!("draw variant {key:?}: {error}")))
    }
}

#[cfg(all(test, target_os = "linux"))]
#[path = "../tests/internal/covered_draw.rs"]
mod tests;
