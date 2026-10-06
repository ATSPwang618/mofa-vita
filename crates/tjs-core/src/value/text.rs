use super::{ArithmeticError, Value, numeric};
use crate::Heap;

/// Keep the numeric path small enough to inline into instruction dispatch.
#[inline]
pub fn add_in(heap: &mut Heap, lhs: Value, rhs: Value) -> Result<Value, ArithmeticError> {
    if let (Value::Int(lhs), Value::Int(rhs)) = (lhs, rhs) {
        return Ok(Value::Int(lhs.wrapping_add(rhs)));
    }
    concatenate(heap, lhs, rhs)
}

fn concatenate(heap: &mut Heap, lhs: Value, rhs: Value) -> Result<Value, ArithmeticError> {
    if let (Value::Str(left), Value::Str(right)) = (lhs, rhs) {
        let left_len = heap.string(left)?.len();
        let right_len = heap.string(right)?.len();
        if right_len == 0 {
            return Ok(lhs);
        }
        if left_len == 0 {
            return Ok(rhs);
        }
        // Short strings stay inline-owned. Long repeated appends amortize the
        // backing allocation while old Values retain immutable prefix views.
        if left_len >= 256 {
            return heap.append_strings(left, right).map(Value::Str);
        }
    }
    if matches!(lhs, Value::Str(_)) || matches!(rhs, Value::Str(_)) {
        let mut left_number = itoa::Buffer::new();
        let mut right_number = itoa::Buffer::new();
        let mut left = StringOperand::new(heap, lhs, &mut left_number)?;
        let mut right = StringOperand::new(heap, rhs, &mut right_number)?;
        // VM_ADD and member Operation both call Variant::operator+=. Its
        // mixed-type string path converts to two C-string pointers; the
        // string/string path instead uses the shared-string append machinery.
        if !matches!((lhs, rhs), (Value::Str(_), Value::Str(_))) {
            left = left.c_prefix();
            right = right.c_prefix();
        }
        let length = left
            .len()
            .checked_add(right.len())
            .ok_or(ArithmeticError::Allocation)?;
        let mut units = Vec::new();
        units
            .try_reserve_exact(length)
            .map_err(|_| ArithmeticError::Allocation)?;
        left.append_to(&mut units);
        right.append_to(&mut units);
        return Ok(Value::Str(heap.alloc_string(units)));
    }
    if let (Value::Octet(left), Value::Octet(right)) = (lhs, rhs) {
        let left = heap.octet(left)?;
        let right = heap.octet(right)?;
        let length = left
            .len()
            .checked_add(right.len())
            .ok_or(ArithmeticError::Allocation)?;
        let mut bytes = Vec::new();
        bytes
            .try_reserve_exact(length)
            .map_err(|_| ArithmeticError::Allocation)?;
        bytes.extend_from_slice(left);
        bytes.extend_from_slice(right);
        return Ok(Value::Octet(heap.alloc_octet(bytes)));
    }
    numeric::add(lhs, rhs)
}

enum StringOperand<'a> {
    Units(&'a [u16]),
    Decimal(&'a str),
    Real(RealText),
    Owned(String),
}

pub fn to_string(heap: &mut Heap, value: Value) -> Result<Value, ArithmeticError> {
    if matches!(value, Value::Str(_)) {
        return Ok(value);
    }
    let text = to_string_units(heap, value)?;
    Ok(Value::Str(heap.alloc_string(text)))
}

/// Convert for an owned native consumer without creating a temporary GC string.
pub fn to_string_units(heap: &Heap, value: Value) -> Result<Vec<u16>, ArithmeticError> {
    let mut number = itoa::Buffer::new();
    let text = StringOperand::new(heap, value, &mut number)?;
    let mut output = Vec::with_capacity(text.len());
    // AsString retains the complete managed string. The comparison iterator
    // deliberately stops at NUL and must not be reused for value conversion.
    text.append_to(&mut output);
    Ok(output)
}

/// Append a value's complete managed representation, including embedded NULs.
pub fn append_string_units(
    heap: &Heap,
    value: Value,
    output: &mut Vec<u16>,
) -> Result<(), ArithmeticError> {
    let mut number = itoa::Buffer::new();
    StringOperand::new(heap, value, &mut number)?.append_to(output);
    Ok(())
}

/// TJS # reads the first UTF-16 unit after string conversion; it does not
/// decode a Unicode scalar. Numeric inputs use the same text rules as string.
#[inline(never)]
pub fn character_code(heap: &Heap, value: Value) -> Result<Value, ArithmeticError> {
    let mut number = itoa::Buffer::new();
    let text = StringOperand::new(heap, value, &mut number)?;
    Ok(Value::Int(i64::from(text.units().next().unwrap_or(0))))
}

/// TJS $ truncates to one UTF-16 unit. NUL constructs an empty string.
#[inline(never)]
pub fn character_from(heap: &mut Heap, value: Value) -> Result<Value, ArithmeticError> {
    let unit = numeric::to_integer(heap, value)? as u16;
    let units = [unit];
    Ok(Value::Str(heap.alloc_string(if unit == 0 {
        &[][..]
    } else {
        &units
    })))
}

impl<'a> StringOperand<'a> {
    fn c_prefix(self) -> Self {
        match self {
            Self::Units(units) => Self::Units(crate::string::c_string(units)),
            other => other,
        }
    }
    fn new(
        heap: &'a Heap,
        value: Value,
        number: &'a mut itoa::Buffer,
    ) -> Result<Self, ArithmeticError> {
        Ok(match value {
            Value::Str(id) => Self::Units(heap.string(id)?),
            Value::Int(value) => Self::Decimal(number.format(value)),
            Value::Real(value) => Self::Real(format_real(value)),
            Value::Void => Self::Units(&[]),
            Value::Obj(reference) => Self::Owned(heap.object_text(reference)),
            _ => return Err(ArithmeticError::UnsupportedOperands),
        })
    }
    fn len(&self) -> usize {
        match self {
            Self::Units(units) => units.len(),
            Self::Decimal(text) => text.len(),
            Self::Real(text) => text.len,
            Self::Owned(text) => text.len(),
        }
    }
    fn append_to(self, output: &mut Vec<u16>) {
        match self {
            Self::Units(units) => output.extend_from_slice(units),
            Self::Decimal(text) => output.extend(text.bytes().map(u16::from)),
            Self::Real(text) => output.extend(text.bytes().iter().copied().map(u16::from)),
            Self::Owned(text) => output.extend(text.bytes().map(u16::from)),
        }
    }
}

/// Supported normal equality. Binding contexts do not participate in object
/// equality; string equality follows the reference's length then C-string test.
#[inline]
pub fn equal(heap: &Heap, lhs: Value, rhs: Value) -> Result<bool, ArithmeticError> {
    if let (Value::Int(lhs), Value::Int(rhs)) = (lhs, rhs) {
        return Ok(lhs == rhs);
    }
    equal_other(heap, lhs, rhs)
}

fn equal_other(heap: &Heap, lhs: Value, rhs: Value) -> Result<bool, ArithmeticError> {
    Ok(match (lhs, rhs) {
        (Value::Void, Value::Void) => true,
        (Value::Void, Value::Int(value)) | (Value::Int(value), Value::Void) => value == 0,
        (Value::Obj(lhs), Value::Obj(rhs)) => lhs.object == rhs.object,
        (Value::Octet(lhs), Value::Octet(rhs)) => heap.octet(lhs)? == heap.octet(rhs)?,
        (Value::Str(lhs), Value::Str(rhs)) => {
            let same = lhs == rhs;
            let lhs = heap.string(lhs)?;
            if same {
                return Ok(true); // Validate even a shared, potentially stale ID.
            }
            let rhs = heap.string(rhs)?;
            lhs.len() == rhs.len()
                && lhs
                    .iter()
                    .take_while(|&&u| u != 0)
                    .eq(rhs.iter().take_while(|&&u| u != 0))
        }
        (Value::Str(_), _) | (_, Value::Str(_)) => {
            let mut left_number = itoa::Buffer::new();
            let mut right_number = itoa::Buffer::new();
            let left = StringOperand::new(heap, lhs, &mut left_number);
            let right = StringOperand::new(heap, rhs, &mut right_number);
            match (left, right) {
                (Ok(left), Ok(right)) => left.units().eq(right.units()),
                (Err(ArithmeticError::UnsupportedOperands), _)
                | (_, Err(ArithmeticError::UnsupportedOperands)) => false,
                (Err(error), _) | (_, Err(error)) => return Err(error),
            }
        }
        (Value::Void, Value::Real(value)) | (Value::Real(value), Value::Void) => value == 0.0,
        (Value::Int(_) | Value::Real(_), Value::Int(_) | Value::Real(_)) => {
            numeric::to_real(heap, lhs)? == numeric::to_real(heap, rhs)?
        }
        _ => false,
    })
}

impl StringOperand<'_> {
    fn units(&self) -> impl Iterator<Item = u16> + '_ {
        let (units, bytes) = match self {
            Self::Units(units) => (*units, &[][..]),
            Self::Decimal(text) => (&[][..], text.as_bytes()),
            Self::Real(text) => (&[][..], text.bytes()),
            Self::Owned(text) => (&[][..], text.as_bytes()),
        };
        units
            .iter()
            .copied()
            .chain(bytes.iter().copied().map(u16::from))
            .take_while(|&u| u != 0)
    }
}

pub fn strict_equal(heap: &Heap, lhs: Value, rhs: Value) -> Result<bool, ArithmeticError> {
    if std::mem::discriminant(&lhs) != std::mem::discriminant(&rhs) {
        return Ok(false);
    }
    if let (Value::Obj(lhs), Value::Obj(rhs)) = (lhs, rhs) {
        return Ok(lhs == rhs);
    }
    equal(heap, lhs, rhs)
}

// A %.15g double needs at most a sign, 15 digits, point, e/sign and 3 exponent
// digits. Keep the conversion outside the GC heap and avoid a temporary String.
#[derive(Default)]
struct RealText {
    buffer: [u8; 32],
    len: usize,
}
impl RealText {
    fn bytes(&self) -> &[u8] {
        &self.buffer[..self.len]
    }
}
impl std::fmt::Write for RealText {
    fn write_str(&mut self, text: &str) -> std::fmt::Result {
        let destination = self.buffer[self.len..]
            .get_mut(..text.len())
            .ok_or(std::fmt::Error)?;
        destination.copy_from_slice(text.as_bytes());
        self.len += text.len();
        Ok(())
    }
}
impl From<&str> for RealText {
    fn from(text: &str) -> Self {
        use std::fmt::Write;
        let mut output = Self::default();
        output.write_str(text).expect("fixed special real spelling");
        output
    }
}

/// TJSRealToString uses 15 significant decimal digits and signed zero/Infinity.
fn format_real(value: f64) -> RealText {
    if value.is_nan() {
        return "NaN".into();
    }
    if value.is_infinite() {
        return if value.is_sign_negative() {
            "-Infinity"
        } else {
            "+Infinity"
        }
        .into();
    }
    if value == 0.0 {
        return if value.is_sign_negative() {
            "-0.0"
        } else {
            "+0.0"
        }
        .into();
    }
    let mut text = RealText::default();
    fish_printf::printf_c_locale(&mut text, "%.15g", &mut [fish_printf::Arg::Float(value)])
        .expect("fixed real format fits the 32-byte buffer");
    text
}
