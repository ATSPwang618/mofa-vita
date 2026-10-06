use super::*;
#[derive(tjs_bind::Trace)]
pub(super) struct Run {
    owner: ObjId,
    text: Vec<u16>,
    position: usize,
    finish: Option<bool>,
    graph: Option<Vec<u16>>,
}
pub(super) fn start(cx: &mut NativeCx<'_>, text: Vec<u16>) -> NativeResult<NativeStep> {
    let text = tjs_core::string::c_string(&text).to_vec();
    if text.len() > 131072 {
        return Err(NativeError::Message("text render input exceeds limit"));
    }
    Box::new(Run {
        owner: cx.this(),
        text,
        position: 0,
        finish: None,
        graph: None,
    })
    .resume(cx, Value::Void)
}
pub(super) fn finish(cx: &mut NativeCx<'_>, owner: ObjId, clear: bool) -> NativeResult<NativeStep> {
    Box::new(Run {
        owner,
        text: Vec::new(),
        position: 0,
        finish: Some(clear),
        graph: None,
    })
    .resume(cx, Value::Void)
}
impl NativeContinuation for Run {
    fn resume(mut self: Box<Self>, cx: &mut NativeCx<'_>, _: Value) -> NativeResult<NativeStep> {
        for _ in 0..128 {
            let missing = binding::with_state(cx, self.owner, |s| {
                s.layout.metrics.is_none().then(|| s.layout.font.clone())
            })?;
            if let Some(font) = missing {
                let mut chars = self.text.clone();
                chars.extend([9, 32]);
                return extensions::measure_characters(cx, font, chars, self);
            }
            if let Some(clear) = self.finish {
                binding::with_state(cx, self.owner, |s| {
                    if clear {
                        s.layout.clear();
                    } else {
                        s.layout.flush(false);
                    }
                })?;
                return Ok(NativeStep::Return(Value::Void));
            }
            if self.position == self.text.len() {
                let ok = binding::with_state(cx, self.owner, |s| {
                    s.layout.style.over = s.layout.y > s.layout.height;
                    !s.layout.style.over
                })?;
                return Ok(NativeStep::Return(Value::Int(ok as i64)));
            }
            let token = token(&self.text, &mut self.position)?;
            if let Token::Graph(name) = token {
                self.graph = Some(name.clone());
                return extensions::image_size(cx, &name, self);
            }
            binding::with_state(cx, self.owner, |s| -> NativeResult<()> {
                let l = &mut s.layout;
                match token {
                    Token::Character(text) => l.push(text, None)?,
                    Token::Newline(n) => {
                        l.flush(false);
                        l.newlines(n);
                    }
                    Token::Indent => l.indent = l.x,
                    Token::Unindent => l.indent = 0,
                    Token::Face(face) => l.style.face = face,
                    Token::Flag(flag, v) => match flag {
                        98 => l.style.bold = v,
                        105 => l.style.italic = v,
                        115 => l.style.shadow = v,
                        101 => l.style.edge = v,
                        _ => unreachable!(),
                    },
                    Token::Reset => l.style = l.defaults.clone(),
                    Token::Pitch(n) => l.style.pitch = n,
                    Token::Size(n) => {
                        l.style.fontsize = l.defaults.fontsize.wrapping_mul(n) / 100;
                        l.update_font();
                    }
                    Token::Color(n) => l.style.color = n,
                    Token::Ignore => {}
                    Token::Graph(_) => unreachable!(),
                }
                Ok(())
            })??;
        }
        Ok(NativeStep::Continue(self))
    }
}
impl extensions::FontMetricsContinuation for Run {
    fn resume(
        self: Box<Self>,
        cx: &mut NativeCx<'_>,
        ascent: i32,
        widths: Vec<(u16, i32)>,
    ) -> NativeResult<NativeStep> {
        binding::with_state(cx, self.owner, |s| {
            s.layout.metrics = Some(Metrics {
                ascent,
                widths: widths.into_iter().collect(),
            })
        })?;
        NativeContinuation::resume(self, cx, Value::Void)
    }
}
impl extensions::ImageSizeContinuation for Run {
    fn resume(
        mut self: Box<Self>,
        cx: &mut NativeCx<'_>,
        size: krkr_engine::protocol::graphics::Size,
    ) -> NativeResult<NativeStep> {
        let text = self
            .graph
            .take()
            .ok_or(NativeError::Message("missing graphical character"))?;
        binding::with_state(cx, self.owner, |s| {
            s.layout
                .push(text, Some((size.width as i32, size.height as i32)))
        })??;
        NativeContinuation::resume(self, cx, Value::Void)
    }
}
enum Token {
    Character(Vec<u16>),
    Graph(Vec<u16>),
    Newline(i32),
    Indent,
    Unindent,
    Face(Vec<u16>),
    Flag(u16, bool),
    Reset,
    Pitch(i32),
    Size(i32),
    Color(i32),
    Ignore,
}
fn bad() -> NativeError {
    NativeError::Message("TextRenderBase::render() failed to parse format control")
}
fn take(text: &[u16], p: &mut usize) -> NativeResult<u16> {
    let c = *text.get(*p).ok_or_else(bad)?;
    *p += 1;
    Ok(c)
}
fn character(text: &[u16], p: &mut usize, c: u16) -> Vec<u16> {
    let mut out = vec![c];
    if (0xd800..=0xdbff).contains(&c) && text.get(*p).is_some_and(|c| (0xdc00..=0xdfff).contains(c))
    {
        out.push(text[*p]);
        *p += 1;
    }
    out
}
fn terminated(text: &[u16], p: &mut usize) -> NativeResult<Vec<u16>> {
    let start = *p;
    while let Some(&c) = text.get(*p) {
        *p += 1;
        if c == 59 {
            return Ok(text[start..*p - 1].to_vec());
        }
    }
    Err(bad())
}
fn integer(text: &[u16], p: &mut usize, mut value: i32) -> NativeResult<i32> {
    let mut negative = false;
    loop {
        match take(text, p)? {
            c @ 48..=57 => value = value.wrapping_mul(10).wrapping_add(i32::from(c - 48)),
            45 => negative = !negative,
            59 => {
                return Ok(if negative {
                    value.wrapping_neg()
                } else {
                    value
                });
            }
            _ => return Err(bad()),
        }
    }
}
fn token(text: &[u16], p: &mut usize) -> NativeResult<Token> {
    let c = take(text, p)?;
    Ok(match c {
        10 => Token::Newline(1),
        37 => {
            let code = take(text, p)?;
            match code {
                116 | 102 => Token::Face(terminated(text, p)?),
                98 | 105 | 115 | 101 => {
                    let v = take(text, p)?;
                    if ![48, 49].contains(&v) {
                        return Err(bad());
                    }
                    Token::Flag(code, v == 49)
                }
                114 => Token::Reset,
                66 | 83 | 67 | 82 | 76 => Token::Ignore, // Consumed without effects by the reference.
                108 => {
                    terminated(text, p)?;
                    Token::Ignore
                }
                110 => {
                    let n = integer(text, p, 0)?;
                    Token::Newline(if n == 0 {
                        2
                    } else {
                        n.max(0).saturating_add(1)
                    })
                }
                112 => Token::Pitch(integer(text, p, 0)?),
                100 | 119 => {
                    integer(text, p, 0)?;
                    Token::Ignore
                }
                68 => {
                    let first = take(text, p)?;
                    if first == 36 {
                        terminated(text, p)?;
                    } else {
                        integer(text, p, 0)?;
                    }
                    Token::Ignore
                }
                n @ 48..=57 => Token::Size(integer(text, p, i32::from(n - 48))?),
                _ => return Err(bad()),
            }
        }
        92 => {
            let c = take(text, p)?;
            if c > 127 {
                return Err(bad());
            }
            match c {
                110 => Token::Newline(1),
                116 => Token::Character(vec![9]),
                105 => Token::Indent,
                114 => Token::Unindent,
                119 => Token::Character(vec![32]),
                107 | 120 => Token::Ignore,
                _ => Token::Character(vec![c]),
            }
        }
        91 => {
            // The supplied SDL source consumes only the next character here.
            let c = take(text, p)?;
            character(text, p, c);
            Token::Ignore
        }
        35 => {
            let digits = terminated(text, p)?;
            let mut value = 0u32;
            for &c in &digits {
                let digit = match c {
                    48..=57 => c - 48,
                    65..=70 => c - 65 + 10,
                    97..=102 => c - 97 + 10,
                    _ => return Err(bad()),
                };
                value = value.wrapping_shl(4) | u32::from(digit);
            }
            Token::Color(if digits.is_empty() {
                0xffffff
            } else {
                value as i32
            })
        }
        38 => Token::Graph(terminated(text, p)?),
        36 => {
            terminated(text, p)?;
            Token::Ignore
        }
        _ => Token::Character(character(text, p, c)),
    })
}
