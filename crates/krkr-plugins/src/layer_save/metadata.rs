//! Read PNG metadata through the same getter sequence as ncbPropAccessor.
//! TLG tags enumerate raw non-hidden values without evaluating properties.
use super::*;
#[derive(tjs_bind::Trace)]
struct Metadata {
    encode: Option<Encoding>,
    object: Value,
    sentinel: Value,
    #[trace(skip = "Readback owns budgeted bytes without TJS values")]
    pixels: Arc<krkr_engine::protocol::pixels::Pixels>,
    #[trace(skip = "Converted encoding options contain no TJS values")]
    options: Options,
    #[trace(skip = "Allocation budget has no script values")]
    budget: krkr_engine::protocol::budget::Budget,
    members: Vec<(Vec<u16>, Value)>,
    cursor: usize,
    bytes: usize,
    limit: usize,
    group: usize,
    phase: usize,
    xy: [i32; 2],
    unit: bool,
}
pub(super) fn begin(
    cx: &mut NativeCx<'_>,
    encode: Encoding,
    pixels: Arc<krkr_engine::protocol::pixels::Pixels>,
) -> NativeResult<NativeStep> {
    let tags = encode.tags;
    let mode = encode.mode;
    let object = if matches!(mode, Mode::Octet) {
        match tags {
            Some(Value::Obj(r)) => r.object.map(|_| Value::Obj(r)),
            _ => None,
        }
    } else {
        nullable(tags.unwrap_or(Value::Void))?
    };
    let mut options = Options::default();
    if matches!(mode, Mode::Octet) && object.is_none() {
        if let Some(v) = tags.filter(|v| !matches!(v, Value::Obj(_))) {
            let n = value::to_integer(cx.heap(), v)? as i32;
            if n >= 0 {
                options.compression = Some(n);
            }
        }
        return encode.encode(cx, pixels, options);
    }
    let Some(object) = object else {
        return encode.encode(cx, pixels, options);
    };
    let mut members = Vec::new();
    let budget = extensions::layer_pixel_budget(cx, encode.owner)?;
    let mut bytes = 0usize;
    let limit = krkr_engine::storages::service(cx)?
        .borrow()
        .limits()
        .max_index_bytes;
    if mode.tlg() {
        let id = crate::exports::object(object)?;
        for (name, value) in cx.heap().members(id)? {
            let name = cx.heap().symbol(name)?;
            let count = name.len().saturating_mul(2).saturating_add(64);
            bytes = bytes.saturating_add(count);
            if bytes > limit {
                return Err(NativeError::Message("image tag index exceeds limit"));
            }
            options.reserve_metadata(&budget, count).map_err(detail)?;
            members.push((name.to_vec(), value));
        }
    }
    let sentinel = Value::Obj(cx.heap_mut().alloc_dictionary().into());
    let task = Box::new(Metadata {
        encode: Some(encode),
        object,
        sentinel,
        pixels,
        options,
        budget,
        members,
        cursor: 0,
        bytes,
        limit,
        group: 0,
        phase: 0,
        xy: [0; 2],
        unit: false,
    });
    if mode.tlg() {
        Ok(NativeStep::Continue(task))
    } else {
        task.next(cx)
    }
}
impl Metadata {
    fn absent(&self, v: Value) -> bool {
        matches!((v,self.sentinel),(Value::Obj(a),Value::Obj(b)) if a==b)
    }
    fn mode(&self) -> Mode {
        self.encode.as_ref().unwrap().mode
    }
    fn finish(mut self: Box<Self>, cx: &mut NativeCx<'_>) -> NativeResult<NativeStep> {
        self.encode
            .take()
            .unwrap()
            .encode(cx, self.pixels, self.options)
    }
    fn chunk(&mut self) {
        let mut data = Vec::with_capacity(9);
        for n in self.xy {
            data.extend_from_slice(&n.to_be_bytes());
        }
        data.push(u8::from(self.unit));
        self.options
            .chunks
            .push(([*b"pHYs", *b"oFFs", *b"vpAg"][self.group], data));
        self.group += 1;
        self.phase = 0;
        self.xy = [0; 2];
        self.unit = false;
    }
    fn next(mut self: Box<Self>, cx: &mut NativeCx<'_>) -> NativeResult<NativeStep> {
        if self.group == 3 {
            if matches!(self.mode(), Mode::Octet) {
                return self.finish(cx);
            }
            if matches!(self.mode(), Mode::BackgroundPng) && self.phase == 0 {
                self.phase = 1;
            }
            if self.phase >= 3 {
                return self.finish(cx);
            }
        }
        let name = if self.group == 3 {
            "comp_lv"
        } else {
            let names = [
                ["reso_x", "reso_y", "reso_unit"],
                ["offs_x", "offs_y", "offs_unit"],
                ["vpag_w", "vpag_h", "vpag_unit"],
            ][self.group];
            names[match self.phase {
                0 | 2 | 3 => 0,
                1 | 4 | 5 => 1,
                _ => 2,
            }]
        };
        let probe = if self.group == 3 {
            self.phase < 2
        } else {
            matches!(self.phase, 0 | 1 | 2 | 4 | 6)
        };
        let key = key(cx, name);
        Ok(if probe {
            NativeStep::GetRequiredOr {
                object: self.object,
                key,
                fallback: self.sentinel,
                continuation: self,
            }
        } else {
            NativeStep::GetOptional {
                object: self.object,
                key,
                continuation: self,
            }
        })
    }
}
impl NativeContinuation for Metadata {
    fn resume(mut self: Box<Self>, cx: &mut NativeCx<'_>, v: Value) -> NativeResult<NativeStep> {
        if self.mode().tlg() {
            if let Some((name, v)) = self.members.get(self.cursor) {
                let Value::Str(id) = value::to_string(cx.heap_mut(), *v)? else {
                    unreachable!()
                };
                let val = tjs_core::string::c_string(cx.heap().string(id)?);
                let count = name
                    .len()
                    .saturating_add(val.len())
                    .saturating_mul(6)
                    .saturating_add(128);
                self.bytes = self.bytes.saturating_add(count);
                if self.bytes > self.limit {
                    return Err(NativeError::Message("image tags exceed index limit"));
                }
                self.options
                    .reserve_metadata(&self.budget, count)
                    .map_err(detail)?;
                self.options.tags.push((
                    String::from_utf16(tjs_core::string::c_string(name)).map_err(detail)?,
                    String::from_utf16(val).map_err(detail)?,
                ));
                self.cursor += 1;
                return Ok(NativeStep::Continue(self));
            }
            return self.finish(cx);
        }
        let present = !self.absent(v);
        if self.group == 3 {
            match self.phase {
                0 => {
                    if !present {
                        return self.finish(cx);
                    }
                    self.phase = 1;
                }
                1 => {
                    if !present {
                        self.options.compression = Some(-1);
                        return self.finish(cx);
                    }
                    self.phase = 2;
                }
                _ => {
                    self.options.compression = Some(value::to_integer(cx.heap(), v)? as i32);
                    return self.finish(cx);
                }
            }
        } else {
            match self.phase {
                0 => self.phase = if present { 2 } else { 1 },
                1 => {
                    if present {
                        self.phase = 2;
                    } else {
                        self.group += 1;
                        self.phase = 0;
                    }
                }
                2 => self.phase = if present { 3 } else { 4 },
                3 => {
                    self.xy[0] = value::to_integer(cx.heap(), v)? as i32;
                    self.phase = 4;
                }
                4 => self.phase = if present { 5 } else { 6 },
                5 => {
                    self.xy[1] = value::to_integer(cx.heap(), v)? as i32;
                    self.phase = 6;
                }
                6 => {
                    if present {
                        self.phase = 7;
                    } else {
                        self.chunk();
                    }
                }
                _ => {
                    let unit = text(cx, v)?;
                    self.unit = unit
                        == if self.group == 0 {
                            "meter"
                        } else {
                            "micrometer"
                        }
                        .encode_utf16()
                        .collect::<Vec<_>>();
                    self.chunk();
                }
            }
        }
        self.next(cx)
    }
}
