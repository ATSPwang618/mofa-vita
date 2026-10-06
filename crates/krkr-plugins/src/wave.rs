//! Named wave decoder providers; decoded samples use the common audio service.
use krkr_engine::{
    audio::{Codec, Registration},
    plugins::{Context, Plugin},
};
use tjs_core::NativeResult;
macro_rules! provider {
    ($name:ident,$kind:ident,$($alias:literal),+ $(,)?) => {
        #[derive(Default, tjs_bind::Trace)]
        pub(crate) struct $name {
            #[trace(skip = "Audio decoder registration contains no VM objects")]
            registration: Option<Registration>,
        }
        krkr_engine::native_plugin! {impl $name {names:[$($alias),+]}}
        impl Plugin for $name {
            fn link(&mut self, cx: &mut Context<'_>) -> NativeResult<()> {
                self.registration = Some(krkr_engine::extensions::register_audio_decoder(
                    cx.heap,
                    Codec::$kind,
                )?);
                Ok(())
            }
            fn unlink(&mut self, _: &mut Context<'_>) -> NativeResult<bool> {
                self.registration = None;
                Ok(true)
            }
        }
    };
}
provider!(Vorbis, Vorbis, "wuvorbis.dll", "wuvorbis.tpm");
provider!(Tcwf, Tcwf, "wutcwf.dll", "wutcwf.tpm");
provider!(
    Opus,
    Opus,
    "wuopus.dll",
    "wuopus.tpm",
    "kropus.dll",
    "kropus.tpm"
);
provider!(Ffmpeg, Ffmpeg, "wuffmpeg.dll", "wuffmpeg.tpm");
