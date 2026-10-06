//! Window presentation and its inverse input mapping share rounded pixel extents.
use crate::graphics::{Rect, Size};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Viewport {
    pub left: i32,
    pub top: i32,
    numer: i32,
    denom: i32,
    client_scale: (u32, u32),
}
impl Default for Viewport {
    fn default() -> Self {
        Self {
            left: 0,
            top: 0,
            numer: 1,
            denom: 1,
            client_scale: (1, 1),
        }
    }
}
impl Viewport {
    pub fn numer(self) -> i32 {
        self.numer
    }
    pub fn denom(self) -> i32 {
        self.denom
    }
    /// Host client fitting composes with the script zoom without forcing the
    /// product of two valid ratios back into the script's 32-bit API fields.
    pub fn with_client_scale(self, numer: u32, denom: u32) -> Self {
        Self {
            client_scale: (numer, denom),
            ..self
        }
    }
    pub fn zoom(self, numer: i32, denom: i32) -> Result<Self, &'static str> {
        if numer == 0 && denom == 0 {
            return Err("window zoom ratio 0/0 is undefined");
        }
        // Signed Euclidean reduction matches WindowFormUnit, including ratios
        // with zero or negative components. Widen to avoid MIN / -1 overflow.
        let (mut a, mut b) = (i64::from(numer), i64::from(denom));
        while b != 0 {
            (a, b) = (b, a % b);
        }
        Ok(Self {
            numer: (i64::from(numer) / a) as i32,
            denom: (i64::from(denom) / a) as i32,
            ..self
        })
    }
    pub fn destination(self, size: Size) -> Rect {
        let scale = |n: u32| {
            // The reference uses MulDiv, then enforces a one-pixel minimum.
            // MulDiv returns -1 on division by zero or signed result overflow.
            if self.denom == 0 || self.client_scale.1 == 0 {
                return 1;
            }
            let product = i128::from(n) * i128::from(self.numer) * i128::from(self.client_scale.0);
            let denom = i128::from(self.denom) * i128::from(self.client_scale.1);
            let magnitude =
                (product.unsigned_abs() + denom.unsigned_abs() / 2) / denom.unsigned_abs();
            if (product < 0) != (denom < 0) || magnitude > i32::MAX as u128 {
                1
            } else {
                magnitude.max(1) as u32
            }
        };
        Rect {
            left: self.left,
            top: self.top,
            width: scale(size.width),
            height: scale(size.height),
        }
    }
    pub fn to_layer(self, size: Size, point: (i32, i32)) -> (i32, i32) {
        let dest = self.destination(size);
        (
            narrow(
                (i64::from(point.0) - i64::from(dest.left)) * i64::from(size.width)
                    / i64::from(dest.width),
            ),
            narrow(
                (i64::from(point.1) - i64::from(dest.top)) * i64::from(size.height)
                    / i64::from(dest.height),
            ),
        )
    }
    pub fn to_window(self, size: Size, point: (i32, i32)) -> (i32, i32) {
        let dest = self.destination(size);
        (
            narrow(
                i64::from(point.0) * i64::from(dest.width) / i64::from(size.width.max(1))
                    + i64::from(dest.left),
            ),
            narrow(
                i64::from(point.1) * i64::from(dest.height) / i64::from(size.height.max(1))
                    + i64::from(dest.top),
            ),
        )
    }
    pub fn attention(self, size: Size, rect: Rect) -> Rect {
        let (left, top) = self.to_window(size, (rect.left, rect.top));
        let dest = self.destination(size);
        Rect {
            left,
            top,
            width: (u64::from(rect.width) * u64::from(dest.width) / u64::from(size.width.max(1)))
                .clamp(1, i32::MAX as u64) as u32,
            height: (u64::from(rect.height) * u64::from(dest.height)
                / u64::from(size.height.max(1)))
            .clamp(1, i32::MAX as u64) as u32,
        }
    }
}
fn narrow(value: i64) -> i32 {
    value.clamp(i32::MIN as i64, i32::MAX as i64) as i32
}
