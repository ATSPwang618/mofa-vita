//! Audio identities and source-domain metadata, independent of device APIs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PlaybackId(pub u64);

#[derive(Clone, Copy, Debug, Default)]
pub struct Format {
    pub rate: u32,
    pub channels: u32,
    pub bits: u32,
    pub frames: u64,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Position {
    pub decoded: u64,
    pub submitted: u64,
    /// Source position estimated from the output device's playback timestamp.
    pub played: u64,
}
