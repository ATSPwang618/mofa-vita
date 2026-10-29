//! Bounded MP4/H.264 geometry inspection for Vita movie presentation.
//!
//! The native AvPlayer buffer dimensions and this source geometry are separate
//! observations. Cropping is expressed in pixels, not H.264 crop units.
//! This reads configuration metadata only; avc3 parameter changes carried in
//! media samples are outside the contract and are not inferred by scanning mdat.
//! Baseline, Main and High 8-bit 4:2:0 progressive SPS configurations are accepted
//! within the decoder's current 960x544 limit. Unsupported modes fail explicitly.

use std::fmt;
use std::fs::File;
use std::io::{self, Cursor, Read, Seek, SeekFrom};

const MAX_BOXES: usize = 4096;
const MAX_DEPTH: usize = 8;
const MAX_METADATA_READ: usize = 1024 * 1024;
const MAX_AVCC_BYTES: usize = 65536;
const MAX_SPS_BYTES: usize = 4096;
const MAX_TRACKS: usize = 32;
const MAX_SAMPLE_ENTRIES: usize = 32;
const MAX_CODED_WIDTH: u32 = 960;
const MAX_CODED_HEIGHT: u32 = 544;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SampleAspectRatio {
    pub numerator: u32,
    pub denominator: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct VideoGeometry {
    pub coded_width: u32,
    pub coded_height: u32,
    pub crop_left: u32,
    pub crop_top: u32,
    pub crop_right: u32,
    pub crop_bottom: u32,
    pub visible_width: u32,
    pub visible_height: u32,
    /// None means absent or unspecified; this is a sample, not display, ratio.
    pub sar: Option<SampleAspectRatio>,
}

#[derive(Debug)]
pub enum GeometryError {
    Io(io::Error),
    Malformed(&'static str),
    Truncated,
    Limit(&'static str),
    Unsupported(&'static str),
    Ambiguous,
}

impl fmt::Display for GeometryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(f, "MP4 geometry I/O: {error}"),
            Self::Malformed(reason) => write!(f, "malformed MP4/H.264: {reason}"),
            Self::Truncated => f.write_str("truncated MP4/H.264 metadata"),
            Self::Limit(reason) => write!(f, "MP4/H.264 inspection limit: {reason}"),
            Self::Unsupported(reason) => write!(f, "unsupported Vita movie: {reason}"),
            Self::Ambiguous => f.write_str("ambiguous video stream or SPS geometry"),
        }
    }
}

impl std::error::Error for GeometryError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            _ => None,
        }
    }
}

pub fn read_file(path: &str) -> Result<VideoGeometry, GeometryError> {
    let mut file = File::open(path).map_err(GeometryError::Io)?;
    read_from(&mut file)
}

pub fn parse_bytes(bytes: &[u8]) -> Result<VideoGeometry, GeometryError> {
    read_from(&mut Cursor::new(bytes))
}

/// Inspects one MP4 without reading its media payload. The reader is left at an
/// unspecified position. Extended box sizes and moov-after-mdat are supported.
pub fn read_from<R: Read + Seek>(reader: &mut R) -> Result<VideoGeometry, GeometryError> {
    let end = reader.seek(SeekFrom::End(0)).map_err(GeometryError::Io)?;
    let mut input = Input {
        reader,
        end,
        boxes: 0,
        metadata_read: 0,
    };
    let roots = input.children(0, end, 0)?;
    let moov = unique(&roots, *b"moov")?.ok_or(GeometryError::Unsupported("missing moov"))?;
    let children = input.children(moov.data, moov.end, 1)?;
    let tracks: Vec<_> = children
        .iter()
        .filter(|b| b.kind == *b"trak")
        .copied()
        .collect();
    if tracks.len() > MAX_TRACKS {
        return Err(GeometryError::Limit("track count"));
    }
    let mut result = None;
    for track in tracks {
        let track_children = input.children(track.data, track.end, 2)?;
        let Some(mdia) = unique(&track_children, *b"mdia")? else {
            continue;
        };
        let media = input.children(mdia.data, mdia.end, 3)?;
        let handler =
            unique(&media, *b"hdlr")?.ok_or(GeometryError::Malformed("missing track handler"))?;
        let header = input.fixed::<12>(handler.data, handler.end)?;
        if header[..4] != [0; 4] {
            return Err(GeometryError::Unsupported("handler version or flags"));
        }
        if header[8..12] != *b"vide" {
            continue;
        }
        if result.is_some() {
            return Err(GeometryError::Ambiguous);
        }
        let minf =
            unique(&media, *b"minf")?.ok_or(GeometryError::Malformed("missing video minf"))?;
        let information = input.children(minf.data, minf.end, 4)?;
        let stbl = unique(&information, *b"stbl")?
            .ok_or(GeometryError::Malformed("missing video stbl"))?;
        let tables = input.children(stbl.data, stbl.end, 5)?;
        let stsd =
            unique(&tables, *b"stsd")?.ok_or(GeometryError::Malformed("missing video stsd"))?;
        result = Some(input.sample_descriptions(stsd)?);
    }
    result.ok_or(GeometryError::Unsupported("no supported video stream"))
}

#[derive(Clone, Copy)]
struct Atom {
    kind: [u8; 4],
    data: u64,
    end: u64,
}

fn unique(atoms: &[Atom], kind: [u8; 4]) -> Result<Option<Atom>, GeometryError> {
    let mut found = atoms.iter().filter(|atom| atom.kind == kind);
    let first = found.next().copied();
    if found.next().is_some() {
        return Err(GeometryError::Ambiguous);
    }
    Ok(first)
}

struct Input<'a, R> {
    reader: &'a mut R,
    end: u64,
    boxes: usize,
    metadata_read: usize,
}

impl<R: Read + Seek> Input<'_, R> {
    fn read(&mut self, at: u64, enclosing_end: u64, out: &mut [u8]) -> Result<(), GeometryError> {
        let end = at
            .checked_add(out.len() as u64)
            .ok_or(GeometryError::Malformed("read offset overflow"))?;
        if end > enclosing_end || end > self.end {
            return Err(GeometryError::Truncated);
        }
        self.metadata_read = self
            .metadata_read
            .checked_add(out.len())
            .ok_or(GeometryError::Limit("metadata bytes"))?;
        if self.metadata_read > MAX_METADATA_READ {
            return Err(GeometryError::Limit("metadata bytes"));
        }
        self.reader
            .seek(SeekFrom::Start(at))
            .map_err(GeometryError::Io)?;
        self.reader.read_exact(out).map_err(|error| {
            if error.kind() == io::ErrorKind::UnexpectedEof {
                GeometryError::Truncated
            } else {
                GeometryError::Io(error)
            }
        })
    }

    fn fixed<const N: usize>(&mut self, at: u64, end: u64) -> Result<[u8; N], GeometryError> {
        let mut out = [0u8; N];
        self.read(at, end, &mut out)?;
        Ok(out)
    }

    fn children(
        &mut self,
        mut at: u64,
        end: u64,
        depth: usize,
    ) -> Result<Vec<Atom>, GeometryError> {
        if depth > MAX_DEPTH {
            return Err(GeometryError::Limit("box depth"));
        }
        if at > end || end > self.end {
            return Err(GeometryError::Truncated);
        }
        let mut children = Vec::new();
        while at < end {
            self.boxes += 1;
            if self.boxes > MAX_BOXES {
                return Err(GeometryError::Limit("box count"));
            }
            let header = self.fixed::<8>(at, end)?;
            let small_size = u32::from_be_bytes(header[..4].try_into().unwrap());
            let (size, header_size) = match small_size {
                0 => (end - at, 8u64),
                1 => (u64::from_be_bytes(self.fixed::<8>(at + 8, end)?), 16),
                n => (u64::from(n), 8),
            };
            if size < header_size {
                return Err(GeometryError::Malformed("box smaller than header"));
            }
            let box_end = at
                .checked_add(size)
                .ok_or(GeometryError::Malformed("box offset overflow"))?;
            if box_end > end {
                return Err(GeometryError::Truncated);
            }
            children.push(Atom {
                kind: header[4..8].try_into().unwrap(),
                data: at + header_size,
                end: box_end,
            });
            at = box_end;
        }
        Ok(children)
    }

    fn sample_descriptions(&mut self, stsd: Atom) -> Result<VideoGeometry, GeometryError> {
        let header = self.fixed::<8>(stsd.data, stsd.end)?;
        if header[..4] != [0; 4] {
            return Err(GeometryError::Unsupported("stsd version or flags"));
        }
        let count = u32::from_be_bytes(header[4..8].try_into().unwrap()) as usize;
        if count == 0 {
            return Err(GeometryError::Malformed("empty video sample descriptions"));
        }
        if count > MAX_SAMPLE_ENTRIES {
            return Err(GeometryError::Limit("sample description count"));
        }
        let entries = self.children(stsd.data + 8, stsd.end, 6)?;
        if count != entries.len() {
            return Err(GeometryError::Malformed("stsd entry count mismatch"));
        }
        let mut result = None;
        for entry in entries {
            if entry.kind != *b"avc1" && entry.kind != *b"avc3" {
                return Err(GeometryError::Unsupported(
                    "video sample entry is not avc1/avc3",
                ));
            }
            // ISO/IEC 14496-12 VisualSampleEntry has 78 payload bytes before its
            // child boxes. Never search arbitrary bytes for an avcC signature.
            let fixed = self.fixed::<78>(entry.data, entry.end)?;
            if fixed[..6] != [0; 6] || fixed[6..8] == [0; 2] {
                return Err(GeometryError::Malformed("video sample entry header"));
            }
            if fixed[24..26] == [0; 2] || fixed[26..28] == [0; 2] {
                return Err(GeometryError::Malformed("zero sample entry dimensions"));
            }
            let children = self.children(entry.data + 78, entry.end, 7)?;
            let avcc =
                unique(&children, *b"avcC")?.ok_or(GeometryError::Malformed("missing avcC"))?;
            let length = avcc.end - avcc.data;
            if length > MAX_AVCC_BYTES as u64 {
                return Err(GeometryError::Limit("avcC bytes"));
            }
            let mut configuration = vec![0u8; length as usize];
            self.read(avcc.data, avcc.end, &mut configuration)?;
            agree(&mut result, parse_avcc(&configuration)?)?;
        }
        result.ok_or(GeometryError::Malformed("missing video configuration"))
    }
}

fn agree(result: &mut Option<VideoGeometry>, current: VideoGeometry) -> Result<(), GeometryError> {
    match result {
        Some(previous) if *previous != current => Err(GeometryError::Ambiguous),
        Some(_) => Ok(()),
        None => {
            *result = Some(current);
            Ok(())
        }
    }
}

struct Bytes<'a> {
    data: &'a [u8],
    at: usize,
}
impl<'a> Bytes<'a> {
    fn take(&mut self, length: usize) -> Result<&'a [u8], GeometryError> {
        let end = self
            .at
            .checked_add(length)
            .ok_or(GeometryError::Malformed("configuration offset overflow"))?;
        let bytes = self
            .data
            .get(self.at..end)
            .ok_or(GeometryError::Truncated)?;
        self.at = end;
        Ok(bytes)
    }
    fn byte(&mut self) -> Result<u8, GeometryError> {
        Ok(self.take(1)?[0])
    }
    fn nal(&mut self) -> Result<&'a [u8], GeometryError> {
        let length = u16::from_be_bytes(self.take(2)?.try_into().unwrap()) as usize;
        if length == 0 {
            return Err(GeometryError::Malformed("empty parameter set"));
        }
        self.take(length)
    }
}

fn extended_profile(profile: u8) -> bool {
    matches!(
        profile,
        100 | 110 | 122 | 244 | 44 | 83 | 86 | 118 | 128 | 138 | 139 | 134 | 135
    )
}

fn parse_avcc(data: &[u8]) -> Result<VideoGeometry, GeometryError> {
    let mut bytes = Bytes { data, at: 0 };
    let header = bytes.take(6)?;
    if header[0] != 1 {
        return Err(GeometryError::Unsupported("avcC configuration version"));
    }
    if header[4] & 0xfc != 0xfc || header[5] & 0xe0 != 0xe0 {
        return Err(GeometryError::Malformed("avcC reserved bits"));
    }
    if header[4] & 3 == 2 {
        return Err(GeometryError::Malformed("reserved NAL length size"));
    }
    let count = header[5] & 31;
    if count == 0 {
        return Err(GeometryError::Unsupported("avc3 requires an in-band SPS"));
    }
    let mut result = None;
    for _ in 0..count {
        let nal = bytes.nal()?;
        if nal.len() > MAX_SPS_BYTES {
            return Err(GeometryError::Limit("SPS bytes"));
        }
        if nal.get(1).copied() != Some(header[1]) {
            return Err(GeometryError::Malformed("avcC/SPS profile mismatch"));
        }
        agree(&mut result, parse_sps(nal)?)?;
    }
    let pps_count = bytes.byte()?;
    if pps_count == 0 {
        return Err(GeometryError::Malformed("missing PPS"));
    }
    for _ in 0..pps_count {
        let pps = bytes.nal()?;
        if pps.len() < 2 || pps[0] & 0x9f != 8 {
            return Err(GeometryError::Malformed("invalid PPS NAL"));
        }
    }
    if bytes.at != data.len() {
        if !extended_profile(header[1]) {
            return Err(GeometryError::Malformed("unexpected avcC extension"));
        }
        let extension = bytes.take(4)?;
        if extension[0] & 0xfc != 0xfc || extension[1] & 0xf8 != 0xf8 || extension[2] & 0xf8 != 0xf8
        {
            return Err(GeometryError::Malformed("avcC extension reserved bits"));
        }
        if extension[0] & 3 != 1 || extension[1] & 7 != 0 || extension[2] & 7 != 0 {
            return Err(GeometryError::Unsupported("avcC is not 8-bit 4:2:0"));
        }
        if extension[3] != 0 {
            return Err(GeometryError::Unsupported("SPS extensions"));
        }
        if bytes.at != data.len() {
            return Err(GeometryError::Malformed("trailing avcC data"));
        }
    }
    result.ok_or(GeometryError::Malformed("missing SPS"))
}

struct Bits<'a> {
    data: &'a [u8],
    at: usize,
}
impl Bits<'_> {
    fn take(&mut self, count: usize) -> Result<u32, GeometryError> {
        if count > 32 {
            return Err(GeometryError::Limit("bit field size"));
        }
        let end = self
            .at
            .checked_add(count)
            .ok_or(GeometryError::Limit("bit offset"))?;
        if end > self.data.len() * 8 {
            return Err(GeometryError::Truncated);
        }
        let mut value = 0u32;
        for bit in self.at..end {
            value = (value << 1) | u32::from((self.data[bit / 8] >> (7 - bit % 8)) & 1);
        }
        self.at = end;
        Ok(value)
    }
    fn flag(&mut self) -> Result<bool, GeometryError> {
        Ok(self.take(1)? != 0)
    }
    fn ue(&mut self) -> Result<u32, GeometryError> {
        // A malformed all-zero prefix cannot make the reader spin or shift by32.
        for zeros in 0..32 {
            if self.flag()? {
                return Ok(((1u32 << zeros) - 1) + self.take(zeros)?);
            }
        }
        Err(GeometryError::Limit("Exp-Golomb prefix"))
    }
    fn bounded_ue(&mut self, maximum: u32, reason: &'static str) -> Result<u32, GeometryError> {
        let value = self.ue()?;
        if value > maximum {
            return Err(GeometryError::Malformed(reason));
        }
        Ok(value)
    }
    fn se(&mut self) -> Result<i32, GeometryError> {
        let code = i64::from(self.ue()?);
        let value = if code & 1 != 0 {
            (code + 1) / 2
        } else {
            -(code / 2)
        };
        i32::try_from(value).map_err(|_| GeometryError::Malformed("signed Exp-Golomb overflow"))
    }
    fn trailing(&mut self) -> Result<(), GeometryError> {
        if !self.flag()? {
            return Err(GeometryError::Malformed("missing SPS stop bit"));
        }
        while self.at % 8 != 0 {
            if self.flag()? {
                return Err(GeometryError::Malformed("nonzero SPS alignment bit"));
            }
        }
        if self.at != self.data.len() * 8 {
            return Err(GeometryError::Malformed("trailing SPS bytes"));
        }
        Ok(())
    }
}

fn unescape(nal: &[u8]) -> Result<Vec<u8>, GeometryError> {
    if nal.len() > MAX_SPS_BYTES {
        return Err(GeometryError::Limit("SPS bytes"));
    }
    if nal.len() < 2 {
        return Err(GeometryError::Truncated);
    }
    if nal[0] & 0x9f != 7 {
        return Err(GeometryError::Malformed("invalid SPS NAL"));
    }
    let mut rbsp = Vec::with_capacity(nal.len() - 1);
    let mut zeros = 0;
    let mut at = 1;
    while at < nal.len() {
        let byte = nal[at];
        if zeros >= 2 {
            if byte <= 2 {
                return Err(GeometryError::Malformed("unescaped SPS byte sequence"));
            }
            if byte == 3 {
                let next = nal.get(at + 1).copied().ok_or(GeometryError::Truncated)?;
                if next > 3 {
                    return Err(GeometryError::Malformed(
                        "invalid emulation prevention byte",
                    ));
                }
                zeros = 0;
                at += 1;
                continue;
            }
        }
        rbsp.push(byte);
        zeros = if byte == 0 { zeros + 1 } else { 0 };
        at += 1;
    }
    Ok(rbsp)
}

fn scaling_list(bits: &mut Bits<'_>, count: usize) -> Result<(), GeometryError> {
    let mut last = 8i32;
    let mut next = 8i32;
    for _ in 0..count {
        if next != 0 {
            let delta = bits.se()?;
            if !(-128..=127).contains(&delta) {
                return Err(GeometryError::Malformed("scaling list delta"));
            }
            next = (last + delta + 256) % 256;
        }
        if next != 0 {
            last = next;
        }
    }
    Ok(())
}

fn parse_sps(nal: &[u8]) -> Result<VideoGeometry, GeometryError> {
    let rbsp = unescape(nal)?;
    let mut bits = Bits { data: &rbsp, at: 0 };
    let profile = bits.take(8)? as u8;
    if !matches!(profile, 66 | 77 | 100) {
        return Err(GeometryError::Unsupported(
            "H.264 profile is not Baseline/Main/High",
        ));
    }
    if bits.take(8)? & 3 != 0 {
        return Err(GeometryError::Malformed("SPS constraint reserved bits"));
    }
    let _level = bits.take(8)?;
    bits.bounded_ue(31, "SPS id")?;
    if extended_profile(profile) {
        if bits.ue()? != 1 {
            return Err(GeometryError::Unsupported("H.264 chroma is not 4:2:0"));
        }
        if bits.ue()? != 0 || bits.ue()? != 0 {
            return Err(GeometryError::Unsupported("H.264 depth is not 8-bit"));
        }
        if bits.flag()? {
            return Err(GeometryError::Unsupported("lossless transform bypass"));
        }
        if bits.flag()? {
            for list in 0..8 {
                if bits.flag()? {
                    scaling_list(&mut bits, if list < 6 { 16 } else { 64 })?;
                }
            }
        }
    } else if !matches!(profile, 66 | 77 | 88) {
        return Err(GeometryError::Unsupported("H.264 profile"));
    }
    bits.bounded_ue(12, "frame number width")?;
    match bits.ue()? {
        0 => {
            bits.bounded_ue(12, "picture order width")?;
        }
        1 => {
            bits.flag()?;
            bits.se()?;
            bits.se()?;
            let count = bits.bounded_ue(255, "picture order cycle")?;
            for _ in 0..count {
                bits.se()?;
            }
        }
        2 => {}
        _ => return Err(GeometryError::Malformed("picture order count type")),
    }
    bits.bounded_ue(16, "reference frame count")?;
    bits.flag()?; // gaps_in_frame_num_value_allowed_flag
    let width_mbs = bits
        .ue()?
        .checked_add(1)
        .ok_or(GeometryError::Malformed("SPS width overflow"))?;
    let height_mbs = bits
        .ue()?
        .checked_add(1)
        .ok_or(GeometryError::Malformed("SPS height overflow"))?;
    if !bits.flag()? {
        return Err(GeometryError::Unsupported("interlaced H.264"));
    }
    bits.flag()?; // direct_8x8_inference_flag
    let (coded_width, coded_height) = (
        width_mbs
            .checked_mul(16)
            .ok_or(GeometryError::Malformed("SPS width overflow"))?,
        height_mbs
            .checked_mul(16)
            .ok_or(GeometryError::Malformed("SPS height overflow"))?,
    );
    if coded_width > MAX_CODED_WIDTH || coded_height > MAX_CODED_HEIGHT {
        return Err(GeometryError::Unsupported(
            "coded dimensions exceed 960x544",
        ));
    }
    let mut crop = [0u32; 4];
    if bits.flag()? {
        // H.264 4:2:0 progressive: SubWidthC=2, SubHeightC=2 and
        // (2-frame_mbs_only_flag)=1, hence both crop units are two pixels.
        for value in &mut crop {
            *value = bits
                .ue()?
                .checked_mul(2)
                .ok_or(GeometryError::Malformed("crop overflow"))?;
        }
    }
    let visible_width = coded_width
        .checked_sub(crop[0])
        .and_then(|n| n.checked_sub(crop[1]))
        .filter(|n| *n != 0)
        .ok_or(GeometryError::Malformed("horizontal crop outside frame"))?;
    let visible_height = coded_height
        .checked_sub(crop[2])
        .and_then(|n| n.checked_sub(crop[3]))
        .filter(|n| *n != 0)
        .ok_or(GeometryError::Malformed("vertical crop outside frame"))?;
    let sar = if bits.flag()? { vui(&mut bits)? } else { None };
    bits.trailing()?;
    Ok(VideoGeometry {
        coded_width,
        coded_height,
        crop_left: crop[0],
        crop_right: crop[1],
        crop_top: crop[2],
        crop_bottom: crop[3],
        visible_width,
        visible_height,
        sar,
    })
}

fn ratio(mut numerator: u32, mut denominator: u32) -> Result<SampleAspectRatio, GeometryError> {
    if numerator == 0 || denominator == 0 {
        return Err(GeometryError::Malformed("zero sample aspect ratio"));
    }
    let (mut a, mut b) = (numerator, denominator);
    while b != 0 {
        (a, b) = (b, a % b);
    }
    numerator /= a;
    denominator /= a;
    Ok(SampleAspectRatio {
        numerator,
        denominator,
    })
}

fn vui(bits: &mut Bits<'_>) -> Result<Option<SampleAspectRatio>, GeometryError> {
    const RATIOS: [(u32, u32); 17] = [
        (0, 0),
        (1, 1),
        (12, 11),
        (10, 11),
        (16, 11),
        (40, 33),
        (24, 11),
        (20, 11),
        (32, 11),
        (80, 33),
        (18, 11),
        (15, 11),
        (64, 33),
        (160, 99),
        (4, 3),
        (3, 2),
        (2, 1),
    ];
    let sar = if bits.flag()? {
        match bits.take(8)? {
            0 => None,
            id @ 1..=16 => {
                let (n, d) = RATIOS[id as usize];
                Some(ratio(n, d)?)
            }
            255 => Some(ratio(bits.take(16)?, bits.take(16)?)?),
            _ => return Err(GeometryError::Unsupported("reserved sample aspect ratio")),
        }
    } else {
        None
    };
    if bits.flag()? {
        bits.flag()?;
    } // overscan
    if bits.flag()? {
        bits.take(3)?;
        bits.flag()?;
        if bits.flag()? {
            bits.take(8)?;
            bits.take(8)?;
            bits.take(8)?;
        }
    }
    if bits.flag()? {
        bits.bounded_ue(5, "top chroma sample location")?;
        bits.bounded_ue(5, "bottom chroma sample location")?;
    }
    if bits.flag()? {
        let units = bits.take(32)?;
        let scale = bits.take(32)?;
        if units == 0 || scale == 0 {
            return Err(GeometryError::Malformed("zero VUI timing"));
        }
        bits.flag()?;
    }
    let nal_hrd = bits.flag()?;
    if nal_hrd {
        hrd(bits)?;
    }
    let vcl_hrd = bits.flag()?;
    if vcl_hrd {
        hrd(bits)?;
    }
    if nal_hrd || vcl_hrd {
        bits.flag()?;
    }
    bits.flag()?; // pic_struct_present_flag
    if bits.flag()? {
        bits.flag()?;
        bits.bounded_ue(16, "max bytes per picture denominator")?;
        bits.bounded_ue(16, "max bits per macroblock denominator")?;
        bits.bounded_ue(16, "horizontal motion vector length")?;
        bits.bounded_ue(16, "vertical motion vector length")?;
        let reorder = bits.bounded_ue(16, "reorder frame count")?;
        let buffering = bits.bounded_ue(16, "decoded frame buffer count")?;
        if reorder > buffering {
            return Err(GeometryError::Malformed("VUI reorder exceeds buffering"));
        }
    }
    Ok(sar)
}

fn hrd(bits: &mut Bits<'_>) -> Result<(), GeometryError> {
    let count = bits.bounded_ue(31, "HRD CPB count")? + 1;
    bits.take(4)?;
    bits.take(4)?;
    for _ in 0..count {
        bits.ue()?;
        bits.ue()?;
        bits.flag()?;
    }
    for _ in 0..4 {
        bits.take(5)?;
    }
    Ok(())
}
