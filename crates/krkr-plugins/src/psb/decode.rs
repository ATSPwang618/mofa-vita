//! PSB object decoding. Owned data only; no VM objects enter storage workers.
use std::{collections::BTreeMap, io::Read, ops::Range, sync::Arc};

pub(crate) enum Node {
    Void,
    Int(i64),
    Real(f64),
    String(String),
    Bytes(Range<usize>),
    Array(Vec<Node>),
    Object(Vec<(String, Node)>),
}
pub(crate) struct Document {
    pub bytes: Arc<[u8]>,
    pub root: Node,
    pub resources: BTreeMap<String, Range<usize>>,
}
type Result<T> = std::result::Result<T, &'static str>;
const LIMIT: usize = 64 * 1024 * 1024;
struct Reader<'a> {
    bytes: &'a [u8],
    pos: usize,
}
impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        let end = self.pos.checked_add(n).ok_or("PSB offset overflow")?;
        let bytes = self.bytes.get(self.pos..end).ok_or("truncated PSB")?;
        self.pos = end;
        Ok(bytes)
    }
    fn uint(&mut self, n: usize) -> Result<u64> {
        if n > 8 {
            return Err("invalid PSB integer width");
        }
        let mut value = [0; 8];
        value[..n].copy_from_slice(self.take(n)?);
        Ok(u64::from_le_bytes(value))
    }
    fn byte(&mut self) -> Result<u8> {
        Ok(self.take(1)?[0])
    }
    fn array(&mut self) -> Result<Vec<u32>> {
        let tag = self.byte()?;
        self.array_body(tag)
    }
    fn array_body(&mut self, tag: u8) -> Result<Vec<u32>> {
        if !(0x0d..=0x14).contains(&tag) {
            return Err("invalid PSB array");
        }
        let count =
            usize::try_from(self.uint((tag - 12) as usize)?).map_err(|_| "PSB array too large")?;
        let width = self
            .byte()?
            .checked_sub(12)
            .ok_or("invalid PSB array width")? as usize;
        if !(1..=4).contains(&width) || count > LIMIT / 4 {
            return Err("PSB array too large");
        }
        // Validate before allocating from an untrusted count.
        let bytes = self.take(count.checked_mul(width).ok_or("PSB array overflow")?)?;
        Ok(bytes
            .chunks_exact(width)
            .map(|chunk| {
                let mut word = [0; 4];
                word[..width].copy_from_slice(chunk);
                u32::from_le_bytes(word)
            })
            .collect())
    }
    fn string(&mut self) -> Result<String> {
        let bytes = self
            .bytes
            .get(self.pos..)
            .ok_or("invalid PSB string offset")?;
        let Some(end) = bytes.iter().position(|&c| c == 0) else {
            return Ok(String::new()); // Reference returns an empty string at EOF.
        };
        Ok(String::from_utf8_lossy(&bytes[..end]).into_owned())
    }
}
struct Parser<'a> {
    bytes: &'a [u8],
    version: u16,
    names: Vec<String>,
    strings: Vec<u32>,
    string_base: usize,
    chunks: Vec<u32>,
    lengths: Vec<u32>,
    chunk_base: usize,
    extra_chunks: Vec<u32>,
    extra_lengths: Vec<u32>,
    extra_base: usize,
    resources: BTreeMap<String, Range<usize>>,
    remaining: usize,
    cancelled: &'a dyn Fn() -> bool,
}
impl Parser<'_> {
    fn node(&mut self, pos: usize, key: &str, depth: usize) -> Result<Node> {
        if depth > 128 || self.remaining == 0 {
            return Err("PSB object limit exceeded");
        }
        if (self.cancelled)() {
            return Err("PSB decode cancelled");
        }
        self.remaining -= 1;
        let mut r = Reader {
            bytes: self.bytes,
            pos,
        };
        let tag = r.byte()?;
        Ok(match tag {
            0 | 1 => Node::Void,
            2 | 3 => Node::Int(i64::from(tag == 3)),
            4 => Node::Int(0),
            5..=12 => {
                let width = (tag - 4) as usize;
                let mut word = r.uint(width)?;
                // The reference has these exact masks for its odd-width signed numbers.
                let mask = match width {
                    3 => 0xffff_0000,
                    5 => 0xffff_ffff_0000_0000,
                    6 => 0xffff_ff00_0000_0000,
                    7 => 0xffff_0000_0000_0000,
                    _ => 0,
                };
                if word & (1 << (width * 8 - 1)) != 0 {
                    word |= mask;
                }
                let value = match width {
                    1 => word as i8 as i64,
                    2 => word as i16 as i64,
                    3 | 4 => word as i32 as i64,
                    _ => word as i64,
                };
                Node::Int(value)
            }
            0x0d..=0x14 => Node::Array(
                r.array_body(tag)?
                    .into_iter()
                    .map(|n| Node::Int(n as i32 as i64))
                    .collect(),
            ),
            0x15..=0x18 => {
                let index = r.uint((tag - 0x14) as usize)? as usize;
                let offset = *self.strings.get(index).ok_or("invalid PSB string index")?;
                r.pos = self
                    .string_base
                    .checked_add(offset as usize)
                    .ok_or("PSB string overflow")?;
                Node::String(r.string()?)
            }
            0x19..=0x1c | 0x22..=0x25 => {
                if key.is_empty() {
                    return Ok(Node::Void);
                }
                let extra = tag >= 0x22;
                let index = r.uint((tag - if extra { 0x21 } else { 0x18 }) as usize)? as usize;
                let (chunks, lengths, base) = if extra {
                    (&self.extra_chunks, &self.extra_lengths, self.extra_base)
                } else {
                    (&self.chunks, &self.lengths, self.chunk_base)
                };
                let Some((&offset, &len)) = chunks.get(index).zip(lengths.get(index)) else {
                    return Ok(Node::Void);
                };
                let start = base
                    .checked_add(offset as usize)
                    .ok_or("PSB chunk overflow")?;
                let end = start
                    .checked_add(len as usize)
                    .ok_or("PSB chunk overflow")?;
                if self.bytes.get(start..end).is_none() {
                    return Ok(Node::Void);
                }
                self.resources.entry(key.into()).or_insert(start..end);
                Node::Bytes(start..end)
            }
            0x1d => Node::Real(0.),
            0x1e => Node::Real(f32::from_bits(r.uint(4)? as u32) as f64),
            0x1f => Node::Real(f64::from_bits(r.uint(8)?)),
            0x20 => {
                let offsets = r.array()?;
                let base = r.pos;
                let mut out = Vec::new();
                for offset in offsets {
                    out.push(
                        self.node(
                            base.checked_add(offset as usize)
                                .ok_or("PSB object overflow")?,
                            "",
                            depth + 1,
                        )?,
                    );
                }
                Node::Array(out)
            }
            0x21 => {
                // v1 name-list iteration and refreshListInfo in the reference access
                // uninitialized vectors. Decode the intended key-prefix representation safely.
                let (keys, offsets) = if self.version == 1 {
                    (None, r.array()?)
                } else {
                    (Some(r.array()?), r.array()?)
                };
                if keys
                    .as_ref()
                    .is_some_and(|keys| keys.len() != offsets.len())
                {
                    return Err("PSB dictionary count mismatch");
                }
                let base = r.pos;
                let mut out = Vec::new();
                for (i, offset) in offsets.into_iter().enumerate() {
                    let mut pos = base
                        .checked_add(offset as usize)
                        .ok_or("PSB object overflow")?;
                    let index = if let Some(keys) = &keys {
                        keys[i] as usize
                    } else {
                        let mut key = Reader {
                            bytes: self.bytes,
                            pos,
                        };
                        let tag = key.byte()?;
                        if !(0x11..=0x14).contains(&tag) {
                            return Err("invalid PSB v1 key");
                        }
                        let index = key.uint((tag - 0x10) as usize)? as usize;
                        pos = key.pos;
                        index
                    };
                    let name = self
                        .names
                        .get(index)
                        .ok_or("invalid PSB name index")?
                        .clone();
                    let node = self.node(pos, &name, depth + 1)?;
                    out.push((name, node));
                }
                Node::Object(out)
            }
            _ => Node::Void,
        })
    }
}
pub(crate) fn decode(mut bytes: Vec<u8>, cancelled: &dyn Fn() -> bool) -> Result<Document> {
    if bytes.len() > LIMIT {
        return Err("PSB input limit exceeded");
    }
    if bytes
        .get(..3)
        .is_some_and(|s| s.eq_ignore_ascii_case(b"mdf"))
    {
        let mut header = Reader {
            bytes: &bytes,
            pos: 4,
        };
        let size = header.uint(4)? as usize;
        if size > LIMIT {
            return Err("MDF size limit exceeded");
        }
        let mut decoder = flate2::read::ZlibDecoder::new(&bytes[8..]);
        let mut output = Vec::new();
        let mut block = [0; 16384];
        loop {
            if cancelled() {
                return Err("PSB decode cancelled");
            }
            let n = decoder.read(&mut block).map_err(|_| "invalid MDF stream")?;
            if n == 0 {
                break;
            }
            if output.len() + n > size {
                return Err("MDF size mismatch");
            }
            output.extend_from_slice(&block[..n]);
        }
        bytes = output;
    }
    let mut r = Reader {
        bytes: &bytes,
        pos: 0,
    };
    if &r.take(4)?[..3] != b"PSB" {
        return Err("invalid PSB signature");
    }
    let version = r.uint(2)? as u16;
    if !(1..=4).contains(&version) {
        return Err("unsupported PSB version");
    }
    r.uint(2)?;
    let encrypt = r.uint(4)? as usize;
    let names_base = r.uint(4)? as usize;
    let strings_base = r.uint(4)? as usize;
    let string_base = r.uint(4)? as usize;
    let chunks_base = r.uint(4)? as usize;
    let lengths_base = r.uint(4)? as usize;
    let chunk_base = r.uint(4)? as usize;
    let root_base = r.uint(4)? as usize;
    if version > 2 {
        r.uint(4)?;
    }
    let (extra_offsets, extra_sizes, extra_base) = if version > 3 {
        (
            r.uint(4)? as usize,
            r.uint(4)? as usize,
            r.uint(4)? as usize,
        )
    } else {
        (0, 0, 0)
    };
    let (extra_chunks, extra_lengths) = if version > 3 && extra_offsets != 0 && extra_sizes != 0 {
        r.pos = extra_offsets;
        let offsets = r.array()?;
        r.pos = extra_sizes;
        (offsets, r.array()?)
    } else {
        (Vec::new(), Vec::new())
    };
    r.pos = strings_base;
    let strings = r.array()?;
    let mut names = Vec::new();
    if version == 1 {
        r.pos = if encrypt >= bytes.len() { 40 } else { encrypt };
        for offset in r.array()? {
            r.pos = names_base
                .checked_add(offset as usize)
                .ok_or("PSB name overflow")?;
            names.push(r.string()?);
        }
    } else {
        r.pos = names_base;
        let charset = r.array()?;
        let parents = r.array()?;
        let indices = r.array()?;
        for index in indices {
            if cancelled() {
                return Err("PSB decode cancelled");
            }
            let mut chr = *parents
                .get(index as usize)
                .ok_or("invalid PSB name index")?;
            let mut name = Vec::new();
            while chr != 0 {
                if name.len() > 65536 {
                    return Err("cyclic PSB name trie");
                }
                let code = *parents.get(chr as usize).ok_or("invalid PSB name parent")?;
                let base = *charset.get(code as usize).ok_or("invalid PSB charset")?;
                name.push(chr.wrapping_sub(base) as u8);
                chr = code;
            }
            name.reverse();
            names.push(String::from_utf8_lossy(&name).into_owned());
        }
    }
    r.pos = chunks_base;
    let chunks = r.array()?;
    r.pos = lengths_base;
    let lengths = r.array()?;
    let mut parser = Parser {
        bytes: &bytes,
        version,
        names,
        strings,
        string_base,
        chunks,
        lengths,
        chunk_base,
        extra_chunks,
        extra_lengths,
        extra_base,
        resources: BTreeMap::new(),
        remaining: 1_000_000,
        cancelled,
    };
    let root = parser.node(root_base, "root", 0)?;
    let resources = parser.resources;
    Ok(Document {
        bytes: bytes.into(),
        root,
        resources,
    })
}
