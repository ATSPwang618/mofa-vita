use crate::gpu::{Gpu, Image};
use krkr_render::Result;
use std::sync::Arc;

impl Gpu {
    /// Detach only planes being written. All allocations succeed before either
    /// plane changes ownership, and the copy stays entirely on the GPU.
    pub(crate) fn independent(&self, image: &mut Image, main: bool, province: bool) -> Result<()> {
        self.detach(image, main, province, true)
    }
    /// Explicit Bitmap/Layer independence does not create an absent plane.
    pub fn independ_image(&self, image: &mut Image, province: bool, copy: bool) -> Result<()> {
        // A recipe is already an immutable independent version. Explicit COW
        // does not require rasterizing it; the eventual pixel write resolves it.
        if !province && image.deferred.is_some() {
            return Ok(());
        }
        self.detach(image, !province && image.main.is_some(), province, copy)
    }
    fn detach(&self, image: &mut Image, main: bool, province: bool, copy: bool) -> Result<()> {
        let writes_main = main;
        if main {
            self.materialize(image)?;
        }
        let main = if main && Arc::strong_count(&image.main_owners) > 1 {
            Some(self.allocation(image.size, image.main()?.texture.format(), &self.resident)?)
        } else {
            None
        };
        let province = if province && Arc::strong_count(&image.province_owners) > 1 {
            image
                .province
                .as_ref()
                .map(|p| self.allocation(image.size, p.texture.format(), &self.resident))
                .transpose()?
        } else {
            None
        };
        if main.is_none() && province.is_none() {
            if writes_main {
                image.main()?.changed();
            }
            return Ok(());
        }
        let mut encoder = self.device.create_command_encoder(&Default::default());
        if let Some(target) = &main {
            if copy {
                crate::copy::copy(&mut encoder, image.main()?, target, image.size.rect(), 0, 0);
            } else {
                // Original IndependNoCopy leaves new storage unspecified. Our
                // managed replacement is zeroed; already-independent data stays.
                self.clear(&mut encoder, target, [0.0; 4]);
            }
        }
        if let Some(target) = &province {
            if copy {
                crate::copy::copy(
                    &mut encoder,
                    image.province.as_ref().unwrap(),
                    target,
                    image.size.rect(),
                    0,
                    0,
                );
            } else {
                self.clear(&mut encoder, target, [0.0; 4]);
            }
        }
        self.submit(
            encoder,
            (copy.then(|| image.source()), main.clone(), province.clone()),
        );
        self.check()?;
        if let Some(main) = main {
            image.main = Some(main);
            image.main_owners = Arc::new(());
        }
        if let Some(province) = province {
            image.province = Some(province);
            image.province_owners = Arc::new(());
        }
        if writes_main {
            image.main()?.changed();
        }
        Ok(())
    }
}
