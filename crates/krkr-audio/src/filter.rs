//! Managed streaming phase vocoder. The script thread changes parameters;
//! windowing, FFT and overlap/add run exclusively on the decoder worker.
use super::*;
use rustfft::{Fft, FftPlanner, num_complex::Complex32};
use std::f32::consts::{PI, TAU};

#[derive(Clone, Copy)]
pub struct Parameters {
    pub window: usize,
    pub overlap: usize,
    pub pitch: f32,
    /// Output duration divided by input duration (not playback frequency).
    pub time: f32,
}
impl Default for Parameters {
    fn default() -> Self {
        Self {
            window: 4096,
            overlap: 0,
            pitch: 1.0,
            time: 1.0,
        }
    }
}
#[derive(Clone, Default)]
pub struct PhaseVocoder(Arc<Control>);
#[derive(Default)]
struct Control {
    parameters: Mutex<Parameters>,
    connected: AtomicBool,
}
impl PhaseVocoder {
    pub fn parameters(&self) -> Parameters {
        *self.0.parameters.lock().unwrap()
    }
    pub fn set_window(&self, value: i32) -> Result<()> {
        if !(64..=32768).contains(&value) || !(value as u32).is_power_of_two() {
            return Err("PhaseVocoder.window must be a power of two from 64 to 32768".into());
        }
        self.0.parameters.lock().unwrap().window = value as usize;
        Ok(())
    }
    pub fn set_overlap(&self, value: i32) -> Result<()> {
        if !matches!(value, 0 | 2 | 4 | 8 | 16 | 32) {
            return Err("PhaseVocoder.overlap must be 0, 2, 4, 8, 16 or 32".into());
        }
        self.0.parameters.lock().unwrap().overlap = value as usize;
        Ok(())
    }
    pub fn set_pitch(&self, value: f32) -> Result<()> {
        positive(value)?;
        self.0.parameters.lock().unwrap().pitch = value;
        Ok(())
    }
    pub fn set_time(&self, value: f32) -> Result<()> {
        positive(value)?;
        self.0.parameters.lock().unwrap().time = value;
        Ok(())
    }
    pub(super) fn connect(&self) -> Result<Connection> {
        if self.0.connected.swap(true, Ordering::AcqRel) {
            return Err("PhaseVocoder cannot connect to multiple sound buffers at once".into());
        }
        Ok(Connection(self.clone()))
    }
}
fn positive(value: f32) -> Result<()> {
    if value.is_finite() && value > 0.0 {
        Ok(())
    } else {
        Err("PhaseVocoder scale must be finite and positive".into())
    }
}
pub(super) struct Connection(PhaseVocoder);
impl Drop for Connection {
    fn drop(&mut self) {
        self.0.0.connected.store(false, Ordering::Release);
    }
}

pub(super) struct Stream {
    source: loops::Stream,
    stages: Vec<Stage>,
}
impl Stream {
    pub fn new(
        source: loops::Stream,
        filters: Vec<PhaseVocoder>,
        budget: Budget,
        channels: usize,
    ) -> Self {
        Self {
            source,
            stages: filters
                .into_iter()
                .map(|control| Stage {
                    parameters: control.parameters(),
                    control,
                    dsp: None,
                    budget: budget.clone(),
                    channels,
                })
                .collect(),
        }
    }
    pub fn update(&mut self) {
        for stage in &mut self.stages {
            stage.parameters = stage.control.parameters();
        }
    }
    pub fn position(&self) -> u64 {
        self.source.position()
    }
    pub fn seek(&mut self, position: u64) -> Result<()> {
        self.source.seek(position)?;
        for stage in &mut self.stages {
            stage.dsp = None;
        }
        self.update();
        Ok(())
    }
    pub fn next(&mut self, voice: &Voice) -> Result<Option<Frame>> {
        fn next(
            stages: &mut [Stage],
            source: &mut loops::Stream,
            voice: &Voice,
        ) -> Result<Option<Frame>> {
            if !voice.alive.load(Ordering::Acquire) {
                return Ok(None);
            }
            if let Some((last, rest)) = stages.split_last_mut() {
                return last.next(&mut || next(rest, source, voice));
            }
            match source.next()? {
                None if voice.looping.load(Ordering::Relaxed) && source.position() != 0 => {
                    source.seek(0)?;
                    source.next()
                }
                frame => Ok(frame),
            }
        }
        next(&mut self.stages, &mut self.source, voice)
    }
    pub fn read_frames(&mut self, voice: &Voice, output: &mut [Frame]) -> Result<(usize, bool)> {
        // update() snapshots controls once per fill. Unity filters with no
        // overlap history need neither recursive dispatch nor per-sample DSP.
        let identity = self
            .stages
            .iter()
            .all(|s| s.dsp.is_none() && s.parameters.pitch == 1.0 && s.parameters.time == 1.0);
        let mut count = 0;
        while count < output.len() {
            if !voice.alive.load(Ordering::Acquire) {
                return Ok((count, false));
            }
            if identity && let Some(read) = self.source.read_plain(&mut output[count..]) {
                let length = read?;
                count += length;
                if length != 0 {
                    continue;
                }
                if voice.looping.load(Ordering::Relaxed) && self.source.position() != 0 {
                    self.source.seek(0)?;
                    // Match next()'s single retry, including empty streams.
                    if let Some(frame) = self.source.next()? {
                        output[count] = frame;
                        count += 1;
                        continue;
                    }
                }
                return Ok((count, true));
            }
            match self.next(voice)? {
                Some(frame) => {
                    output[count] = frame;
                    count += 1;
                }
                None => return Ok((count, true)),
            }
        }
        Ok((count, false))
    }
}
struct Stage {
    control: PhaseVocoder,
    parameters: Parameters,
    dsp: Option<Dsp>,
    budget: Budget,
    channels: usize,
}
impl Stage {
    fn next(&mut self, source: &mut dyn FnMut() -> Result<Option<Frame>>) -> Result<Option<Frame>> {
        // Unity is an actual identity operation: avoid FFT allocation/work on
        // ordinary BGM, including Vita. Once active, keep overlap history even
        // when parameters return to unity, until a seek/reopen resets it.
        if self.dsp.is_none() {
            if self.parameters.pitch == 1.0 && self.parameters.time == 1.0 {
                return source();
            }
            self.dsp = Some(Dsp::new(
                self.parameters.window,
                self.channels,
                &self.budget,
            )?);
        }
        self.dsp.as_mut().unwrap().next(self.parameters, source)
    }
}

struct Dsp {
    size: usize,
    channels: usize,
    input: VecDeque<Frame>,
    output: VecDeque<Frame>,
    overlap: VecDeque<[f32; 10]>,
    labels: VecDeque<[u16; 2]>,
    window: Vec<f32>,
    analysis_phase: Vec<Vec<f32>>,
    synthesis_phase: Vec<Vec<f32>>,
    spectrum: Vec<Complex32>,
    fft_buffer: Vec<Complex32>,
    scratch: Vec<Complex32>,
    forward: Arc<dyn Fft<f32>>,
    inverse: Arc<dyn Fft<f32>>,
    _permit: Permit,
}
impl Dsp {
    fn new(size: usize, source_channels: usize, budget: &Budget) -> Result<Self> {
        let channels = if source_channels <= 2 {
            2
        } else {
            source_channels + 2
        };
        // Includes queues, per-channel phase history and an upper reservation
        // for radix-2 FFT plans/scratch. No allocation in the device callback.
        let bytes = size * (2 * std::mem::size_of::<Frame>() + 40 + 64 + channels * 8 + 8);
        let permit = budget.reserve(bytes).map_err(|e| e.to_string())?;
        let mut planner = FftPlanner::new();
        let forward = planner.plan_fft_forward(size);
        let inverse = planner.plan_fft_inverse(size);
        let scratch = vec![
            Complex32::default();
            forward
                .get_inplace_scratch_len()
                .max(inverse.get_inplace_scratch_len())
        ];
        Ok(Self {
            size,
            channels,
            input: VecDeque::with_capacity(size),
            output: VecDeque::with_capacity(size),
            overlap: std::iter::repeat_n([0.0; 10], size).collect(),
            labels: VecDeque::with_capacity(size),
            // Vorbis-I window, as used by the reference phase vocoder.
            window: (0..size)
                .map(|i| {
                    let x = (i as f64 + 0.5) / size as f64;
                    (std::f64::consts::FRAC_PI_2 * (std::f64::consts::PI * x).sin().powi(2)).sin()
                        as f32
                })
                .collect(),
            analysis_phase: vec![vec![0.0; size / 2]; channels],
            synthesis_phase: vec![vec![0.0; size / 2]; channels],
            spectrum: vec![Complex32::default(); size / 2],
            fft_buffer: vec![Complex32::default(); size],
            scratch,
            forward,
            inverse,
            _permit: permit,
        })
    }
    fn next(
        &mut self,
        parameters: Parameters,
        source: &mut dyn FnMut() -> Result<Option<Frame>>,
    ) -> Result<Option<Frame>> {
        if let Some(frame) = self.output.pop_front() {
            return Ok(Some(frame));
        }
        while self.input.len() < self.size {
            let Some(frame) = source()? else {
                break;
            };
            self.input.push_back(frame);
        }
        if self.input.is_empty() {
            return Ok(None);
        }
        let oversampling = match parameters.overlap {
            0 if parameters.time <= 0.2 => 2,
            0 if parameters.time <= 1.2 => 4,
            0 => 8,
            count => count,
        };
        let input_hop = self.size / oversampling;
        // The reference aligns the synthesis hop to two samples.
        let output_hop = (input_hop as f32 * parameters.time) as usize & !1;
        if !(2..=self.size).contains(&output_hop) {
            return Err("PhaseVocoder time/overlap produces an unsupported synthesis hop".into());
        }
        self.process(parameters, input_hop, output_hop, oversampling);
        let consumed = input_hop.min(self.input.len());
        let written = (consumed * output_hop).div_ceil(input_hop);
        let mut metadata = 0;
        for i in 0..written {
            let index = (i * input_hop / output_hop).min(consumed - 1);
            let mut frame = self.input[index];
            let end = ((i + 1) * input_hop / output_hop).min(consumed);
            for source in self.input.range(metadata..end) {
                if source.labels[0] != source.labels[1] {
                    if let Some(last) = self
                        .labels
                        .back_mut()
                        .filter(|last| last[1] == source.labels[0])
                    {
                        last[1] = source.labels[1];
                    } else {
                        if self.labels.len() == self.size {
                            return Err("audio filter label capacity reached".into());
                        }
                        self.labels.push_back(source.labels);
                    }
                }
            }
            metadata = end;
            frame.labels = self.labels.pop_front().unwrap_or([0, 0]);
            let output = self.overlap.pop_front().unwrap();
            self.overlap.push_back([0.0; 10]);
            frame.sample = [output[0], output[1]];
            frame.pcm = std::array::from_fn(|channel| {
                let channel = if self.channels == 2 {
                    channel
                } else {
                    channel + 2
                };
                if channel < self.channels {
                    (output[channel] * 32768.).round().clamp(-32768., 32767.) as i16
                } else {
                    0
                }
            });
            self.output.push_back(frame);
        }
        self.input.drain(..consumed);
        Ok(self.output.pop_front())
    }
    fn process(
        &mut self,
        parameters: Parameters,
        input_hop: usize,
        output_hop: usize,
        oversampling: usize,
    ) {
        let advance = TAU * input_hop as f32 / self.size as f32;
        let scale = output_hop as f32 / input_hop as f32;
        // RustFFT's inverse gain is N; the reference real FFT has gain N/2.
        let gain =
            2.0 * parameters.time / (self.size * oversampling) as f32 / parameters.pitch.sqrt();
        for channel in 0..self.channels {
            for (i, value) in self.fft_buffer.iter_mut().enumerate() {
                let sample = self.input.get(i).map_or(0.0, |frame| {
                    if channel < 2 {
                        frame.sample[channel]
                    } else {
                        f32::from(frame.pcm[channel - 2]) / 32768.0
                    }
                });
                *value = Complex32::new(sample * self.window[i], 0.0);
            }
            self.forward
                .process_with_scratch(&mut self.fft_buffer, &mut self.scratch);
            for (bin, value) in self.spectrum.iter_mut().enumerate() {
                let sample = self.fft_buffer[bin];
                let phase = sample.arg();
                let delta = wrap(phase - self.analysis_phase[channel][bin] - bin as f32 * advance);
                self.analysis_phase[channel][bin] = phase;
                *value = Complex32::new(sample.norm(), bin as f32 + delta / advance);
            }
            self.fft_buffer.fill(Complex32::default());
            for bin in 0..self.size / 2 {
                let source = bin as f32 / parameters.pitch;
                let index = source as usize;
                let Some(&a) = self.spectrum.get(index) else {
                    continue;
                };
                let b = self.spectrum.get(index + 1).copied().unwrap_or(a);
                let value = a + (b - a) * (source - index as f32);
                let phase = &mut self.synthesis_phase[channel][bin];
                *phase = wrap(*phase + value.im * parameters.pitch * advance * scale);
                let (sin, cos) = phase.sin_cos();
                let value = Complex32::new(value.re * cos, value.re * sin);
                self.fft_buffer[bin] = value;
                if bin != 0 {
                    self.fft_buffer[self.size - bin] = value.conj();
                }
            }
            self.inverse
                .process_with_scratch(&mut self.fft_buffer, &mut self.scratch);
            for (i, output) in self.overlap.iter_mut().enumerate() {
                output[channel] += self.fft_buffer[i].re * self.window[i] * gain;
            }
        }
    }
}
fn wrap(value: f32) -> f32 {
    (value + PI).rem_euclid(TAU) - PI
}
