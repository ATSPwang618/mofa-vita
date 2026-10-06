//! CPAL device ownership stays in the desktop host; mixing is portable.
use cpal::{
    FromSample, OutputCallbackInfo, SampleFormat, SizedSample,
    traits::{DeviceTrait, HostTrait, StreamTrait},
};
use krkr_audio::{Mixer, OutputHost, Result};
use std::{sync::Mutex, time::Duration};

#[derive(Default)]
pub struct Audio {
    stream: Mutex<Option<cpal::Stream>>,
}
impl OutputHost for Audio {
    fn start(&self, mixer: Mixer) -> Result<()> {
        let device = cpal::default_host()
            .default_output_device()
            .ok_or("no audio output device")?;
        let config = device.default_output_config().map_err(|e| e.to_string())?;
        let stream = match config.sample_format() {
            SampleFormat::F32 => build::<f32>(&device, config.into(), mixer),
            SampleFormat::F64 => build::<f64>(&device, config.into(), mixer),
            SampleFormat::I16 => build::<i16>(&device, config.into(), mixer),
            SampleFormat::U16 => build::<u16>(&device, config.into(), mixer),
            SampleFormat::I32 => build::<i32>(&device, config.into(), mixer),
            SampleFormat::I8 => build::<i8>(&device, config.into(), mixer),
            SampleFormat::U8 => build::<u8>(&device, config.into(), mixer),
            SampleFormat::U32 => build::<u32>(&device, config.into(), mixer),
            SampleFormat::I64 => build::<i64>(&device, config.into(), mixer),
            SampleFormat::U64 => build::<u64>(&device, config.into(), mixer),
            SampleFormat::I24 => build::<cpal::I24>(&device, config.into(), mixer),
            _ => return Err("unsupported audio device sample format".into()),
        }?;
        stream.play().map_err(|e| e.to_string())?;
        *self.stream.lock().unwrap() = Some(stream);
        Ok(())
    }
}
fn build<T: SizedSample + FromSample<f32>>(
    device: &cpal::Device,
    config: cpal::StreamConfig,
    mut mixer: Mixer,
) -> Result<cpal::Stream> {
    let channels = config.channels as usize;
    if channels == 0 || channels > 32 {
        return Err("unsupported output channel count".into());
    }
    let rate = config.sample_rate;
    let error = mixer.error_sink();
    // Fixed conversion workspace; no device callback allocation.
    let mut scratch = [0.0f32; 8192];
    device
        .build_output_stream(
            config,
            move |output: &mut [T], info: &OutputCallbackInfo| {
                let stamp = info.timestamp();
                let delay = stamp.playback.duration_since(stamp.callback);
                let chunk = scratch.len() / channels * channels;
                let mut frames = 0usize;
                for target in output.chunks_mut(chunk) {
                    mixer.render(
                        &mut scratch[..target.len()],
                        channels,
                        rate,
                        delay + Duration::from_secs_f64(frames as f64 / rate as f64),
                    );
                    for (out, sample) in target.iter_mut().zip(&scratch) {
                        *out = T::from_sample(*sample);
                    }
                    frames += target.len() / channels;
                }
            },
            move |failure| {
                *error.lock().unwrap() = Some(failure.to_string());
            },
            None,
        )
        .map_err(|e| e.to_string())
}
