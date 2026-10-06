//! Movie images use the existing ordered graphics queue and image allocator.
use super::*;
use krkr_protocol::{
    graphics::Command,
    pixels::VideoPixels,
    transform::*,
    window::{Command as WindowCommand, Ticket},
};
pub(crate) struct Plane {
    pub order: u64,
    pub window: WindowId,
    pub image: ImageRef,
    pub bounds: Rect,
    pub visible: bool,
}
pub(crate) struct Images {
    order: u64,
    pub source: ImageRef,
    display: Option<ImageRef>,
    size: Size,
    source_size: Option<Size>,
    display_size: Size,
}
pub(crate) fn layer(heap: &mut Heap, value: Value) -> NativeResult<LayerId> {
    bindings::layer_id(heap, value)
}
impl Layers {
    pub(crate) fn video_targets_position(
        &mut self,
        targets: [Option<LayerId>; 2],
        left: Option<i32>,
        top: Option<i32>,
    ) -> NativeResult<()> {
        for id in targets.into_iter().flatten() {
            let mut g = self.record(id)?.geometry;
            if let Some(left) = left {
                g.left = left;
            }
            if let Some(top) = top {
                g.top = top;
            }
            self.set_geometry(id, g)?;
        }
        Ok(())
    }
    pub(crate) fn video_targets_visible(
        &mut self,
        targets: [Option<LayerId>; 2],
        visible: bool,
    ) -> NativeResult<()> {
        for id in targets.into_iter().flatten() {
            let r = self.record_mut(id)?;
            r.visible = visible;
            let window = r.window;
            self.dirty.insert(window);
        }
        Ok(())
    }
    pub(crate) fn video_layer(&self, id: LayerId, window: WindowId) -> NativeResult<()> {
        if self.record(id)?.window != window {
            return Err(NativeError::Message(
                "movie layer belongs to another window",
            ));
        }
        Ok(())
    }
    pub(crate) fn video_open(
        &mut self,
        window: WindowId,
        size: Size,
        stored: Size,
    ) -> NativeResult<(Images, VecDeque<Ticket>)> {
        let host = self.host()?;
        self.movie_order += 1;
        let source = ImageRef {
            id: self.images.insert(()),
            lifetime: Arc::default(),
        };
        let mut tickets = VecDeque::new();
        match host.request(
            window,
            WindowCommand::Graphics(Command::Create {
                image: source.id,
                lifetime: Arc::downgrade(&source.lifetime),
                size: stored,
                color: 0xff000000,
            }),
        ) {
            Ok(ticket) => tickets.push_back(ticket),
            Err(error) => {
                self.images.remove(source.id);
                return Err(NativeError::Detail(error));
            }
        }
        Ok((
            Images {
                order: self.movie_order,
                source,
                display: None,
                size,
                source_size: None,
                display_size: size,
            },
            tickets,
        ))
    }
    pub(crate) fn video_close(&mut self, window: WindowId, images: Images) {
        self.movies.remove(&images.source.id);
        self.images.remove(images.source.id);
        if let Some(display) = images.display {
            self.images.remove(display.id);
        }
        self.dirty.insert(window);
    }
    pub(crate) fn video_visible(
        &mut self,
        window: WindowId,
        images: &Images,
        visible: bool,
        bounds: Rect,
    ) {
        if let Some(plane) = self.movies.get_mut(&images.source.id) {
            plane.visible = visible;
            plane.bounds = bounds;
        }
        self.dirty.insert(window);
    }
    pub(crate) fn video_frame(
        &mut self,
        window: WindowId,
        images: &mut Images,
        pixels: Option<VideoPixels>,
        targets: [Option<LayerId>; 2],
        overlay: Option<(Rect, bool)>,
    ) -> NativeResult<VecDeque<Ticket>> {
        let host = self.host()?;
        let mut tickets = VecDeque::new();
        let mut request = |command| -> NativeResult<()> {
            tickets.push_back(
                host.request(window, WindowCommand::Graphics(command))
                    .map_err(NativeError::Detail)?,
            );
            Ok(())
        };
        if let Some(pixels) = pixels {
            // Overlay bounds are logical coordinates, independent of decoder
            // storage. Let the renderer sample the compact converted frame at
            // that extent instead of creating a second, scaled RGBA image.
            let logical_size = overlay.map_or(images.size, |(bounds, _)| Size {
                width: bounds.width.max(1),
                height: bounds.height.max(1),
            });
            match pixels {
                VideoPixels::Yuv420(pixels) => request(Command::UploadYuv {
                    image: images.source.clone(),
                    pixels,
                    logical_size,
                })?,
                VideoPixels::Rgba(pixels) => {
                    request(if pixels.size == logical_size {
                        Command::Upload {
                            image: images.source.clone(),
                            pixels,
                        }
                    } else {
                        Command::UploadScaled {
                            image: images.source.clone(),
                            pixels,
                            logical_size,
                        }
                    })?;
                }
            }
            images.source_size = Some(logical_size);
        }
        // Geometry can arrive before the first due frame. The placeholder is
        // decoder-sized, so defer presentation rather than expanding black
        // storage into an otherwise unused full logical display allocation.
        let Some(source_size) = images.source_size else {
            return Ok(tickets);
        };
        if let Some((bounds, visible)) = overlay {
            let size = Size {
                width: bounds.width.max(1),
                height: bounds.height.max(1),
            };
            // Upload already produces a logical image. Publishing that image
            // directly avoids a second RGBA frame and a full-frame transform
            // when the movie bounds match it, including compact stored frames.
            let image = if size == source_size {
                images.source.clone()
            } else {
                if images.display.is_none() {
                    let display = ImageRef {
                        id: self.images.insert(()),
                        lifetime: Arc::default(),
                    };
                    if let Err(error) = request(Command::Create {
                        image: display.id,
                        lifetime: Arc::downgrade(&display.lifetime),
                        size,
                        color: 0xff000000,
                    }) {
                        self.images.remove(display.id);
                        return Err(error);
                    }
                    images.display = Some(display);
                    images.display_size = size;
                }
                let display = images.display.as_ref().unwrap();
                if images.display_size != size {
                    request(Command::Resize {
                        image: display.clone(),
                        size,
                        color: 0xff000000,
                    })?;
                    images.display_size = size;
                }
                request(Command::Transform {
                    image: display.clone(),
                    source: images.source.clone(),
                    rectangle: source_size.rect(),
                    transform: Transform::Stretch(StretchRect {
                        left: 0,
                        top: 0,
                        width: size.width as i32,
                        height: size.height as i32,
                    }),
                    sampling: Sampling {
                        filter: Filter::Linear,
                        sharpness: -1.0,
                        no_clip: false,
                    },
                    operation: ImageOperation::Copy { hold_alpha: false },
                    clip: size.rect(),
                    clear: None,
                })?;
                display.clone()
            };
            self.movies.insert(
                images.source.id,
                Plane {
                    order: images.order,
                    window,
                    image,
                    bounds,
                    visible,
                },
            );
        } else {
            for id in targets.into_iter().flatten() {
                let r = self.record(id)?;
                let (old, has_main, old_size) =
                    (r.image.clone(), r.has_main, r.geometry.image_size);
                let image = if let Some(image) = old {
                    if !has_main {
                        request(Command::EnableImage {
                            image: image.clone(),
                            source: Some(image.clone()),
                            size: images.size,
                            color: 0xff000000,
                        })?;
                    } else if old_size != images.size {
                        request(Command::Resize {
                            image: image.clone(),
                            size: images.size,
                            color: 0xff000000,
                        })?;
                    }
                    image
                } else {
                    let image = ImageRef {
                        id: self.images.insert(()),
                        lifetime: Arc::default(),
                    };
                    request(Command::Create {
                        image: image.id,
                        lifetime: Arc::downgrade(&image.lifetime),
                        size: images.size,
                        color: 0xff000000,
                    })?;
                    image
                };
                // Copy only the main plane, preserving Layer province data.
                // Each target keeps its own allocation for subsequent drawing.
                request(Command::Copy {
                    image: image.clone(),
                    source: images.source.clone(),
                    rectangle: images.size.rect(),
                    x: 0,
                    y: 0,
                    clip: images.size.rect(),
                    face: DrawFace::Opaque,
                    hold_alpha: false,
                })?;
                let r = self.record_mut(id)?;
                r.image = Some(image);
                r.has_main = true;
                r.geometry.image_size = images.size;
                r.image_modified = true;
                r.geometry.set_size(images.size);
                r.geometry.clip = images.size.rect();
                if self.is_primary(id) {
                    self.windows.borrow_mut().recheck_viewport(window);
                }
            }
        }
        self.dirty.insert(window);
        Ok(tickets)
    }
}
use std::collections::VecDeque;

#[cfg(test)]
mod tests {
    use super::*;
    use krkr_protocol::{
        budget::Budget,
        pixels::{Bytes, Yuv420, Yuv420Layout},
        window,
    };

    fn setup() -> (Shared, window::Host, WindowId) {
        let mut runtime = tjs_runtime::Runtime::new();
        let events = Rc::new(RefCell::new(crate::events::Events::new(32)));
        let windows = crate::window::install(
            &mut runtime.heap,
            crate::operations::Operations::new(32),
            events,
            Rc::new(tjs_runtime::clock::MonotonicClock::default()),
        )
        .unwrap();
        let (client, host) = window::channel(Default::default(), Arc::new(|| {}));
        windows.borrow_mut().host = Some(client);
        let layers = super::super::install(&mut runtime.heap, windows).unwrap();
        let mut ids = SlotMap::<WindowId, ()>::with_key();
        (layers, host, ids.insert(()))
    }

    fn frame(size: Size) -> VideoPixels {
        VideoPixels::Yuv420(Arc::new(Yuv420 {
            size,
            layout: Yuv420Layout::Nv12,
            data: Bytes::zeroed(
                Yuv420::byte_len(size).unwrap(),
                &Budget::new(Yuv420::byte_len(size).unwrap()),
            )
            .unwrap(),
        }))
    }

    fn drain(host: &window::Host) -> Vec<&'static str> {
        let mut commands = Vec::new();
        while let Some(request) = host.next_request() {
            commands.push(match &request.command {
                WindowCommand::Graphics(Command::Create { .. }) => "create",
                WindowCommand::Graphics(Command::Resize { .. }) => "resize",
                WindowCommand::Graphics(Command::UploadYuv { .. }) => "upload",
                WindowCommand::Graphics(Command::Transform { .. }) => "transform",
                _ => panic!("unexpected video command"),
            });
            request.respond(Ok(window::Response::Done));
        }
        commands
    }

    #[test]
    fn compact_movie_overlay_uses_source_without_allocating_or_copying_a_display() {
        let (layers, host, window) = setup();
        let logical = Size {
            width: 1024,
            height: 576,
        };
        let stored = Size {
            width: 960,
            height: 544,
        };
        let mut layers = layers.borrow_mut();
        let (mut images, _) = layers.video_open(window, logical, stored).unwrap();
        assert_eq!(drain(&host), ["create"]);
        let source_alive = Arc::downgrade(&images.source.lifetime);
        let bounds = Rect {
            left: 3,
            top: -2,
            ..logical.rect()
        };
        layers
            .video_frame(window, &mut images, None, [None; 2], Some((bounds, true)))
            .unwrap();
        assert!(drain(&host).is_empty());
        assert!(layers.movies.is_empty());
        assert!(images.display.is_none());
        for _ in 0..3 {
            layers
                .video_frame(
                    window,
                    &mut images,
                    Some(frame(stored)),
                    [None; 2],
                    Some((bounds, true)),
                )
                .unwrap();
            let request = host.next_request().unwrap();
            let WindowCommand::Graphics(Command::UploadYuv {
                pixels,
                logical_size,
                ..
            }) = &request.command
            else {
                panic!("expected compact YUV upload");
            };
            assert_eq!(pixels.size, stored);
            assert_eq!(*logical_size, logical);
            request.respond(Ok(window::Response::Done));
            assert!(drain(&host).is_empty());
            assert_eq!(layers.movies[&images.source.id].image.id, images.source.id);
            assert_eq!(layers.movies[&images.source.id].bounds, bounds);
            assert!(images.display.is_none());
        }
        layers.video_visible(window, &images, false, bounds);
        assert!(!layers.movies[&images.source.id].visible);
        layers.video_close(window, images);
        assert!(layers.movies.is_empty());
        assert!(layers.images.is_empty());
        assert!(source_alive.upgrade().is_none());
    }

    #[test]
    fn scaled_overlay_allocates_lazily_resizes_and_can_return_to_the_source() {
        let (layers, host, window) = setup();
        let size = Size {
            width: 8,
            height: 8,
        };
        let mut layers = layers.borrow_mut();
        let (mut images, _) = layers.video_open(window, size, size).unwrap();
        drain(&host);
        let scaled = Size {
            width: 4,
            height: 4,
        }
        .rect();
        layers
            .video_frame(
                window,
                &mut images,
                Some(frame(size)),
                [None; 2],
                Some((scaled, true)),
            )
            .unwrap();
        assert_eq!(drain(&host), ["upload"]);
        assert!(images.display.is_none());
        assert_eq!(layers.movies[&images.source.id].image.id, images.source.id);
        layers
            .video_frame(
                window,
                &mut images,
                None,
                [None; 2],
                Some((size.rect(), true)),
            )
            .unwrap();
        assert_eq!(drain(&host), ["create", "transform"]);
        let display = images.display.as_ref().unwrap();
        let display_id = display.id;
        let display_alive = Arc::downgrade(&display.lifetime);
        assert_eq!(layers.movies[&images.source.id].image.id, display_id);
        let resized = Size {
            width: 6,
            height: 4,
        }
        .rect();
        layers
            .video_frame(window, &mut images, None, [None; 2], Some((resized, true)))
            .unwrap();
        assert_eq!(drain(&host), ["resize", "transform"]);
        assert_eq!(layers.movies[&images.source.id].image.id, display_id);
        layers
            .video_frame(
                window,
                &mut images,
                Some(frame(size)),
                [None; 2],
                Some((resized, true)),
            )
            .unwrap();
        assert_eq!(drain(&host), ["upload"]);
        assert_eq!(layers.movies[&images.source.id].image.id, images.source.id);
        layers.video_close(window, images);
        assert!(layers.movies.is_empty());
        assert!(layers.images.is_empty());
        assert!(display_alive.upgrade().is_none());
    }
}
