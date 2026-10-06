//! Numeric state recovered from layerExParticle.dll's four particle managers.
//! Parameter offsets identify the reference fields, not native memory addresses.
use krkr_engine::protocol::budget::{Budget, Permit};
use std::f64::consts::TAU;
use tjs_core::{NativeError, NativeResult};

#[derive(Clone, Copy, Default)]
pub(super) struct Particle {
    pub x: f64,
    pub y: f64,
    pub magnify: f64,
    pub magnify_delta: f64,
    pub angle: f64,
    pub angle_delta: f64,
    pub opacity: f64,
    pub opacity_delta: f64,
    pub age: i32,
    pub image: usize,
    pub motion: [f64; 6],
}
pub(super) struct Model {
    pub kind: i32,
    pub start: usize,
    pub max: usize,
    pub count: usize,
    pub rate: f64,
    pub accumulated: f64,
    // Common fields end at 0xb0. The union of the four manager parameter
    // layouts retains the DLL's intentional aliases and cached range widths.
    pub parameters: [f64; 43],
    pub particles: Vec<Particle>,
    next_free: usize,
    permits: Vec<Permit>,
}
pub(super) fn error(e: impl std::fmt::Display) -> NativeError {
    NativeError::Detail(e.to_string())
}
impl Model {
    pub fn new(
        kind: i32,
        start: usize,
        max: usize,
        rate: f64,
        area: [f64; 4],
        budget: &Budget,
    ) -> NativeResult<Self> {
        let mut s = Self {
            kind,
            start: start.min(max),
            max: 0,
            count: 0,
            rate,
            accumulated: 0.,
            parameters: [0.; 43],
            particles: Vec::new(),
            next_free: 0,
            permits: Vec::new(),
        };
        s.pair(0x58, 1., 1.);
        s.pair(0x88, 255., 0.);
        s.parameters[0xd8 / 8] = area[0];
        s.parameters[0xe0 / 8] = area[1];
        match kind {
            1 => {
                s.parameters[0xe8 / 8] = area[2];
                s.parameters[0xf0 / 8] = area[3];
                s.pair(0xf8, 0.1, 0.1);
                s.pair(0x128, TAU, 0.);
            }
            2 | 4 => {
                s.pair(0xe8, 1024., 0.);
                s.pair(0x118, TAU / 1000., 0.);
                if kind == 4 {
                    s.pair(0x138, 2000., 1000.);
                }
            }
            3 => {
                s.parameters[0xe8 / 8] = area[2];
                s.parameters[0xf0 / 8] = area[3];
                s.pair(0x88, 255., 255.);
                s.pair(0xf8, 1000., 500.);
                s.pair(0x110, 1., 1.);
            }
            _ => unreachable!(),
        }
        s.resize(max, budget)?;
        Ok(s)
    }
    pub fn get(&self, at: usize) -> f64 {
        self.parameters[at / 8]
    }
    pub fn pair(&mut self, at: usize, max: f64, min: f64) {
        self.parameters[at / 8..at / 8 + 3].copy_from_slice(&[max, min, max - min]);
    }
    pub fn range(&self, at: usize, rng: &mut u32) -> f64 {
        self.get(at + 8) + random(rng) * self.get(at + 16)
    }
    pub fn resize(&mut self, max: usize, budget: &Budget) -> NativeResult<()> {
        if max > self.particles.len() {
            let count = max - self.particles.len();
            let permit = budget
                .reserve(
                    count
                        .checked_mul(std::mem::size_of::<Particle>())
                        .ok_or(NativeError::Message("particle count overflow"))?,
                )
                .map_err(error)?;
            self.particles.try_reserve_exact(count).map_err(error)?;
            self.particles.resize(
                max,
                Particle {
                    age: -1,
                    ..Particle::default()
                },
            );
            self.permits.push(permit);
        }
        while self.count > max {
            if !self.kill_oldest() {
                break;
            }
        }
        self.max = max;
        Ok(())
    }
    pub fn duplicate(&self, budget: &Budget) -> NativeResult<Self> {
        // The old assign allocates fresh derived particles but only copies
        // their base fields, leaving motion uninitialized. Managed copies
        // retain all numeric state and have independent allocation ownership.
        let mut copy = Self::new(
            self.kind,
            self.start,
            self.particles.len(),
            self.rate,
            [0.; 4],
            budget,
        )?;
        copy.parameters = self.parameters;
        copy.max = self.max;
        copy.count = self.count;
        copy.accumulated = self.accumulated;
        copy.particles.copy_from_slice(&self.particles);
        copy.next_free = self.next_free;
        Ok(copy)
    }
    pub fn kill_oldest(&mut self) -> bool {
        let Some((index, _)) = self
            .particles
            .iter()
            .enumerate()
            .filter(|(_, p)| p.age >= 0)
            .max_by_key(|(i, p)| (p.age, std::cmp::Reverse(*i)))
        else {
            return false;
        };
        self.particles[index].age = -1;
        self.next_free = self.next_free.min(index);
        self.count -= 1;
        true
    }
    pub fn advance(&mut self, tick: i32) -> usize {
        let dt = f64::from(tick);
        let cx = self.get(0xd8);
        let cy = self.get(0xe0);
        for (index, p) in self
            .particles
            .iter_mut()
            .enumerate()
            .filter(|(_, p)| p.age >= 0)
        {
            p.age = p.age.wrapping_add(tick);
            p.magnify += p.magnify_delta * dt;
            p.angle += p.angle_delta * dt;
            p.opacity += p.opacity_delta * dt;
            match self.kind {
                1 => {
                    p.x += p.motion[0] * dt + 0.5 * p.motion[2] * dt * dt;
                    p.y += p.motion[1] * dt + 0.5 * p.motion[3] * dt * dt;
                    p.motion[0] += p.motion[2] * dt;
                    p.motion[1] += p.motion[3] * dt;
                }
                2 | 4 => {
                    p.motion[0] += p.motion[1] * dt;
                    p.motion[2] += p.motion[3] * dt;
                    let (sin, cos) = p.motion[0].sin_cos();
                    p.x = cx + cos * p.motion[2];
                    p.y = cy - sin * p.motion[2];
                    if self.kind == 4 {
                        p.motion[1] = (p.motion[1] + p.motion[5] * dt).min(p.motion[4]);
                    }
                }
                3 => {
                    if p.opacity < 0. {
                        p.motion[1] -= 1.;
                        p.opacity = 0.;
                        p.opacity_delta = -p.opacity_delta;
                    } else if p.opacity >= p.motion[0] {
                        p.opacity = p.motion[0];
                        p.opacity_delta = -p.opacity_delta;
                    }
                }
                _ => unreachable!(),
            }
            // All four original manager vtables share this predicate. Blink
            // counters are retained even though this DLL does not inspect them.
            if !(p.magnify > 0. && p.opacity > 0.) {
                p.age = -1;
                self.next_free = self.next_free.min(index);
                self.count -= 1;
            }
        }
        self.accumulated += dt * self.rate;
        let amount = self.accumulated.trunc();
        if amount.abs() < 1. {
            return 0;
        }
        self.accumulated -= amount;
        if amount < 0. {
            for _ in 0..((-amount) as usize).min(self.count) {
                self.kill_oldest();
            }
            0
        } else {
            (amount as usize).min(self.max.saturating_sub(self.count))
        }
    }
    pub fn seed_common(&self, rng: &mut u32) -> Particle {
        let magnify = self.range(0x58, rng);
        let magnify_delta = self.range(0x70, rng);
        let angle = self.range(0x20, rng);
        let mut angle_delta = self.range(0x38, rng);
        if self.get(0x50) != 0. && random(rng) > 0.5 {
            angle_delta = -angle_delta;
        }
        Particle {
            magnify,
            magnify_delta,
            angle,
            angle_delta,
            opacity: self.range(0x88, rng),
            opacity_delta: self.range(0xa0, rng),
            ..Particle::default()
        }
    }
    pub fn spawn(
        &mut self,
        rng: &mut u32,
        images: usize,
        custom: Option<&[f64]>,
        blink_common: Option<Particle>,
    ) -> bool {
        if self.count >= self.max {
            return false;
        }
        let Some(offset) = self.particles[self.next_free..]
            .iter()
            .position(|p| p.age < 0)
        else {
            return false;
        };
        let index = self.next_free + offset;
        let mut p = Particle {
            magnify: 1.,
            ..Particle::default()
        };
        if self.kind == 3 {
            // Blink consumes the common random values before the script hook,
            // then replaces the fields in its own initializer.
            let _common = blink_common.unwrap_or_else(|| self.seed_common(rng));
            let (x, y, max, speed, count) = if let Some(v) = custom {
                (v[0], v[1], v[2], v[3], v[4])
            } else {
                let time = self.range(0xf8, rng);
                let max = self.range(0x88, rng);
                let x = random(rng);
                random(rng);
                let y = random(rng);
                (
                    self.get(0xd8) + self.get(0xe8) * x,
                    self.get(0xe0) + self.get(0xf0) * y,
                    max,
                    2. * max / time,
                    self.get(0x118) + self.get(0x120) * y,
                )
            };
            p.x = x;
            p.y = y;
            p.opacity_delta = speed;
            p.motion[0] = f64::from(max as i32);
            p.motion[1] = f64::from(count as i32);
        } else {
            match (self.kind, custom) {
                (1, Some(v)) => {
                    p.x = v[0];
                    p.y = v[1];
                    p.motion[..4].copy_from_slice(&v[3..7]);
                }
                (1, None) => {
                    let a = self.range(0x128, rng);
                    let speed = self.range(0xf8, rng);
                    let (sin, cos) = a.sin_cos();
                    p.motion[0] = cos * speed;
                    p.motion[1] = -sin * speed;
                    let a = self.range(0x140, rng);
                    let accel = self.range(0x110, rng);
                    let (sin, cos) = a.sin_cos();
                    p.motion[2] = cos * accel;
                    p.motion[3] = -sin * accel;
                    // The optimized original discards two draws and uses the
                    // third for both coordinates and its temporary opacity.
                    random(rng);
                    random(rng);
                    let xy = random(rng);
                    p.x = self.get(0xd8) + self.get(0xe8) * xy;
                    p.y = self.get(0xe0) + self.get(0xf0) * xy;
                }
                (2 | 4, Some(v)) => {
                    p.motion[..4].copy_from_slice(&v[3..7]);
                    if self.kind == 4 {
                        p.motion[4] = v[4];
                        p.motion[1] = 0.;
                        p.motion[5] = v[7];
                    }
                }
                (2 | 4, None) => {
                    p.motion[0] = random(rng) * TAU;
                    let mut omega = self.range(0x118, rng);
                    if self.get(0x130) != 0. && random(rng) > 0.5 {
                        omega = -omega;
                    }
                    if self.kind == 4 {
                        let time = self.range(0x138, rng);
                        p.motion[4] = omega;
                        p.motion[5] = omega / time;
                    } else {
                        p.motion[1] = omega;
                    }
                    p.motion[3] = self.range(0x100, rng);
                    p.motion[2] = self.range(0xe8, rng);
                    self.range(0x88, rng);
                }
                _ => unreachable!(),
            }
            let common = self.seed_common(rng);
            p.magnify = common.magnify;
            p.magnify_delta = common.magnify_delta;
            p.angle = common.angle;
            p.angle_delta = common.angle_delta;
            p.opacity = common.opacity;
            p.opacity_delta = common.opacity_delta;
        }
        p.image = (random(rng) * images as f64) as usize;
        self.particles[index] = p;
        self.next_free = index + 1;
        self.count += 1;
        true
    }
}
pub(super) fn random(state: &mut u32) -> f64 {
    *state = state.wrapping_mul(0x7d2b89dd).wrapping_add(1);
    f64::from(*state) * (1. / 4294967296.)
}
