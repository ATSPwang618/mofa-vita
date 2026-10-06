use super::*;
#[derive(Clone, Copy)]
pub enum Codec {
    Vorbis,
    Tcwf,
    Opus,
    Ffmpeg,
}
pub struct Registration {
    counts: Arc<Mutex<[usize; 4]>>,
    kind: Codec,
}
impl Service {
    /// Enable a decoder provider for future opens. Open streams own their
    /// decoder and remain valid when the registration is released.
    pub fn register_decoder(&self, kind: Codec) -> Result<Registration> {
        if matches!(kind, Codec::Opus | Codec::Ffmpeg)
            && self.0.decoder_backend.lock().unwrap().is_none()
        {
            return Err("host has no extended audio decoder backend".into());
        }
        self.0.codecs.lock().unwrap()[kind as usize] += 1;
        Ok(Registration {
            counts: self.0.codecs.clone(),
            kind,
        })
    }
}
impl Drop for Registration {
    fn drop(&mut self) {
        self.counts.lock().unwrap()[self.kind as usize] -= 1;
    }
}
