//! Compose in layer pixels before scaling the completed scene for a window.
use crate::{Gpu, Image};
use krkr_protocol::{
    graphics::{ImageId, Scene},
    transform::{Filter, ImageOperation, Sampling, StretchRect, Transform},
};
use krkr_render::Result;
use std::collections::HashMap;

impl Gpu {
    pub fn compose_window(
        &self,
        target: &mut Image,
        logical: &mut Option<Image>,
        scene: &Scene,
        images: &HashMap<ImageId, Image>,
    ) -> Result<()> {
        let primary = scene
            .nodes
            .iter()
            .find(|node| node.parent.is_none() && node.visible);
        if scene.viewport == Default::default() || primary.is_none() {
            *logical = None;
            return self.compose(target, scene, images);
        }
        let rect = primary.expect("primary layer").rectangle;
        let size = krkr_protocol::graphics::Size {
            width: rect.width,
            height: rect.height,
        };
        if logical.as_ref().is_none_or(|image| image.size != size) {
            *logical = None;
            *logical = Some(self.create_surface_image(size)?);
        }
        let logical = logical.as_mut().expect("logical surface");
        self.compose(logical, scene, images)?;
        self.map_window(target, logical, scene.viewport)
    }

    /// Present a completed canvas without retaining or re-reading its source
    /// layers. OS redraw and resize can reuse this immutable frame.
    pub fn map_window(
        &self,
        target: &mut Image,
        canvas: &Image,
        viewport: krkr_protocol::viewport::Viewport,
    ) -> Result<()> {
        self.materialize(target)?;
        // Clear uncovered client pixels each frame, including after moving the layer.
        let mut encoder = self.device.create_command_encoder(&Default::default());
        self.independ_image(target, false, false)?;
        self.clear(&mut encoder, target.main()?, [0.0; 4]);
        self.submit(encoder, target.main()?.clone());
        let size = canvas.size;
        let dest = viewport.destination(size);
        self.transform(
            target,
            &canvas.source(),
            size.rect(),
            Transform::Stretch(StretchRect {
                left: dest.left,
                top: dest.top,
                width: dest.width as i32,
                height: dest.height as i32,
            }),
            Sampling {
                filter: Filter::FastLinear,
                sharpness: -1.0,
                no_clip: false,
            },
            ImageOperation::Copy { hold_alpha: false },
            target.size.rect(),
            None,
        )
    }
}
