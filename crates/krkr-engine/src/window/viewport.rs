use super::*;
use krkr_protocol::graphics::Size;
use krkr_protocol::viewport::Viewport;
use tjs_core::{NativeCx, NativeStep};

impl Windows {
    pub(crate) fn recheck_viewport(&mut self, id: WindowId) {
        let now = self.now();
        if let Some(record) = self.records.get_mut(id)
            && (record.recheck.is_some()
                || record
                    .input_style
                    .is_none_or(|style| style.attention.is_some()))
        {
            // Attention coordinates change even when the hovered layer does
            // not. Refresh the cached style and reject pre-resize replies.
            record.input_style = None;
            record.style_revision = record.style_revision.wrapping_add(1);
            record.recheck = Some(now);
        }
    }
    pub(crate) fn viewport(&self, id: WindowId) -> NativeResult<Viewport> {
        let record = self.record(id)?;
        let Some(basis) = record.full_screen.or(record.client_basis) else {
            return Ok(record.viewport);
        };
        // Fit the script's client canvas into the physical drawable. The
        // windowed case composes with its explicit zoom and layer offset.
        let size = |width: u32, height: u32| Size {
            width: width.clamp(1, i32::MAX as u32),
            height: height.clamp(1, i32::MAX as u32),
        };
        let basis = size(basis.width, basis.height);
        let client = size(record.geometry.inner_width, record.geometry.inner_height);
        if record.full_screen.is_none() && basis == client {
            return Ok(record.viewport);
        }
        let (numer, denom) = if u64::from(client.width) * u64::from(basis.height)
            <= u64::from(client.height) * u64::from(basis.width)
        {
            (client.width, basis.width)
        } else {
            (client.height, basis.height)
        };
        let fit = Viewport::default()
            .zoom(numer as i32, denom as i32)
            .expect("positive client ratio");
        let dest = fit.destination(basis);
        let mut viewport = fit;
        if record.full_screen.is_none() {
            let original = record.viewport;
            viewport = original.with_client_scale(numer, denom);
            let scale = |offset: i32| {
                (i64::from(offset) * i64::from(fit.numer()) / i64::from(fit.denom()))
                    .clamp(i32::MIN as i64, i32::MAX as i64) as i32
            };
            viewport.left = scale(original.left);
            viewport.top = scale(original.top);
        }
        viewport.left = viewport
            .left
            .saturating_add(((client.width - dest.width) / 2) as i32);
        viewport.top = viewport
            .top
            .saturating_add(((client.height - dest.height) / 2) as i32);
        Ok(viewport)
    }
    pub(super) fn invalidate_viewport(&mut self, id: WindowId) {
        self.recheck_viewport(id);
        if let Some(layers) = self.layers.upgrade() {
            layers.borrow_mut().invalidate_viewport(id);
        }
    }
}
impl bindings::State {
    pub(super) fn viewport(&self) -> NativeResult<Viewport> {
        let lease = self.lease()?;
        Ok(lease.shared.borrow().record(lease.id)?.viewport)
    }
    pub(super) fn change_viewport(
        &self,
        cx: &mut NativeCx<'_>,
        viewport: Viewport,
    ) -> NativeResult<NativeStep> {
        let lease = self.lease()?;
        let layers = {
            let mut world = lease.shared.borrow_mut();
            let record = world.record_mut(lease.id)?;
            let basis = Size {
                width: record.geometry.inner_width,
                height: record.geometry.inner_height,
            };
            if record.viewport == viewport
                && (record.full_screen.is_some() || record.client_basis == Some(basis))
            {
                let layers = world.layers.upgrade().expect("installed Layer");
                drop(world);
                return crate::layer::sync_input(
                    &layers,
                    lease.id,
                    cx,
                    Value::Void,
                    Box::new(tasks::Returned),
                );
            }
            record.viewport = viewport;
            if record.full_screen.is_none() {
                record.client_basis = Some(basis);
            }
            world.recheck_viewport(lease.id);
            world.layers.upgrade().expect("installed Layer")
        };
        layers.borrow_mut().invalidate_viewport(lease.id);
        crate::layer::sync_input(
            &layers,
            lease.id,
            cx,
            Value::Void,
            Box::new(tasks::Returned),
        )
    }
}
