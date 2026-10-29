//! Independent finite observation contract; it does not emulate AvPlayer.
//! Host tests only test these rejection rules. Runtime facts must come from the
//! actual SDK calls made by the Vita binary, never from these unit fixtures.

#[derive(Clone, Copy, Debug)]
pub struct Asset {
    pub name: &'static str,
    pub relative_path: &'static str,
    pub bytes: u64,
    pub sha256: &'static str,
    pub duration_ms: u64,
    pub has_audio: bool,
    pub require_natural_eof: bool,
}

pub const ASSETS: [Asset; 3] = [
    Asset { name: "LOGO", relative_path: "movie/logo.mp4", bytes: 336200,
        sha256: "34137eac81996def746518c7a00e4612cd41c63809a4d90c11fbfc42cfd86a84",
        duration_ms: 10453, has_audio: true, require_natural_eof: true },
    Asset { name: "OPENING_EN", relative_path: "movie/opening_en.mp4", bytes: 15826531,
        sha256: "6bc4686fb0e76d13542ee36d2d288c62e38e17d6128dccf4f49e33f4c98554c6",
        duration_ms: 197300, has_audio: false, require_natural_eof: false },
    Asset { name: "OPENING_ZC", relative_path: "movie/opening_zc.mp4", bytes: 16279483,
        sha256: "11b8201ad1fcbc6242e6470ed63ac52f5096b281d4834fcff912c9cac0f50702",
        duration_ms: 197300, has_audio: false, require_natural_eof: false },
];

#[derive(Debug)]
pub struct Observations {
    pub asset: Asset,
    pub video_frames: u64,
    pub video_timestamp_advances: u64,
    pub last_video_ms: Option<u64>,
    pub presented_frames: u64,
    pub audio_frames: u64,
    pub audio_chunks: u64,
    pub audio_nonzero_samples: u64,
    pub last_audio_ms: Option<u64>,
    paused: bool,
    resumed: bool,
    stopped: bool,
    conclusion: Option<(bool, u64)>,
}

impl Default for Observations {
    fn default() -> Self { Self::new(ASSETS[0]) }
}

impl Observations {
    pub fn new(asset: Asset) -> Self {
        Self { asset, video_frames: 0, video_timestamp_advances: 0, last_video_ms: None,
            presented_frames: 0, audio_frames: 0, audio_chunks: 0, audio_nonzero_samples: 0,
            last_audio_ms: None, paused: false, resumed: false, stopped: false, conclusion: None }
    }

    pub fn video(&mut self, width: u32, height: u32, aspect: f32, timestamp_ms: u64) -> Result<(), String> {
        if width != 960 || height != 540 || !aspect.is_finite() || (aspect - 16.0 / 9.0).abs() > 0.02 {
            return Err(format!("unexpected decoded video metadata {width}x{height} aspect={aspect}"));
        }
        self.timestamp(timestamp_ms, self.last_video_ms)?;
        if self.last_video_ms.is_some_and(|last| timestamp_ms > last) { self.video_timestamp_advances += 1; }
        self.last_video_ms = Some(timestamp_ms);
        self.video_frames += 1;
        Ok(())
    }

    pub fn presented(&mut self) -> Result<(), String> {
        if self.presented_frames >= self.video_frames { return Err("presentation without a native frame".into()); }
        self.presented_frames += 1;
        Ok(())
    }

    pub fn audio(&mut self, samples: &[[f32; 2]], timestamp_ms: u64) -> Result<(), String> {
        if !self.asset.has_audio { return Err("unexpected PCM from a file with no audio stream".into()); }
        if samples.is_empty() || samples.len() > 960 { return Err("PCM pull must contain 1..960 stereo frames".into()); }
        self.timestamp(timestamp_ms, self.last_audio_ms)?;
        let mut nonzero = 0;
        for &sample in samples.iter().flatten() {
            if !sample.is_finite() || !(-1.0..=1.0).contains(&sample) { return Err("invalid decoded PCM sample".into()); }
            nonzero += u64::from(sample != 0.0);
        }
        self.audio_frames += samples.len() as u64;
        self.audio_chunks += 1;
        self.audio_nonzero_samples += nonzero;
        self.last_audio_ms = Some(timestamp_ms);
        Ok(())
    }

    fn timestamp(&self, value: u64, previous: Option<u64>) -> Result<(), String> {
        if value > self.asset.duration_ms + 1000 { return Err(format!("timestamp outside millisecond source duration: {value}")); }
        if previous.is_some_and(|previous| value < previous) { return Err(format!("timestamp moved backwards: {previous:?} -> {value}")); }
        Ok(())
    }

    pub fn pause_hold(&mut self, before_ms: u64, after_ms: u64, state_paused: bool,
        video_available: bool, audio_frames: usize) -> Result<(), String> {
        if !state_paused || video_available || audio_frames != 0 || before_ms.abs_diff(after_ms) > 5 {
            return Err("pause must hold clock and reject fresh video/PCM".into());
        }
        self.paused = true;
        Ok(())
    }

    pub fn resumed(&mut self, playing: bool) -> Result<(), String> {
        if !self.paused || !playing { return Err("resume requires observed pause and Playing state".into()); }
        self.resumed = true;
        Ok(())
    }

    pub fn stopped(&mut self, stopped: bool) -> Result<(), String> {
        if !self.resumed || !stopped { return Err("stop requires observed resume and Stopped state".into()); }
        self.stopped = true;
        Ok(())
    }

    pub fn conclude(&mut self, natural_eof: bool, elapsed_ms: u64) { self.conclusion = Some((natural_eof, elapsed_ms)); }

    pub fn complete(&self) -> Result<(), String> {
        if self.video_frames < 2 || self.video_timestamp_advances < 1 || self.presented_frames < 2 {
            return Err("missing advancing native video and completed GXM submissions".into());
        }
        let (natural_eof, elapsed_ms) = self.conclusion.ok_or("missing runtime conclusion")?;
        if self.asset.require_natural_eof {
            let near_end = self.asset.duration_ms.saturating_sub(250);
            if !natural_eof || elapsed_ms + 500 < self.asset.duration_ms || self.last_video_ms.unwrap_or(0) < near_end {
                return Err("missing full logo duration or premature native inactive/EOF".into());
            }
            if self.audio_frames < near_end * 48 || self.last_audio_ms.unwrap_or(0) < near_end || self.audio_nonzero_samples == 0 {
                return Err("missing real complete logo PCM observations".into());
            }
        } else {
            if natural_eof || elapsed_ms < 2500 || self.last_video_ms.unwrap_or(0) < 1500 || !self.paused || !self.resumed || !self.stopped {
                return Err("opening needs advancing playback, pause/resume and explicit stop".into());
            }
            if self.audio_frames != 0 { return Err("video-only opening unexpectedly produced PCM".into()); }
        }
        Ok(())
    }
}
