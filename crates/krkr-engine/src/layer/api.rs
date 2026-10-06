//! Shared geometry, tree and image operations behind the native bindings.
use super::{
    bindings::{State, integer, layer_id, size},
    tasks::{Change, request},
    *,
};
use krkr_protocol::graphics::{Command, Fill};
use tjs_core::{NativeCx, NativeStep};
struct ImageLoad {
    shared: Shared,
    id: LayerId,
    owner: ObjId,
}
impl Trace for ImageLoad {
    fn trace(&self, visit: &mut dyn FnMut(Value)) {
        self.owner.trace(visit);
    }
}
impl State {
    pub(super) fn adjust(
        &self,
        operation: krkr_protocol::graphics::Adjustment,
    ) -> NativeResult<NativeStep> {
        let image = self.image()?;
        let rectangle = self.read(|r| {
            if matches!(operation, krkr_protocol::graphics::Adjustment::Flip { .. }) {
                r.geometry.image_size.rect()
            } else {
                r.geometry.clip
            }
        })?;
        self.command(
            Command::Adjust {
                image,
                rectangle,
                operation,
            },
            Change::None,
        )
    }
    pub(super) fn require_main(&self) -> NativeResult<()> {
        self.read(|r| r.image().map(|_| ()))?
    }
    pub(super) fn image(&self) -> NativeResult<ImageRef> {
        self.read(|r| r.image().cloned())?
    }
    pub(super) fn load(
        &self,
        cx: &mut NativeCx<'_>,
        name: Value,
        key: u32,
        province: bool,
    ) -> NativeResult<NativeStep> {
        self.require_main()?;
        let Value::Str(name) = tjs_core::value::to_string(cx.heap_mut(), name)? else {
            unreachable!()
        };
        let name = tjs_core::string::c_string(cx.heap().string(name)?).to_vec();
        let lease = self.lease()?;
        let budget = lease.shared.borrow().host()?.staging_budget();
        let size = if province {
            Some(self.read(|r| r.geometry.image_size)?)
        } else {
            None
        };
        let options = crate::storages::image::Options {
            name,
            key,
            size,
            grayscale: false,
            budget,
        };
        let state = ImageLoad {
            shared: lease.shared.clone(),
            id: lease.id,
            owner: self.read(|r| r.owner)?,
        };
        crate::storages::image::request(cx, options, state, |state, _, request| {
            state.shared.borrow().record(state.id)?.image()?;
            super::loading::start(&state.shared, state.id, request)
        })
    }
    pub(super) fn lease(&self) -> NativeResult<&Lease> {
        self.lease.as_ref().ok_or(NativeError::This)
    }
    pub(super) fn read<T>(&self, read: impl FnOnce(&Record) -> T) -> NativeResult<T> {
        let lease = self.lease()?;
        Ok(read(lease.shared.borrow().record(lease.id)?))
    }
    pub(super) fn update(
        &self,
        update: impl FnOnce(&mut Record) -> NativeResult<()>,
    ) -> NativeResult<()> {
        self.update_if_changed(|record| {
            update(record)?;
            Ok(true)
        })
    }
    /// Property setters validate normally, but equal values need no scene
    /// publication or self-updating transition refresh. Explicit update/paint
    /// requests continue to use update(), even when metadata is unchanged.
    pub(super) fn update_if_changed(
        &self,
        update: impl FnOnce(&mut Record) -> NativeResult<bool>,
    ) -> NativeResult<()> {
        let lease = self.lease()?;
        let mut world = lease.shared.borrow_mut();
        let record = world.record_mut(lease.id)?;
        if !update(record)? {
            return Ok(());
        }
        let window = record.window;
        world.changed(window);
        Ok(())
    }
    pub(super) fn command(&self, command: Command, change: Change) -> NativeResult<NativeStep> {
        let lease = self.lease()?;
        request(&lease.shared, lease.id, command, change, None)
    }
    pub(super) fn geometry(
        &self,
        mut geometry: Geometry,
        image_size: Size,
    ) -> NativeResult<NativeStep> {
        let (old, image, color) =
            self.read(|r| (r.geometry.image_size, r.image.clone(), r.neutral()))?;
        geometry.image_size = image_size;
        if old != image_size && self.read(|r| r.has_main)? {
            geometry.clip = image_size.rect();
            self.command(
                Command::Resize {
                    image: image.unwrap(),
                    size: image_size,
                    color,
                },
                Change::Geometry(geometry),
            )
        } else {
            let lease = self.lease()?;
            lease.shared.borrow_mut().set_geometry(lease.id, geometry)?;
            Ok(NativeStep::Return(Value::Void))
        }
    }
    pub(super) fn change_size(
        &self,
        width: i32,
        height: i32,
        image: bool,
    ) -> NativeResult<NativeStep> {
        if image {
            self.require_main()?;
        }
        let mut g = self.read(|r| r.geometry)?;
        let image_size = if image {
            let size = size(width, height)?;
            g.width = g.width.min(width);
            g.height = g.height.min(height);
            size
        } else {
            g.width = width;
            g.height = height;
            Size {
                width: g.image_size.width.max(width.max(0) as u32),
                height: g.image_size.height.max(height.max(0) as u32),
            }
        };
        g.image_left = g
            .image_left
            .max(g.width.saturating_sub(image_size.width as i32));
        g.image_top = g
            .image_top
            .max(g.height.saturating_sub(image_size.height as i32));
        self.geometry(g, image_size)
    }
    pub(super) fn move_to(&self, left: i32, top: i32) -> NativeResult<()> {
        let lease = self.lease()?;
        let primary = lease.shared.borrow().is_primary(lease.id);
        self.update_if_changed(|r| {
            if primary && (left != 0 || top != 0) {
                return Err(NativeError::Message("primary layer cannot be moved"));
            }
            if (r.geometry.left, r.geometry.top) == (left, top) {
                return Ok(false);
            }
            r.geometry.left = left;
            r.geometry.top = top;
            Ok(true)
        })
    }
    pub(super) fn pixel(
        &self,
        cx: &NativeCx<'_>,
        x: Value,
        y: Value,
        province: bool,
        mask: u32,
        shift: u32,
    ) -> NativeResult<NativeStep> {
        let image = if province {
            let Some(image) = self.read(|r| r.image.clone())? else {
                return Ok(NativeStep::Return(Value::Int(0)));
            };
            image
        } else {
            self.image()?
        };
        let x = integer(cx, x)?;
        let y = integer(cx, y)?;
        if mask == 255 && (province || shift == 24) {
            let lease = self.lease()?;
            let mut world = lease.shared.borrow_mut();
            let size = world.record(lease.id)?.geometry.image_size;
            if x >= 0 && y >= 0 && (x as u32) < size.width && (y as u32) < size.height {
                if let Some(plane) = world.hit_cache.get(&image, province) {
                    return Ok(NativeStep::Return(Value::Int(
                        plane.sample(x.into(), y.into()).into(),
                    )));
                }
                let pixels = u64::from(size.width) * u64::from(size.height);
                let host = world.host()?;
                if pixels <= 256 * 1024
                    && pixels * 8 <= host.staging_budget().available() as u64
                    && world.hit_cache.repeated_pixel(&image, province, pixels)
                {
                    let revision = image.lifetime.revision(province);
                    drop(world);
                    return self.command(
                        Command::ReadHitPlane {
                            image: image.clone(),
                            province,
                        },
                        Change::MaskPixel {
                            image,
                            province,
                            revision,
                            x,
                            y,
                        },
                    );
                }
            }
        }
        self.command(
            Command::Pixel {
                image,
                x,
                y,
                province,
            },
            Change::Pixel { mask, shift },
        )
    }
    pub(super) fn put_pixel(
        &self,
        cx: &NativeCx<'_>,
        x: Value,
        y: Value,
        color: Value,
        face: DrawFace,
    ) -> NativeResult<NativeStep> {
        let rect = Rect {
            left: integer(cx, x)?,
            top: integer(cx, y)?,
            width: 1,
            height: 1,
        };
        let color = integer(cx, color)? as u32;
        if face == DrawFace::Province && self.read(|r| r.image.is_none())? {
            let clip = self.read(|r| r.geometry.clip)?;
            let rectangle = rect.intersection(clip).unwrap_or(Rect {
                width: 0,
                height: 0,
                ..rect
            });
            return self.create_province(krkr_protocol::graphics::ProvinceOperation::Fill(Fill {
                rectangle,
                color,
                face,
                hold_alpha: false,
            }));
        }
        self.fill(rect, color, Some(face))
    }
    pub(super) fn fill(
        &self,
        rectangle: Rect,
        color: u32,
        face: Option<DrawFace>,
    ) -> NativeResult<NativeStep> {
        let (image, clip, resolved, hold_alpha) =
            self.read(|r| (r.image.clone(), r.geometry.clip, r.face(), r.hold_alpha))?;
        let hold_alpha = hold_alpha || face == Some(DrawFace::Opaque);
        let face = face.map_or(resolved, Ok)?;
        let color = if face == DrawFace::Opaque && hold_alpha {
            crate::color::actual(color)
        } else {
            color
        };
        if face != DrawFace::Province {
            self.require_main()?;
        }
        let Some(rectangle) = rectangle.intersection(clip) else {
            return Ok(NativeStep::Return(Value::Void));
        };
        let image = if let Some(image) = image {
            image
        } else {
            if color & 255 == 0 {
                return Ok(NativeStep::Return(Value::Void));
            }
            return self.create_province(krkr_protocol::graphics::ProvinceOperation::Fill(Fill {
                rectangle,
                color,
                face,
                hold_alpha,
            }));
        };
        self.command(
            Command::Fill {
                image,
                fills: vec![Fill {
                    rectangle,
                    color,
                    face,
                    hold_alpha,
                }],
            },
            Change::None,
        )
    }
    pub(super) fn reparent(&self, cx: &mut NativeCx<'_>, input: Value) -> NativeResult<NativeStep> {
        let parent = if matches!(input, Value::Obj(ObjRef { object: None, .. })) {
            None
        } else {
            Some(layer_id(cx.heap_mut(), input)?)
        };
        self.change_input(input::changes::Operation::Parent(parent))
    }
}
impl Layers {
    pub(super) fn validate_parent(&self, id: LayerId, parent: Option<LayerId>) -> NativeResult<()> {
        if self.is_primary(id) {
            return Err(NativeError::Message("primary layer cannot have a parent"));
        }
        self.validate_join(id, parent)
    }
    pub(super) fn validate_join(&self, id: LayerId, parent: Option<LayerId>) -> NativeResult<()> {
        let window = self.record(id)?.window;
        let mut ancestor = parent;
        let mut depth = 0;
        while let Some(ancestor_id) = ancestor {
            if ancestor_id == id {
                return Err(NativeError::Message(
                    "layer cannot become its own descendant",
                ));
            }
            let r = self.record(ancestor_id)?;
            if r.window != window {
                return Err(NativeError::Message(
                    "parent belongs to another primary layer",
                ));
            }
            ancestor = r.parent;
            depth += 1;
        }
        let mut stack = vec![(id, depth)];
        while let Some((id, depth)) = stack.pop() {
            if depth > 128 {
                return Err(NativeError::Message(
                    "layer nesting exceeds the renderer limit",
                ));
            }
            stack.extend(self.records[id].children.iter().map(|&id| (id, depth + 1)));
        }
        Ok(())
    }
    pub(super) fn attach(&mut self, id: LayerId, parent: Option<LayerId>) -> NativeResult<()> {
        let record = self.record(id)?;
        let window = record.window;
        let old = record.parent;
        if old == parent {
            return Ok(());
        }
        if let Some(old) = old.and_then(|id| self.records.get_mut(id)) {
            old.children.retain(|&child| child != id);
            old.children_dirty = true;
        }
        if let Some(parent) = parent {
            let r = self.record_mut(parent)?;
            r.children.push(id);
            r.children_dirty = true;
        }
        self.record_mut(id)?.parent = parent;
        if let Some(parent) = parent {
            self.joined_order(id, parent);
        }
        self.changed(window);
        Ok(())
    }
}
