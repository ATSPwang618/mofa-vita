//! TJS format parsing and UTF-16 fields resume in the ordinary VM budget.
//! fish-printf owns numeric formatting; the format and string inputs stay rooted.
use crate::{
    Heap, NativeContinuation, NativeCx, NativeError, NativeResult, NativeStep, StrId, Trace, Value,
    value,
};

const WORK: usize = 1024;

#[derive(Clone, Copy)]
enum Parse {
    Flag,
    Zero,
    WidthStart,
    Width,
    Precision,
    PrecisionStart,
    PrecisionDigits,
    Kind,
}
struct Field {
    state: Parse,
    flag: u16,
    width: u32,
    precision: u32,
    width_indirect: bool,
    precision_indirect: bool,
    numeric: [u8; 68],
    length: usize,
}
impl Field {
    fn new() -> Self {
        Self {
            state: Parse::Flag,
            flag: 0,
            width: 0,
            precision: 0,
            width_indirect: false,
            precision_indirect: false,
            numeric: {
                let mut bytes = [0; 68];
                bytes[0] = b'%';
                bytes
            },
            length: 1,
        }
    }
    fn consume(&mut self, unit: u16) {
        if self.length < self.numeric.len() {
            self.numeric[self.length] = unit as u8;
        }
        self.length += 1;
    }
}
struct Text {
    source: StrId,
    precision: usize,
    width: usize,
    offset: usize,
    position: usize,
    ended: bool,
}
pub(super) struct Format {
    source: StrId,
    args: Vec<Value>,
    argument: usize,
    position: usize,
    field: Option<Field>,
    text: Option<Text>,
    output: Vec<u16>,
    first_nul: Option<usize>,
}
impl Trace for Format {
    fn trace(&self, visit: &mut dyn FnMut(Value)) {
        self.source.trace(visit);
        self.args.trace(visit);
        if let Some(text) = &self.text {
            text.source.trace(visit);
        }
    }
}
impl NativeContinuation for Format {
    fn resume(self: Box<Self>, cx: &mut NativeCx<'_>, _: Value) -> NativeResult<NativeStep> {
        self.advance(cx.heap_mut())
    }
}
pub(super) fn start(heap: &mut Heap, source: StrId, args: &[Value]) -> NativeResult<NativeStep> {
    Format {
        source,
        args: args.to_vec(),
        argument: 0,
        position: 0,
        field: None,
        text: None,
        output: Vec::new(),
        first_nul: None,
    }
    .advance(heap)
}
impl Format {
    fn next(&mut self) -> NativeResult<Value> {
        let value = self
            .args
            .get(self.argument)
            .copied()
            .ok_or(NativeError::Missing(self.argument + 1))?;
        self.argument += 1;
        Ok(value)
    }
    fn reserve(&mut self, count: usize) -> NativeResult<()> {
        self.output
            .try_reserve(count)
            .map_err(|_| NativeError::Message("format allocation failed"))
    }
    fn push(&mut self, unit: u16) -> NativeResult<()> {
        self.reserve(1)?;
        if unit == 0 && self.first_nul.is_none() {
            self.first_nul = Some(self.output.len());
        }
        self.output.push(unit);
        Ok(())
    }
    fn text_batch(&mut self, heap: &Heap, text: &mut Text, budget: usize) -> NativeResult<usize> {
        let at = text.position;
        let end = (at + budget).min(text.width);
        let end = if at < text.offset {
            end.min(text.offset)
        } else if at < text.offset + text.precision {
            end.min(text.offset + text.precision)
        } else {
            end
        };
        let count = end - at;
        self.reserve(count)?;
        if at < text.offset || at >= text.offset + text.precision {
            self.output.resize(self.output.len() + count, 32);
        } else {
            let source = heap.string(text.source)?;
            let offset = at - text.offset;
            let copied = if text.ended || offset >= source.len() {
                0
            } else {
                let piece = super::c_string(&source[offset..(offset + count).min(source.len())]);
                self.output.extend_from_slice(piece);
                piece.len()
            };
            if copied < count {
                text.ended = true;
                if self.first_nul.is_none() {
                    self.first_nul = Some(self.output.len());
                }
                self.output.resize(self.output.len() + count - copied, 0);
            }
        }
        text.position = end;
        Ok(count)
    }
    fn advance(mut self, heap: &mut Heap) -> NativeResult<NativeStep> {
        let mut budget = WORK;
        while budget > 0 {
            if let Some(mut text) = self.text.take() {
                budget -= self.text_batch(heap, &mut text, budget)?;
                if text.position != text.width {
                    self.text = Some(text);
                }
                continue;
            }
            budget -= 1;
            let unit = heap
                .string(self.source)?
                .get(self.position)
                .copied()
                .unwrap_or(0);
            let Some(mut field) = self.field.take() else {
                if unit == 0 {
                    // FixLength happens only after all fields, including those
                    // following the first generated NUL, have been processed.
                    if let Some(nul) = self.first_nul {
                        self.output.truncate(nul);
                    }
                    return Ok(NativeStep::Return(Value::Str(
                        heap.alloc_string(self.output),
                    )));
                }
                if unit == 37 {
                    self.position += 1;
                    self.field = Some(Field::new());
                } else {
                    let source = heap.string(self.source)?;
                    let end = (self.position + budget + 1).min(source.len());
                    let piece = &source[self.position..end];
                    let count = piece
                        .iter()
                        .position(|&unit| matches!(unit, 0 | 37))
                        .unwrap_or(piece.len());
                    self.reserve(count)?;
                    self.output.extend_from_slice(&piece[..count]);
                    self.position += count;
                    budget -= count - 1;
                }
                continue;
            };
            if unit == 0 {
                return Err(bad_format());
            }
            let mut consume = false;
            match field.state {
                Parse::Flag => {
                    if matches!(unit, 45 | 43 | 35) {
                        field.flag = unit;
                        consume = true;
                    }
                    field.state = Parse::Zero;
                }
                Parse::Zero => {
                    if unit == 48 {
                        consume = true;
                    }
                    field.state = Parse::WidthStart;
                }
                Parse::WidthStart => {
                    if unit == 42 {
                        field.width_indirect = true;
                        consume = true;
                        field.state = Parse::Precision;
                    } else {
                        field.state = Parse::Width;
                    }
                }
                Parse::Width => {
                    if matches!(unit, 48..=57) {
                        field.width = field
                            .width
                            .wrapping_mul(10)
                            .wrapping_add(u32::from(unit - 48));
                        consume = true;
                    } else {
                        field.state = Parse::Precision;
                    }
                }
                Parse::Precision => {
                    if unit == 46 {
                        consume = true;
                        field.state = Parse::PrecisionStart;
                    } else {
                        field.state = Parse::Kind;
                    }
                }
                Parse::PrecisionStart => {
                    if unit == 42 {
                        field.precision_indirect = true;
                        consume = true;
                        field.state = Parse::Kind;
                    } else if matches!(unit, 48..=57) {
                        field.state = Parse::PrecisionDigits;
                    } else {
                        return Err(bad_format());
                    }
                }
                Parse::PrecisionDigits => {
                    if matches!(unit, 48..=57) {
                        field.precision = field
                            .precision
                            .wrapping_mul(10)
                            .wrapping_add(u32::from(unit - 48));
                        consume = true;
                    } else {
                        field.state = Parse::Kind;
                    }
                }
                Parse::Kind => {
                    self.position += 1;
                    let before = self.output.len();
                    self.emit(heap, field, unit)?;
                    budget = budget.saturating_sub(self.output.len() - before);
                    continue;
                }
            }
            if consume {
                self.position += 1;
                field.consume(unit);
            }
            self.field = Some(field);
        }
        Ok(NativeStep::Continue(Box::new(self)))
    }
    fn emit(&mut self, heap: &mut Heap, mut field: Field, kind: u16) -> NativeResult<()> {
        if !matches!(
            kind,
            99 | 115 | 100 | 105 | 111 | 117 | 120 | 88 | 102 | 101 | 103 | 69 | 71
        ) {
            // The reference's switch has no default error: an unknown type
            // becomes an ordinary literal on the next outer-loop iteration.
            return self.push(kind);
        }
        let string = matches!(kind, 99 | 115);
        let integer = matches!(kind, 100 | 105 | 111 | 117 | 120 | 88);
        if !string
            && (field.width.wrapping_add(field.precision) > 900
                || field.length > if integer { 65 } else { 67 })
        {
            return Err(bad_format());
        }
        let width = if field.width_indirect {
            value::to_integer(heap, self.next()?)? as u32
        } else {
            field.width
        };
        let precision = if field.precision_indirect {
            value::to_integer(heap, self.next()?)? as u32
        } else {
            field.precision
        };
        if string {
            let Value::Str(source) = value::to_string(heap, self.next()?)? else {
                unreachable!()
            };
            let length = heap.string(source)?.len();
            if length == 0 {
                return Ok(());
            }
            let precision = if precision == 0 {
                length
            } else {
                precision as usize
            };
            let precision = if kind == 99 {
                precision.min(1)
            } else {
                precision
            };
            let width = (width as usize).max(precision);
            self.text = Some(Text {
                source,
                precision,
                width,
                offset: if field.flag == 45 {
                    0
                } else {
                    width - precision
                },
                position: 0,
                ended: false,
            });
            return Ok(());
        }
        let exceeds = if field.width_indirect && field.precision_indirect {
            (width as i32).wrapping_add(precision as i32) > 900
        } else {
            width.wrapping_add(precision) > 900
        };
        if exceeds {
            return Err(bad_format());
        }
        use fish_printf::Arg;
        // Fixed-size parameter storage avoids a Vec per numeric field.
        let mut params = [Arg::SInt(0, 32), Arg::SInt(0, 32), Arg::SInt(0, 32)];
        let mut count = 0;
        if field.width_indirect {
            params[count] = Arg::SInt(i64::from(width as i32), 32);
            count += 1;
        }
        if field.precision_indirect {
            params[count] = Arg::SInt(i64::from(precision as i32), 32);
            count += 1;
        }
        let value = self.next()?;
        params[count] = if matches!(kind, 111 | 117 | 120 | 88) {
            Arg::UInt(value::to_integer(heap, value)? as u64)
        } else if integer {
            Arg::SInt(value::to_integer(heap, value)?, 64)
        } else {
            Arg::Float(value::to_real(heap, value)?)
        };
        field.numeric[field.length] = kind as u8;
        let numeric = std::str::from_utf8(&field.numeric[..field.length + 1]).unwrap();
        let mut output = NumericOutput {
            bytes: [0; 1023],
            len: 0,
        };
        fish_printf::printf_c_locale(&mut output, numeric, &mut params[..count + 1])
            .map_err(|_| bad_format())?;
        self.reserve(output.len)?;
        self.output
            .extend(output.bytes[..output.len].iter().copied().map(u16::from));
        Ok(())
    }
}
fn bad_format() -> NativeError {
    NativeError::Message("invalid sprintf format")
}
/// The original numeric branch has a 1024-unit CRT buffer including NUL.
/// Exceeding it is a format error here; CRT invalid-parameter handling differs.
struct NumericOutput {
    bytes: [u8; 1023],
    len: usize,
}
impl std::fmt::Write for NumericOutput {
    fn write_str(&mut self, text: &str) -> std::fmt::Result {
        if text.len() > self.bytes.len() - self.len {
            return Err(std::fmt::Error);
        }
        self.bytes[self.len..self.len + text.len()].copy_from_slice(text.as_bytes());
        self.len += text.len();
        Ok(())
    }
}
