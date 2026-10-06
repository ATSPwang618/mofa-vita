//! Bounded whole-canvas blurs can overwrite unique storage a strip at a time.
//! A bounded original-pixel history protects the next strip's halo, including
//! the extra overlap introduced by logical-to-display nearest sampling.
use crate::{Gpu, Image, Result, drawing::Draw, image::Plane};
use krkr_protocol::graphics::{Rect, Size};
use std::rc::Rc;

pub(crate) struct Plan {
    area: Rect,
    // Stored columns needed by the output plus its horizontal kernel halo.
    left: u32,
    width: u32,
    band: u32,
    guard: u32,
    history: u32,
    pub bytes: usize,
}

fn stored_rows(start: u32, end: u32, logical: u32, stored: u32) -> (u32, u32) {
    if logical == stored {
        return (start, end);
    }
    // Include a stored texel on each side for nearest-coordinate rounding.
    let low = (u64::from(start) * u64::from(stored) / u64::from(logical)) as u32;
    let high = (u64::from(end) * u64::from(stored)).div_ceil(u64::from(logical)) as u32;
    (low.saturating_sub(1), high.saturating_add(1).min(stored))
}

fn stored_row_capacity(rows: u32, logical: u32, stored: u32) -> u32 {
    if logical == stored {
        rows
    } else {
        ((u64::from(rows) * u64::from(stored)).div_ceil(u64::from(logical)) as u32)
            .saturating_add(3)
            .min(stored)
    }
}

impl Gpu {
    pub(crate) fn stream_blur_plan(
        &self,
        image: &Image,
        area: Rect,
        radius: [u32; 2],
    ) -> Option<Plan> {
        self.stream_blur_plan_with_capacity(
            image,
            area,
            radius,
            self.scratch_capacity()
                .min(self.capacity_after_collect(&self.resident)),
        )
    }

    /// Optional headroom to avoid repeatedly gathering the same halo for one
    /// or two output rows. Failure to reclaim it keeps the smaller valid plan.
    pub fn blur_preferred_headroom(
        &self,
        image: &Image,
        area: Rect,
        radius: [u32; 2],
    ) -> Option<usize> {
        let current = self.stream_blur_plan(image, area, radius)?;
        if current.band >= 32 {
            return None;
        }
        let capacity = (8 * 1024 * 1024).min(self.scratch.limit());
        let preferred = self.stream_blur_plan_with_capacity(image, area, radius, capacity)?;
        (preferred.band >= 32 && preferred.bytes <= capacity && preferred.bytes > current.bytes)
            .then_some(preferred.bytes)
    }

    fn stream_blur_plan_with_capacity(
        &self,
        image: &Image,
        area: Rect,
        radius: [u32; 2],
        available: usize,
    ) -> Option<Plan> {
        let kernel = (u64::from(radius[0]) * 2 + 1).checked_mul(u64::from(radius[1]) * 2 + 1)?;
        let plane = image.main.as_ref()?;
        if !image.canvas
            || area != image.size.rect()
            || radius.iter().any(|&r| r > 64)
            || radius == [0, 0]
            || Rc::strong_count(plane) != 1
            || plane.size != self.box_output_size(image, area)
        {
            return None;
        }
        let output = plane.size.rgba_bytes()?;
        // Transparent black stays exactly zero for either alpha convention.
        // Keep margins beyond the kernel footprint virtual, including the
        // logical/storage sampling guard around the first nonzero tile.
        let guard_x = image
            .size
            .width
            .div_ceil(plane.size.width)
            .saturating_add(1);
        let guard_y = image
            .size
            .height
            .div_ceil(plane.size.height)
            .saturating_add(1);
        let active = plane
            .tiles
            .iter()
            .filter(|t| t.texture.solid_color() != Some(0))
            .map(|t| t.rectangle)
            .reduce(|a, b| crate::scene_damage::union(Some(a), b))?;
        let axis = |start: u32, length: u32, logical: u32, stored: u32, radius: u32, guard: u32| {
            let low = (u64::from(start) * u64::from(logical) / u64::from(stored)) as u32;
            let high =
                (u64::from(start + length) * u64::from(logical)).div_ceil(u64::from(stored)) as u32;
            (
                low.saturating_sub(radius.saturating_add(guard)),
                high.saturating_add(radius.saturating_add(guard))
                    .min(logical),
            )
        };
        let (left, right) = axis(
            active.left as u32,
            active.width,
            image.size.width,
            plane.size.width,
            radius[0],
            guard_x,
        );
        let (top, bottom) = axis(
            active.top as u32,
            active.height,
            image.size.height,
            plane.size.height,
            radius[1],
            guard_y,
        );
        let active = Rect {
            left: left as i32,
            top: top as i32,
            width: right - left,
            height: bottom - top,
        };
        let (window_left, window_right) = stored_rows(
            left.saturating_sub(radius[0].saturating_add(guard_x)),
            right
                .saturating_add(radius[0].saturating_add(guard_x))
                .min(image.size.width),
            image.size.width,
            plane.size.width,
        );
        let window_width = window_right - window_left;
        let physical = crate::scene::raster::Raster::new(image.size, plane.size, (0, 0))
            .ok()?
            .rect(active)?;
        let parts: usize = plane
            .tiles
            .iter()
            .map(|t| {
                if t.backing.is_some() || t.texture.size == t.size() {
                    return 1;
                }
                t.rectangle.intersection(physical).map_or(1, |r| {
                    1 + usize::from(r.left > t.rectangle.left)
                        + usize::from(r.top > t.rectangle.top)
                        + usize::from(
                            (r.left as i64 + r.width as i64)
                                < t.rectangle.left as i64 + t.rectangle.width as i64,
                        )
                        + usize::from(
                            (r.top as i64 + r.height as i64)
                                < t.rectangle.top as i64 + t.rectangle.height as i64,
                        )
                })
            })
            .sum();
        let expand = plane
            .tiles
            .iter()
            .filter(|t| t.backing.is_none() && t.texture.size != t.size())
            .filter_map(|t| {
                t.rectangle.intersection(physical).map(|r| {
                    if parts <= crate::canvas_tiles::MAX_WRITE_REGIONS {
                        r
                    } else {
                        t.rectangle
                    }
                })
            })
            .try_fold(0usize, |n, r| {
                n.checked_add(
                    (r.width as usize)
                        .checked_mul(r.height as usize)?
                        .checked_mul(4)?,
                )
            })?;
        // Shared tiles need copy-on-write, not a second copy of every tile in
        // the image. Detach only the active region before streaming its halos.
        let detach = plane
            .tiles
            .iter()
            .filter(|t| {
                (t.backing.is_some() || t.texture.size == t.size())
                    && (!t.renderable() || Rc::strong_count(&t.texture) > 1)
                    && t.rectangle.intersection(physical).is_some()
            })
            .try_fold(0usize, |n, t| n.checked_add(t.size().rgba_bytes()?))?;
        if output < 256 * 1024 || (output < 4 * 1024 * 1024 && output <= self.resident.available())
        {
            return None;
        }
        let guard = image
            .size
            .height
            .div_ceil(plane.size.height)
            .saturating_add(1);
        let ry = radius[1].min(image.size.height - 1);
        let history = ry
            .saturating_add(guard.saturating_mul(2))
            .min(image.size.height);
        let history = stored_row_capacity(history, image.size.height, plane.size.height);
        let estimate = |band: u32| {
            let rows = band
                .saturating_add(ry.saturating_add(guard).saturating_mul(2))
                .min(image.size.height);
            let window = Size {
                width: window_width,
                height: stored_row_capacity(rows, image.size.height, plane.size.height),
            }
            .rgba_bytes()?;
            let saved = Size {
                width: window_width,
                height: history,
            }
            .rgba_bytes()?;
            let gather = Size {
                width: image
                    .size
                    .width
                    .min(active.width.min(self.tile_edge.min(256)) + radius[0] * 2),
                height: rows.min(self.tile_edge.min(256) + radius[1] * 2),
            }
            .rgba_bytes()?;
            window
                .checked_add(saved)?
                // Packed kernels keep two RGBA horizontal sum surfaces in
                // addition to the gathered pixels. Each is no bigger than
                // the gather; account for all three before mutating pixels.
                .checked_add(gather.checked_mul(if kernel <= 81 { 1 } else { 3 })?)?
                .checked_add(expand)?
                .checked_add(detach)
        };
        // Amortize halo preparation and target switches across up to one
        // kernel block. Keep scratch proportional to the streamed window,
        // not a second full canvas, and shrink before any pixels are changed.
        let mut band = image.size.height.min(self.tile_edge.min(256));
        while band > 1 && estimate(band)? > available {
            band = band.div_ceil(2);
        }
        let bytes = estimate(band)?;
        // A one-row / very sparse compact image may need more halo storage
        // than a regular output. Keep that cheaper out-of-place path.
        if bytes >= output && output <= available {
            return None;
        }
        Some(Plan {
            area: active,
            left: window_left,
            width: window_width,
            band,
            guard,
            history,
            bytes,
        })
    }

    fn blur_window(&self, size: Size) -> Result<Image> {
        Ok(Image {
            size,
            canvas: false,
            text: false,
            device: self.device.clone(),
            province: None,
            main: Some(self.overwrite_plane(size, &self.scratch)?),
        })
    }

    pub(crate) fn stream_box_blur(
        &self,
        image: &mut Image,
        radius: [u32; 2],
        alpha: bool,
        plan: Plan,
    ) -> Result<()> {
        let _profile = krkr_protocol::profile::span_detail("gpu.blur.stream", || {
            format!(
                "band={} bytes={} columns={}..{}",
                plan.band,
                plan.bytes,
                plan.left,
                plan.left + plan.width
            )
        });
        // Reject insufficient scratch before changing the original image.
        if plan.bytes > self.scratch.available() {
            self.collect()?;
        }
        drop(self.scratch.reserve(plan.bytes)?);
        let size = image.size;
        // Expand virtual constant bands and detach shared active tiles. Unique
        // nonuniform tiles remain in place; aliases retain their original data.
        self.writable_compact(image, plan.area, false)?;
        let stored = image.plane(false)?.size;
        let ry = radius[1].min(size.height - 1);
        let history = self.blur_window(Size {
            width: plan.width,
            height: plan.history,
        })?;
        let mut saved: Option<Rect> = None;
        let kernel = (u64::from(radius[0]) * 2 + 1) * (u64::from(radius[1]) * 2 + 1);
        let rows = plan
            .band
            .saturating_add((ry + plan.guard) * 2)
            .min(size.height);
        let window = self.blur_window(Size {
            width: plan.width,
            height: stored_row_capacity(rows, size.height, stored.height),
        })?;
        let mut input = crate::box_blur::BoxInput::default();
        for y in (0..size.height).step_by(plan.band as usize) {
            let end = (y + plan.band).min(size.height);
            let Some(output) = (Rect {
                left: 0,
                top: y as i32,
                width: size.width,
                height: end - y,
            })
            .intersection(plan.area) else {
                continue;
            };
            let top = y.saturating_sub(ry + plan.guard);
            let bottom = end.saturating_add(ry + plan.guard).min(size.height);
            let (top, bottom) = stored_rows(top, bottom, size.height, stored.height);
            let window_size = Size {
                width: plan.width,
                height: bottom - top,
            };
            // Reuse the admitted maximum strip. Fractional density can change
            // its used height by one row even away from the image borders.
            let original = image.plane(false)?;
            // The saved halo replaces the top rows with their original pixels.
            // Do not first read those rows from the already modified canvas.
            // Both writes are disjoint and together initialize the whole window.
            let first = saved.map_or(top, |saved| {
                (saved.top as u32 + saved.height).clamp(top, bottom)
            });
            let fresh = Rect {
                left: 0,
                top: (first - top) as i32,
                width: window_size.width,
                height: bottom - first,
            };
            // Preserve stored texels. The blur shader reconstructs logical
            // neighbors, so neither the window nor the history needs expansion.
            self.draw(
                window.plane(false)?,
                Some(original),
                fresh,
                &Draw::copy([1., 0., plan.left as f32, 0., 1., top as f32], [true; 4]),
            )?;
            if let Some(saved) = saved {
                self.draw(
                    window.plane(false)?,
                    Some(history.plane(false)?),
                    Rect {
                        left: 0,
                        top: saved.top - top as i32,
                        width: plan.width,
                        height: saved.height,
                    },
                    &Draw::copy(
                        [1., 0., 0., 0., 1., top as f32 - saved.top as f32],
                        [true; 4],
                    ),
                )?;
            }
            if end < size.height {
                let start = end.saturating_sub(ry + plan.guard);
                let stop = end.saturating_add(plan.guard).min(size.height);
                let (start, stop) = stored_rows(start, stop, size.height, stored.height);
                self.draw(
                    history.plane(false)?,
                    Some(window.plane(false)?),
                    Rect {
                        left: 0,
                        top: 0,
                        width: plan.width,
                        height: stop - start,
                    },
                    &Draw::copy([1., 0., 0., 0., 1., (start - top) as f32], [true; 4]),
                )?;
                saved = Some(Rect {
                    left: 0,
                    top: start as i32,
                    width: plan.width,
                    height: stop - start,
                });
            }
            // Present saved input in the original stored coordinate system.
            // Every sampled halo is inside this window. The output can now go
            // directly to the unique canvas, without blurring unused halo rows
            // into another window and then copying them back to the canvas.
            let plane = window.plane(false)?;
            let mut tiles: Vec<_> = plane
                .tiles
                .iter()
                .filter_map(|tile| {
                    let part = tile.rectangle.intersection(window_size.rect())?;
                    Some(tile.cropped(part))
                })
                .collect();
            for tile in &mut tiles {
                tile.rectangle.left += plan.left as i32;
                tile.rectangle.top += top as i32;
                if let Some(backing) = &mut tile.backing {
                    backing.left += plan.left as i32;
                    backing.top += top as i32;
                }
            }
            let source = Image {
                size,
                canvas: false,
                text: false,
                device: self.device.clone(),
                province: None,
                main: Some(Rc::new(Plane {
                    size: stored,
                    budget: plane.budget.clone(),
                    tiles,
                })),
            };
            if kernel > 81 {
                self.packed_box_blur_into(
                    &source,
                    image,
                    output,
                    radius,
                    alpha,
                    kernel < 256,
                    &mut input,
                )?;
            } else {
                self.small_box_blur_into(&source, image, output, radius, alpha, &mut input)?;
            }
        }
        Ok(())
    }
}
