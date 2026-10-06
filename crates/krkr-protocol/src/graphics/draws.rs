//! A host-prepared, bounded write batch. Its storage is reserved before any
//! client call can finish without a host round trip.
use super::{BlendOptions, Color, Command, DrawFace, Fill, ImageRef, Rect, Size};
use crate::budget::{Budget, Permit};
use std::sync::Arc;

pub const DRAW_BATCH_CAPACITY: usize = 128;

#[derive(Clone, Copy, Debug)]
pub enum PreparedDraw {
    Fill(Fill),
    Color(Color),
    Copy {
        rectangle: Rect,
        x: i32,
        y: i32,
        clip: Rect,
        face: DrawFace,
        hold_alpha: bool,
    },
    Operate {
        rectangle: Rect,
        x: i32,
        y: i32,
        clip: Rect,
        options: BlendOptions,
    },
}

#[derive(Debug)]
pub struct PreparedDraws {
    image: ImageRef,
    size: Size,
    source: Option<ImageRef>,
    blend: bool,
    colors: Option<(Size, Vec<Rect>)>,
    draws: Vec<PreparedDraw>,
    permit: Arc<Permit>,
    extra_bytes: usize,
    lifetime: Arc<()>,
}
impl PreparedDraws {
    pub fn reserve(
        image: ImageRef,
        size: Size,
        source: Option<ImageRef>,
        blend: bool,
        gpu_bytes: usize,
        budget: &Budget,
    ) -> Option<Self> {
        let bytes =
            gpu_bytes.checked_add(DRAW_BATCH_CAPACITY * std::mem::size_of::<PreparedDraw>())?;
        let permit = Arc::new(budget.reserve(bytes).ok()?);
        let mut draws = Vec::new();
        draws.try_reserve_exact(DRAW_BATCH_CAPACITY).ok()?;
        Some(Self {
            image,
            size,
            source,
            blend,
            colors: None,
            draws,
            permit,
            extra_bytes: gpu_bytes,
            lifetime: Arc::new(()),
        })
    }
    pub fn image(&self) -> &ImageRef {
        &self.image
    }
    /// Disjoint physical regions already writable without allocation. Color-only
    /// grants cannot change their tile representation through a fill/copy.
    pub fn allow_colors(mut self, stored: Size, regions: Vec<Rect>) -> Option<Self> {
        if self.size.width == 0 || self.size.height == 0 || stored.width == 0 || stored.height == 0
        {
            return None;
        }
        if regions.capacity() * std::mem::size_of::<Rect>() > self.extra_bytes {
            return None;
        }
        self.colors = Some((stored, regions));
        Some(self)
    }
    fn writable_color(&self, rectangle: Rect) -> bool {
        let Some((stored, regions)) = &self.colors else {
            return false;
        };
        let Some(r) = rectangle.intersection(self.size.rect()) else {
            return true;
        };
        let edge = |at: i64, logical: u32, physical: u32| {
            let n = 2 * i128::from(at) * i128::from(physical) - i128::from(logical);
            (-(-n).div_euclid(2 * i128::from(logical))).clamp(0, i128::from(physical)) as i32
        };
        let left = edge(i64::from(r.left), self.size.width, stored.width);
        let top = edge(i64::from(r.top), self.size.height, stored.height);
        let right = edge(
            i64::from(r.left) + i64::from(r.width),
            self.size.width,
            stored.width,
        );
        let bottom = edge(
            i64::from(r.top) + i64::from(r.height),
            self.size.height,
            stored.height,
        );
        let area = Rect {
            left,
            top,
            width: (right - left) as u32,
            height: (bottom - top) as u32,
        };
        let covered: u64 = regions
            .iter()
            .filter_map(|r| area.intersection(*r))
            .map(|r| u64::from(r.width) * u64::from(r.height))
            .sum();
        covered == u64::from(area.width) * u64::from(area.height)
    }
    pub fn size(&self) -> Size {
        self.size
    }
    pub fn draws(&self) -> &[PreparedDraw] {
        &self.draws
    }
    pub fn permit(&self) -> Arc<Permit> {
        self.permit.clone()
    }
    pub fn lifetime(&self) -> std::sync::Weak<()> {
        Arc::downgrade(&self.lifetime)
    }
    pub fn source(&self) -> Option<&ImageRef> {
        self.source.as_ref()
    }
    pub(crate) fn push(&mut self, command: &Command) -> bool {
        if self.full() {
            return false;
        }
        let (image, draw) = match command {
            Command::Color {
                image,
                rectangle,
                color,
                opacity,
                face,
            } if self.writable_color(*rectangle)
                && matches!(
                    face,
                    DrawFace::Alpha | DrawFace::Opaque | DrawFace::AddAlpha
                )
                && (-254..=254).contains(opacity)
                && !(*face == DrawFace::AddAlpha && *opacity < 0) =>
            {
                (
                    image,
                    PreparedDraw::Color(Color {
                        rectangle: *rectangle,
                        color: *color,
                        opacity: *opacity,
                        face: *face,
                    }),
                )
            }
            Command::Fill { image, fills } => {
                if self.colors.is_some() {
                    return false;
                }
                let [fill] = fills.as_slice() else {
                    return false;
                };
                if fill.face == DrawFace::Province {
                    return false;
                }
                (image, PreparedDraw::Fill(*fill))
            }
            Command::Copy {
                image,
                source,
                rectangle,
                x,
                y,
                clip,
                face,
                hold_alpha,
            } if self.colors.is_none()
                && *face != DrawFace::Province
                && self.source.as_ref().is_some_and(|s| s.id == source.id) =>
            {
                (
                    image,
                    PreparedDraw::Copy {
                        rectangle: *rectangle,
                        x: *x,
                        y: *y,
                        clip: *clip,
                        face: *face,
                        hold_alpha: *hold_alpha,
                    },
                )
            }
            Command::Operate {
                image,
                source,
                rectangle,
                x,
                y,
                clip,
                options,
            } if self.colors.is_none()
                && self.blend
                && options.accepts_face()
                && self.source.as_ref().is_some_and(|s| s.id == source.id) =>
            {
                (
                    image,
                    PreparedDraw::Operate {
                        rectangle: *rectangle,
                        x: *x,
                        y: *y,
                        clip: *clip,
                        options: *options,
                    },
                )
            }
            _ => return false,
        };
        if image.id != self.image.id {
            return false;
        }
        self.draws.push(draw);
        true
    }
    pub(crate) fn full(&self) -> bool {
        self.draws.len() == DRAW_BATCH_CAPACITY
    }
    pub(crate) fn payload_bytes(&self) -> usize {
        self.draws.capacity() * std::mem::size_of::<PreparedDraw>()
            + self
                .colors
                .as_ref()
                .map_or(0, |(_, r)| r.capacity() * std::mem::size_of::<Rect>())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn color_scope_uses_physical_pixel_centers_and_rejects_representation_changes() {
        let mut ids = slotmap::SlotMap::with_key();
        let image = ImageRef {
            id: ids.insert(()),
            lifetime: Arc::default(),
        };
        let logical = Size {
            width: 101,
            height: 41,
        };
        let stored = Size {
            width: 80,
            height: 32,
        };
        let mut batch = PreparedDraws::reserve(
            image.clone(),
            logical,
            None,
            false,
            DRAW_BATCH_CAPACITY * std::mem::size_of::<Color>(),
            &Budget::new(64 * 1024),
        )
        .unwrap()
        .allow_colors(
            stored,
            vec![
                Rect {
                    left: 0,
                    top: 0,
                    width: 20,
                    height: 32,
                },
                Rect {
                    left: 60,
                    top: 0,
                    width: 20,
                    height: 32,
                },
            ],
        )
        .unwrap();
        let color = |left, width, opacity| Command::Color {
            image: image.clone(),
            rectangle: Rect {
                left,
                top: 0,
                width,
                height: 41,
            },
            color: 0xff123456,
            opacity,
            face: DrawFace::Alpha,
        };
        assert!(batch.push(&color(0, 25, 128)));
        assert!(batch.push(&color(76, 25, -24)));
        assert!(
            !batch.push(&color(24, 2, 128)),
            "cannot touch an unmaterialized gap"
        );
        assert!(!batch.push(&color(0, 25, 255)), "clear may change storage");
        assert!(!batch.push(&Command::Fill {
            image,
            fills: vec![Fill {
                rectangle: logical.rect(),
                color: 0,
                face: DrawFace::Alpha,
                hold_alpha: false,
            }]
        }));
        assert_eq!(batch.draws().len(), 2);
    }
}
