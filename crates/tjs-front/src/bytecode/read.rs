//! TJS2100 container reader. Chunk sizes follow ScriptBlock::ExportByteCode:
//! DATA/OBJS include their headers; individual TJS2 object sizes exclude them.
use super::{Limits, instruction};
use tjs_core::{Diagnostic, Phase};

pub(super) fn error(offset: usize, message: impl std::fmt::Display) -> Diagnostic {
    Diagnostic::new(
        Phase::Verify,
        None,
        format!("TJS2100 byte {offset}: {message}"),
    )
}

struct Reader<'a> {
    bytes: &'a [u8],
    base: usize,
    pos: usize,
}

impl<'a> Reader<'a> {
    fn at(&self) -> usize {
        self.base + self.pos
    }
    fn take(&mut self, length: usize) -> Result<&'a [u8], Diagnostic> {
        let end = self
            .pos
            .checked_add(length)
            .filter(|&end| end <= self.bytes.len())
            .ok_or_else(|| error(self.at(), "truncated section"))?;
        let bytes = &self.bytes[self.pos..end];
        self.pos = end;
        Ok(bytes)
    }
    fn i32(&mut self) -> Result<i32, Diagnostic> {
        Ok(i32::from_le_bytes(
            self.take(4)?.try_into().expect("four bytes"),
        ))
    }
    fn count(&mut self, width: usize) -> Result<usize, Diagnostic> {
        let at = self.at();
        let count = usize::try_from(self.i32()?).map_err(|_| error(at, "negative count"))?;
        if count > (self.bytes.len() - self.pos) / width {
            return Err(error(at, "count exceeds section bounds"));
        }
        Ok(count)
    }
    fn table(&mut self, width: usize) -> Result<&'a [u8], Diagnostic> {
        let count = self.count(width)?;
        let bytes = self.take(count * width)?;
        self.take((4 - bytes.len() % 4) % 4)?;
        Ok(bytes)
    }
    fn section(&mut self, tag: &[u8; 4], includes_header: bool) -> Result<Self, Diagnostic> {
        let at = self.at();
        if self.take(4)? != tag {
            return Err(error(at, "unexpected section tag"));
        }
        let length =
            usize::try_from(self.i32()?).map_err(|_| error(at + 4, "negative section size"))?;
        let length = length
            .checked_sub(if includes_header { 8 } else { 0 })
            .ok_or_else(|| error(at + 4, "section smaller than its header"))?;
        let base = self.at();
        Ok(Self {
            bytes: self.take(length)?,
            base,
            pos: 0,
        })
    }
    fn finish(self) -> Result<(), Diagnostic> {
        if self.pos != self.bytes.len() {
            return Err(error(self.at(), "trailing section data"));
        }
        Ok(())
    }
}

/// Tables borrow the input. Only variable-length entry offsets are indexed;
/// decoded strings/octets are allocated later, once per imported constant.
pub(super) struct Data<'a> {
    pub bytes: &'a [u8],
    pub shorts: &'a [u8],
    pub integers: &'a [u8],
    pub longs: &'a [u8],
    pub reals: &'a [u8],
    pub strings: Vec<&'a [u8]>,
    pub octets: Vec<&'a [u8]>,
}

impl Data<'_> {
    pub fn string(&self, index: usize) -> Vec<u16> {
        // The reference loader reconstructs a C string, even though the file
        // contains a counted UTF-16 payload. Surrogate code units stay intact.
        self.strings[index]
            .chunks_exact(2)
            .map(|b| u16::from_le_bytes([b[0], b[1]]))
            .take_while(|&u| u != 0)
            .collect()
    }
}

#[derive(Clone, Copy, Debug)]
pub(super) enum Value {
    Void,
    Null,
    Function(usize),
    String(usize),
    Octet(usize),
    Int(i64),
    Real(u64),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Context {
    Top,
    Function,
    Expression,
    Property,
    Setter,
    Getter,
    Class,
    Super,
}

pub(super) struct Object {
    pub offset: usize,
    pub parent: Option<usize>,
    pub name: Option<usize>,
    pub context: Context,
    pub variables: u32,
    pub reserve: u32,
    pub frames: u32,
    pub arguments: u32,
    pub unnamed: Option<u32>,
    pub collapse: Option<u32>,
    pub setter: Option<usize>,
    pub getter: Option<usize>,
    pub super_getter: Option<usize>,
    pub source_positions: Vec<(u32, u32)>,
    pub code_offset: usize,
    pub code: Vec<i16>,
    pub instructions: Vec<instruction::Decoded>,
    pub values: Vec<Value>,
    pub super_entries: Vec<u32>,
    /// Exported on a child record, installed on that record's parent.
    pub members: Vec<(usize, usize)>,
}

pub(super) struct File<'a> {
    pub data: Data<'a>,
    pub top: Option<usize>,
    pub objects: Vec<Object>,
}

fn index(value: i32, count: usize, at: usize) -> Result<usize, Diagnostic> {
    usize::try_from(value)
        .ok()
        .filter(|&i| i < count)
        .ok_or_else(|| error(at, "table index is out of bounds"))
}
fn optional(value: i32, count: usize, at: usize) -> Result<Option<usize>, Diagnostic> {
    if value == -1 {
        Ok(None)
    } else {
        index(value, count, at).map(Some)
    }
}
fn nonnegative(value: i32, at: usize) -> Result<u32, Diagnostic> {
    u32::try_from(value).map_err(|_| error(at, "negative frame/argument size"))
}

pub(super) fn read<'a>(bytes: &'a [u8], limits: &Limits) -> Result<File<'a>, Diagnostic> {
    if bytes.len() > limits.max_input_bytes {
        return Err(error(0, "input exceeds import byte limit"));
    }
    let mut input = Reader {
        bytes,
        base: 0,
        pos: 0,
    };
    if input.take(8)? != b"TJS2100\0" {
        return Err(error(0, "unsupported bytecode signature/version"));
    }
    if usize::try_from(input.i32()?).ok() != Some(bytes.len()) {
        return Err(error(8, "declared file size differs from input"));
    }
    let mut section = input.section(b"DATA", true)?;
    let bytes = section.table(1)?;
    let shorts = section.table(2)?;
    let integers = section.table(4)?;
    let longs = section.table(8)?;
    let reals = section.table(8)?;
    let variable_table = |section: &mut Reader<'a>| -> Result<usize, Diagnostic> {
        let count = section.count(4)?;
        if count > limits.max_data_entries {
            return Err(error(section.at(), "too many data entries"));
        }
        Ok(count)
    };
    let string_count = variable_table(&mut section)?;
    let mut strings = Vec::with_capacity(string_count);
    for _ in 0..string_count {
        strings.push(section.table(2)?);
    }
    let octet_count = variable_table(&mut section)?;
    let mut octets = Vec::with_capacity(octet_count);
    for _ in 0..octet_count {
        octets.push(section.table(1)?);
    }
    section.finish()?;
    let data = Data {
        bytes,
        shorts,
        integers,
        longs,
        reals,
        strings,
        octets,
    };
    let mut section = input.section(b"OBJS", true)?;
    let top = section.i32()?;
    let count = section.count(8 + 16 * 4)?;
    if count > limits.max_objects {
        return Err(error(section.at(), "too many code objects"));
    }
    let top = optional(top, count, section.base)?;
    let mut objects = Vec::with_capacity(count);
    let mut total_words = 0_usize;
    let mut total_values = 0_usize;
    for _ in 0..count {
        let mut record = section.section(b"TJS2", false)?;
        let offset = record.base;
        let parent = optional(record.i32()?, count, offset)?;
        let name = optional(record.i32()?, data.strings.len(), offset + 4)?;
        let context = match record.i32()? {
            0 => Context::Top,
            1 => Context::Function,
            2 => Context::Expression,
            3 => Context::Property,
            4 => Context::Setter,
            5 => Context::Getter,
            6 => Context::Class,
            7 => Context::Super,
            _ => return Err(error(offset + 8, "unknown context kind")),
        };
        let variables = nonnegative(record.i32()?, record.at() - 4)?;
        let reserve = nonnegative(record.i32()?, record.at() - 4)?;
        let frames = nonnegative(record.i32()?, record.at() - 4)?;
        let arguments = nonnegative(record.i32()?, record.at() - 4)?;
        let locals = variables
            .checked_add(reserve)
            .filter(|&n| n <= 32768)
            .ok_or_else(|| error(offset, "local register range exceeds i16 encoding"))?;
        let local_values = locals.saturating_sub(2);
        if (context != Context::Property && reserve < 2)
            || (context == Context::Property
                && (reserve != 0 || variables != 0 || frames != 0 || arguments != 0))
            || frames > 32767
            || arguments > local_values
        {
            return Err(error(
                offset,
                "invalid local/temporary/argument register layout",
            ));
        }
        let unnamed =
            optional(record.i32()?, arguments as usize + 1, record.at() - 4)?.map(|n| n as u32);
        let collapse =
            optional(record.i32()?, local_values as usize, record.at() - 4)?.map(|n| n as u32);
        let setter = optional(record.i32()?, count, record.at() - 4)?;
        let getter = optional(record.i32()?, count, record.at() - 4)?;
        let super_getter = optional(record.i32()?, count, record.at() - 4)?;
        let position_count = record.count(8)?;
        if position_count > limits.max_code_words {
            return Err(error(record.at(), "too many source positions"));
        }
        let mut source_positions = Vec::with_capacity(position_count);
        for _ in 0..position_count {
            source_positions.push((nonnegative(record.i32()?, record.at() - 4)?, 0));
        }
        for position in &mut source_positions {
            position.1 = nonnegative(record.i32()?, record.at() - 4)?;
        }
        let word_count = record.count(2)?;
        total_words = total_words
            .checked_add(word_count)
            .filter(|&n| n <= limits.max_code_words)
            .ok_or_else(|| error(record.at(), "code exceeds import word limit"))?;
        let code_offset = record.at();
        let code = record
            .take(word_count * 2)?
            .chunks_exact(2)
            .map(|b| i16::from_le_bytes([b[0], b[1]]))
            .collect();
        record.take((word_count % 2) * 2)?;
        let value_count = record.count(4)?;
        total_values = total_values
            .checked_add(value_count)
            .filter(|&n| n <= limits.max_data_entries)
            .ok_or_else(|| error(record.at(), "object data exceeds import entry limit"))?;
        let mut values = Vec::with_capacity(value_count);
        for _ in 0..value_count {
            let at = record.at();
            let raw = record.take(4)?;
            let kind = i16::from_le_bytes([raw[0], raw[1]]);
            let item = i32::from(i16::from_le_bytes([raw[2], raw[3]]));
            let get = |table: &[u8], width| -> Result<usize, Diagnostic> {
                index(item, table.len() / width, at + 2).map(|i| i * width)
            };
            values.push(match kind {
                0 => Value::Void,
                1 => Value::Null,
                2 | 10 => Value::Function(index(item, count, at + 2)?),
                3 => Value::String(index(item, data.strings.len(), at + 2)?),
                4 => Value::Octet(index(item, data.octets.len(), at + 2)?),
                5 => {
                    let i = get(data.reals, 8)?;
                    Value::Real(u64::from_le_bytes(
                        data.reals[i..i + 8].try_into().expect("real entry"),
                    ))
                }
                6 => Value::Int(i64::from(data.bytes[get(data.bytes, 1)?] as i8)),
                7 => {
                    let i = get(data.shorts, 2)?;
                    Value::Int(i64::from(i16::from_le_bytes(
                        data.shorts[i..i + 2].try_into().expect("short entry"),
                    )))
                }
                8 => {
                    let i = get(data.integers, 4)?;
                    Value::Int(i64::from(i32::from_le_bytes(
                        data.integers[i..i + 4].try_into().expect("integer entry"),
                    )))
                }
                9 => {
                    let i = get(data.longs, 8)?;
                    Value::Int(i64::from_le_bytes(
                        data.longs[i..i + 8].try_into().expect("long entry"),
                    ))
                }
                _ => return Err(error(at, "unknown constant type")),
            });
        }
        let entry_count = record.count(4)?;
        if entry_count > limits.max_objects {
            return Err(error(record.at(), "too many superclass entries"));
        }
        let mut super_entries = Vec::with_capacity(entry_count);
        for _ in 0..entry_count {
            super_entries.push(nonnegative(record.i32()?, record.at() - 4)?);
        }
        let member_count = record.count(8)?;
        if member_count > limits.max_data_entries {
            return Err(error(record.at(), "too many exported members"));
        }
        let mut members = Vec::with_capacity(member_count);
        for _ in 0..member_count {
            let name = index(record.i32()?, data.strings.len(), record.at() - 4)?;
            let object = index(record.i32()?, count, record.at() - 4)?;
            members.push((name, object));
        }
        record.finish()?;
        objects.push(Object {
            offset,
            parent,
            name,
            context,
            variables,
            reserve,
            frames,
            arguments,
            unnamed,
            collapse,
            setter,
            getter,
            super_getter,
            source_positions,
            code_offset,
            code,
            instructions: Vec::new(),
            values,
            super_entries,
            members,
        });
    }
    section.finish()?;
    input.finish()?;
    for object in &mut objects {
        object.instructions = instruction::decode(object)?;
    }
    for object in &objects {
        for (target, expected) in [
            (object.setter, Context::Setter),
            (object.getter, Context::Getter),
            (object.super_getter, Context::Super),
        ] {
            if target.is_some_and(|i| objects[i].context != expected) {
                return Err(error(
                    object.offset,
                    "accessor reference has the wrong context kind",
                ));
            }
        }
        if (!object.members.is_empty() && object.parent.is_none())
            || ((object.setter.is_some() || object.getter.is_some())
                && object.context != Context::Property)
            || (object.super_getter.is_some() && object.context != Context::Class)
        {
            return Err(error(object.offset, "inconsistent context relationships"));
        }
    }
    if top.is_some_and(|i| objects[i].context != Context::Top || objects[i].arguments != 0) {
        return Err(error(12, "entry must be a parameterless top-level context"));
    }
    let mut colors = vec![0_u8; objects.len()];
    for start in 0..objects.len() {
        let mut current = Some(start);
        while let Some(index) = current {
            match colors[index] {
                1 => return Err(error(objects[index].offset, "cyclic parent relationship")),
                2 => break,
                _ => {
                    colors[index] = 1;
                    current = objects[index].parent;
                }
            }
        }
        let mut current = Some(start);
        while let Some(index) = current.filter(|&i| colors[i] == 1) {
            colors[index] = 2;
            current = objects[index].parent;
        }
    }
    Ok(File { data, top, objects })
}
