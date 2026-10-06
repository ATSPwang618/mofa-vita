//! Modified Rust adaptation of krkrsdl3/plugins/wutcwf.cpp.
//! See tcwf-LICENSE.txt. Buffers fit the voice's reserved decode allowance.
use crate::{Format, Frame, Result};
use krkr_assets::Stream;
use std::io::{Read, Seek, SeekFrom};
const ADAPT: [i32; 16] = [
    230, 230, 230, 230, 307, 409, 512, 614, 768, 614, 512, 409, 307, 230, 230, 230,
];
const C1: [i32; 7] = [256, 512, 0, 192, 240, 460, 392];
const C2: [i32; 7] = [0, -256, 0, 64, 0, -208, -232];
const INDEX: [i32; 16] = [-1, -1, -1, -1, 2, 4, 6, 8, -1, -1, -1, -1, 2, 4, 6, 8];
const STEPS: [i32; 89] = [
    7, 8, 9, 10, 11, 12, 13, 14, 16, 17, 19, 21, 23, 25, 28, 31, 34, 37, 41, 45, 50, 55, 60, 66,
    73, 80, 88, 97, 107, 118, 130, 143, 157, 173, 190, 209, 230, 253, 279, 307, 337, 371, 408, 449,
    494, 544, 598, 658, 724, 796, 876, 963, 1060, 1166, 1282, 1411, 1552, 1707, 1878, 2066, 2272,
    2499, 2749, 3024, 3327, 3660, 4026, 4428, 4871, 5358, 5894, 6484, 7132, 7845, 8630, 9493,
    10442, 11487, 12635, 13899, 15289, 16818, 18500, 20350, 22385, 24623, 27086, 29794, 32767,
];
pub(crate) struct Decoder {
    stream: Box<dyn Stream>,
    pub format: Format,
    bytes: usize,
    frames: usize,
    length: u64,
    data: Vec<u8>,
    samples: Vec<i16>,
    offset: usize,
    position: u64,
}
impl Decoder {
    pub fn open(mut stream: Box<dyn Stream>, length: u64) -> Result<Self> {
        let mut header = [0; 24];
        stream.read_exact(&mut header).map_err(|e| e.to_string())?;
        if &header[..6] != b"TCWF0\x1a" {
            return Err("invalid TCWF signature".into());
        }
        let channels = u32::from(header[6]);
        let rate = u32::from_le_bytes(header[8..12].try_into().unwrap());
        let bytes = u32::from_le_bytes(header[16..20].try_into().unwrap()) as usize;
        let frames = u32::from_le_bytes(header[20..24].try_into().unwrap()) as usize;
        if !(1..=2).contains(&channels)
            || rate == 0
            || rate > 384_000
            || !(2..=65536).contains(&frames)
            || bytes < frames + 30
            || bytes > 131072
        {
            return Err("invalid or unsupported TCWF header".into());
        }
        // Original format deliberately reports unknown total samples/time.
        let format = Format {
            rate,
            channels,
            bits: 16,
            frames: 0,
        };
        Ok(Self {
            stream,
            format,
            bytes,
            frames,
            length,
            data: vec![0; bytes],
            samples: vec![0; frames * channels as usize],
            offset: frames,
            position: 0,
        })
    }
    fn block(&mut self) -> Result<bool> {
        let channels = self.format.channels as usize;
        for channel in 0..channels {
            match self.stream.read_exact(&mut self.data) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(false),
                Err(e) => return Err(e.to_string()),
            }
            decode_channel(
                &self.data,
                &mut self.samples,
                channels,
                channel,
                self.frames,
            )?;
        }
        self.offset = 0;
        Ok(true)
    }
    pub fn seek(&mut self, frame: u64) -> Result<()> {
        let block = frame / self.frames as u64;
        let byte = block
            .checked_mul(self.bytes as u64 * self.format.channels as u64)
            .and_then(|v| v.checked_add(24))
            .ok_or("TCWF seek overflow")?;
        if byte > self.length {
            return Err("TCWF seek outside stream".into());
        }
        self.stream
            .seek(SeekFrom::Start(byte))
            .map_err(|e| e.to_string())?;
        self.position = frame;
        self.offset = self.frames;
        if byte == self.length {
            return Ok(());
        }
        if !self.block()? {
            return Err("incomplete TCWF seek block".into());
        }
        self.offset = (frame % self.frames as u64) as usize;
        Ok(())
    }
    pub fn next(&mut self) -> Result<Option<Frame>> {
        if self.offset == self.frames && !self.block()? {
            return Ok(None);
        }
        let channels = self.format.channels as usize;
        let i = self.offset * channels;
        let sample = [
            self.samples[i] as f32 / 32768.,
            self.samples[i + channels - 1] as f32 / 32768.,
        ];
        let frame = Frame {
            sample,
            pcm: std::array::from_fn(|c| if c < channels { self.samples[i + c] } else { 0 }),
            position: self.position,
            labels: [0, 0],
        };
        self.offset += 1;
        self.position += 1;
        Ok(Some(frame))
    }
}
fn decode_channel(
    data: &[u8],
    out: &mut [i16],
    stride: usize,
    channel: usize,
    frames: usize,
) -> Result<()> {
    let short = |i| i16::from_le_bytes([data[i], data[i + 1]]);
    let predictor = data[6] as usize;
    let mut step_index = i32::from(data[7]);
    if predictor >= 7 || step_index > 88 {
        return Err("invalid TCWF predictor".into());
    }
    out[channel] = short(0);
    out[channel + stride] = short(2);
    let mut delta = i32::from(short(4));
    for k in 2..frames {
        let code = (data[32 + k - 2] & 15) as usize;
        let previous_delta = delta;
        delta = (ADAPT[code].wrapping_mul(delta) >> 8).max(16);
        let signed = if code & 8 != 0 {
            code as i32 - 16
        } else {
            code as i32
        };
        let predict = (i32::from(out[channel + (k - 1) * stride]) * C1[predictor]
            + i32::from(out[channel + (k - 2) * stride]) * C2[predictor])
            >> 8;
        out[channel + k * stride] = signed
            .wrapping_mul(previous_delta)
            .wrapping_add(predict)
            .clamp(-32768, 32767) as i16;
    }
    let mut previous = 0;
    for k in 2..frames {
        let code = (data[32 + k - 2] >> 4) as usize;
        let step = STEPS[step_index as usize];
        let mut diff = step >> 3;
        if code & 1 != 0 {
            diff += step >> 2;
        }
        if code & 2 != 0 {
            diff += step >> 1;
        }
        if code & 4 != 0 {
            diff += step;
        }
        if code & 8 != 0 {
            diff = -diff;
        }
        previous = (previous + diff).clamp(-32768, 32767);
        step_index = (step_index + INDEX[code]).clamp(0, 88);
        let i = channel + k * stride;
        out[i] = (i32::from(out[i]) + previous).clamp(-32768, 32767) as i16;
    }
    for i in 0..6 {
        let pos = u16::from_le_bytes([data[8 + i * 4], data[9 + i * 4]]) as usize;
        let revise = short(10 + i * 4);
        if revise != 0 {
            if pos >= frames {
                return Err("TCWF peak outside block".into());
            }
            let i = channel + pos * stride;
            out[i] = (i32::from(out[i]) - i32::from(revise)).clamp(-32768, 32767) as i16;
        }
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn dual_adpcm_peaks_stereo_blocks_and_seek() {
        let mut left = vec![0; 38];
        for (offset, n) in [(0, 1000i16), (2, 1200), (4, 16), (10, 17)] {
            left[offset..offset + 2].copy_from_slice(&n.to_le_bytes());
        }
        left[8] = 2;
        left[32..].copy_from_slice(&[0x11, 0x21, 0x31, 0x41, 0x51, 0x61]);
        let mut right = vec![0; 38];
        right[0..2].copy_from_slice(&(-2000i16).to_le_bytes());
        right[2..4].copy_from_slice(&(-2000i16).to_le_bytes());
        right[4] = 16;
        let mut bytes = b"TCWF0\x1a\x02\0".to_vec();
        for n in [8000u32, 2, 38, 8] {
            bytes.extend(n.to_le_bytes());
        }
        for _ in 0..2 {
            bytes.extend(&left);
            bytes.extend(&right);
        }
        let length = bytes.len() as u64;
        let mut d = Decoder::open(Box::new(std::io::Cursor::new(bytes)), length).unwrap();
        let expected = [1000, 1200, 1200, 1236, 1256, 1279, 1307, 1343];
        for i in 0..16 {
            let f = d.next().unwrap().unwrap();
            assert_eq!(f.position, i);
            assert_eq!(
                f.sample,
                [expected[i as usize % 8] as f32 / 32768., -2000. / 32768.]
            );
        }
        assert!(d.next().unwrap().is_none());
        d.seek(11).unwrap();
        assert_eq!(d.next().unwrap().unwrap().sample[0], 1236. / 32768.);
        d.seek(16).unwrap();
        assert!(d.next().unwrap().is_none());
        assert!(d.seek(u64::MAX).is_err());
        let mut out = [0; 16];
        left[6] = 7;
        assert!(decode_channel(&left, &mut out, 2, 0, 8).is_err());
        left[6] = 0;
        left[8] = 8;
        assert!(decode_channel(&left, &mut out, 2, 0, 8).is_err());
    }
}
