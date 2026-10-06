//! XML token processing without namespaces, matching XML_ParserCreate (not CreateNS).
use quick_xml::{Reader, events::Event};
use std::{
    collections::{BTreeMap, VecDeque},
    io::Cursor,
    sync::Arc,
};
pub const LIMIT: usize = 16 * 1024 * 1024;
#[derive(Clone, Default)]
pub struct Position {
    pub index: usize,
    pub count: usize,
    pub line: usize,
    pub column: usize,
}
#[derive(Clone)]
pub enum Data {
    Text(String),
    Attributes(String, Vec<(String, String)>),
    Pair(String, String),
    None,
}
#[derive(Clone)]
pub struct Message {
    pub handler: usize,
    pub data: Data,
    pub raw: String,
    pub position: Position,
}
pub struct Failure {
    pub code: i64,
    pub position: Position,
}
struct Frame {
    reader: Reader<Cursor<Arc<[u8]>>>,
    at: Option<Position>,
    name: Option<String>,
}
pub struct Parser {
    source: Arc<[u8]>,
    cursor: std::cell::Cell<(usize, usize, usize, bool)>,
    frames: Vec<Frame>,
    buffer: Vec<u8>,
    pending: VecDeque<Message>,
    entities: BTreeMap<String, Option<String>>,
    defaults: BTreeMap<String, Vec<(String, bool, Option<String>)>>,
    tags: Vec<String>,
    root: bool,
    doctype: bool,
    expanded: usize,
}
fn make_frame(bytes: Arc<[u8]>, at: Option<Position>, name: Option<String>) -> Frame {
    let mut reader = Reader::from_reader(Cursor::new(bytes));
    reader.config_mut().check_comments = true;
    // One shared tag stack also covers entity replacement fragments.
    reader.config_mut().check_end_names = false;
    reader.config_mut().allow_unmatched_ends = true;
    Frame { reader, at, name }
}
fn valid_char(c: char) -> bool {
    matches!(c, '\t' | '\n' | '\r')
        || ('\u{20}'..='\u{d7ff}').contains(&c)
        || ('\u{e000}'..='\u{fffd}').contains(&c)
        || c >= '\u{10000}'
}
fn name(s: &str) -> bool {
    let first = |c: char| matches!(c, ':'|'_'|'A'..='Z'|'a'..='z'|'\u{c0}'..='\u{d6}'|'\u{d8}'..='\u{f6}'|'\u{f8}'..='\u{2ff}'|'\u{370}'..='\u{37d}'|'\u{37f}'..='\u{1fff}'|'\u{200c}'..='\u{200d}'|'\u{2070}'..='\u{218f}'|'\u{2c00}'..='\u{2fef}'|'\u{3001}'..='\u{d7ff}'|'\u{f900}'..='\u{fdcf}'|'\u{fdf0}'..='\u{fffd}'|'\u{10000}'..='\u{effff}');
    let mut chars = s.chars();
    chars.next().is_some_and(first) && chars.all(|c| {
        first(c)
            || matches!(c, '0'..='9'|'.'|'-'|'\u{b7}'|'\u{300}'..='\u{36f}'|'\u{203f}'..='\u{2040}')
    })
}
fn dtd_token<'a>(rest: &mut &'a str) -> Result<(&'a str, bool), i64> {
    *rest = rest.trim_start();
    let first = rest.chars().next().ok_or(2)?;
    if matches!(first, '\'' | '"') {
        *rest = &rest[1..];
        let end = rest.find(first).ok_or(2)?;
        let value = &rest[..end];
        *rest = &rest[end + 1..];
        Ok((value, true))
    } else if first == '(' {
        let end = rest.find(')').ok_or(2)?;
        let value = &rest[..end + 1];
        *rest = &rest[end + 1..];
        Ok((value, false))
    } else {
        let end = rest
            .find(|c: char| c.is_whitespace() || c == '>')
            .unwrap_or(rest.len());
        if end == 0 {
            return Err(2);
        }
        let value = &rest[..end];
        *rest = &rest[end..];
        Ok((value, false))
    }
}
fn text(bytes: &[u8]) -> Result<String, i64> {
    let s = std::str::from_utf8(bytes).map_err(|_| 4)?;
    if !s.chars().all(valid_char) {
        return Err(4);
    }
    Ok(s.replace("\r\n", "\n").replace('\r', "\n"))
}
impl Parser {
    pub fn new(bytes: Vec<u8>) -> Self {
        let source: Arc<[u8]> = bytes.into();
        Self {
            frames: vec![make_frame(source.clone(), None, None)],
            source,
            cursor: std::cell::Cell::new((0, 1, 0, false)),
            buffer: Vec::new(),
            pending: VecDeque::new(),
            entities: BTreeMap::new(),
            defaults: BTreeMap::new(),
            tags: Vec::new(),
            root: false,
            doctype: false,
            expanded: 0,
        }
    }
    pub fn position(&self, index: usize, count: usize) -> Position {
        let index = index.min(self.source.len());
        let (mut offset, mut line, mut column, mut cr) = self.cursor.get();
        if offset > index {
            (offset, line, column, cr) = (0, 1, 0, false);
        }
        for c in String::from_utf8_lossy(&self.source[offset..index]).chars() {
            if c == '\r' {
                line += 1;
                column = 0;
                cr = true;
            } else if c == '\n' {
                if !cr {
                    line += 1;
                }
                column = 0;
                cr = false;
            } else {
                column += 1;
                cr = false;
            }
        }
        self.cursor.set((index, line, column, cr));
        Position {
            index,
            count,
            line,
            column,
        }
    }
    pub fn end(&self) -> Position {
        self.position(self.source.len(), 0)
    }
    fn resolve(&mut self, entity: &str, depth: usize) -> Result<String, i64> {
        let s = match entity {
            "amp" => "&".to_owned(),
            "lt" => "<".to_owned(),
            "gt" => ">".to_owned(),
            "apos" => "'".to_owned(),
            "quot" => "\"".to_owned(),
            _ if entity.starts_with('#') => {
                let n = if let Some(s) = entity.strip_prefix("#x") {
                    u32::from_str_radix(s, 16)
                } else {
                    entity[1..].parse()
                }
                .map_err(|_| 14)?;
                let c = char::from_u32(n).filter(|&c| valid_char(c)).ok_or(14)?;
                c.to_string()
            }
            _ => {
                if depth >= 16 {
                    return Err(12);
                }
                let Some(value) = self.entities.get(entity).cloned() else {
                    return Err(11);
                };
                let Some(value) = value else { return Err(16) };
                self.unescape(&value, depth + 1)?
            }
        };
        self.expanded = self.expanded.saturating_add(s.len());
        if self.expanded > LIMIT {
            return Err(43);
        }
        Ok(s)
    }
    fn unescape(&mut self, s: &str, depth: usize) -> Result<String, i64> {
        let mut out = String::new();
        let mut rest = s;
        while let Some(i) = rest.find('&') {
            out.push_str(&rest[..i]);
            rest = &rest[i + 1..];
            let end = rest.find(';').ok_or(4)?;
            out.push_str(&self.resolve(&rest[..end], depth)?);
            rest = &rest[end + 1..];
            if out.len() > LIMIT {
                return Err(43);
            }
        }
        out.push_str(rest);
        Ok(out)
    }
    fn declarations(&mut self, s: &str) -> Result<(), i64> {
        let mut head = s;
        let (root, quoted) = dtd_token(&mut head)?;
        if quoted || !name(root) {
            return Err(2);
        }
        let Some(open) = head.find('[') else {
            return Ok(());
        };
        let end = head.rfind(']').ok_or(2)?;
        if end < open {
            return Err(2);
        }
        let mut rest = &head[open + 1..end];
        while !rest.trim().is_empty() {
            rest = rest.trim_start();
            if let Some(comment) = rest.strip_prefix("<!--") {
                let end = comment.find("-->").ok_or(2)?;
                rest = &comment[end + 3..];
                continue;
            }
            if let Some(pi) = rest.strip_prefix("<?") {
                let end = pi.find("?>").ok_or(2)?;
                rest = &pi[end + 2..];
                continue;
            }
            if let Some(parameter) = rest.strip_prefix('%') {
                let end = parameter.find(';').ok_or(10)?;
                rest = &parameter[end + 1..];
                continue;
            }
            rest = rest.strip_prefix("<!").ok_or(2)?;
            let (kind, _) = dtd_token(&mut rest)?;
            match kind {
                "ENTITY" => {
                    let (mut key, mut quoted) = dtd_token(&mut rest)?;
                    let parameter = key == "%";
                    if parameter {
                        (key, quoted) = dtd_token(&mut rest)?;
                    }
                    if quoted || !name(key) {
                        return Err(4);
                    }
                    let (value, literal) = dtd_token(&mut rest)?;
                    let value = if literal {
                        Some(text(value.as_bytes())?)
                    } else if matches!(value, "SYSTEM" | "PUBLIC") {
                        let (_, q) = dtd_token(&mut rest)?;
                        if !q {
                            return Err(2);
                        }
                        if value == "PUBLIC" {
                            let (_, q) = dtd_token(&mut rest)?;
                            if !q {
                                return Err(2);
                            }
                        }
                        None
                    } else {
                        return Err(2);
                    };
                    if !parameter {
                        if self.entities.len() >= 4096 {
                            return Err(43);
                        }
                        self.entities.entry(key.to_owned()).or_insert(value);
                    }
                }
                "ATTLIST" => {
                    let (element, q) = dtd_token(&mut rest)?;
                    if q || !name(element) {
                        return Err(4);
                    }
                    loop {
                        rest = rest.trim_start();
                        if rest.starts_with('>') {
                            break;
                        }
                        let (key, q) = dtd_token(&mut rest)?;
                        if q || !name(key) {
                            return Err(4);
                        }
                        let (kind, q) = dtd_token(&mut rest)?;
                        if q {
                            return Err(2);
                        }
                        if kind == "NOTATION" {
                            dtd_token(&mut rest)?;
                        }
                        let (mut value, mut literal) = dtd_token(&mut rest)?;
                        if value == "#FIXED" && !literal {
                            (value, literal) = dtd_token(&mut rest)?;
                        }
                        let default = if literal {
                            Some(text(value.as_bytes())?)
                        } else if matches!(value, "#REQUIRED" | "#IMPLIED") {
                            None
                        } else {
                            return Err(2);
                        };
                        if self.defaults.len() >= 4096 {
                            return Err(43);
                        }
                        let list = self.defaults.entry(element.to_owned()).or_default();
                        if list.len() >= 4096 {
                            return Err(43);
                        }
                        if !list.iter().any(|(k, _, _)| k == key) {
                            list.push((key.to_owned(), kind != "CDATA", default));
                        }
                    }
                }
                "ELEMENT" | "NOTATION" => {
                    let (key, q) = dtd_token(&mut rest)?;
                    if q || !name(key) {
                        return Err(4);
                    }
                }
                _ => return Err(2),
            }
            // Consume the declaration's remaining model/external identifier, respecting quotes.
            rest = rest.trim_start();
            while !rest.starts_with('>') {
                dtd_token(&mut rest)?;
                rest = rest.trim_start();
            }
            rest = &rest[1..];
        }
        Ok(())
    }
    pub fn next(&mut self, expand: bool) -> Result<Option<Message>, Failure> {
        if let Some(message) = self.pending.pop_front() {
            return Ok(Some(message));
        }
        loop {
            self.buffer.clear();
            let frame = self.frames.last_mut().expect("document frame");
            let start = frame.reader.buffer_position() as usize;
            let result = frame
                .reader
                .read_event_into(&mut self.buffer)
                .map(|e| e.into_owned());
            let end = frame.reader.buffer_position() as usize;
            let origin = frame.at.clone();
            let raw =
                String::from_utf8_lossy(&frame.reader.get_ref().get_ref()[start..end]).into_owned();
            let at = origin.unwrap_or_else(|| self.position(start, end - start));
            let event = result.map_err(|e| Failure {
                code: match e {
                    quick_xml::Error::Syntax(quick_xml::errors::SyntaxError::UnclosedCData) => 20,
                    quick_xml::Error::Syntax(_) => 5,
                    _ => 4,
                },
                position: at.clone(),
            })?;
            let converted = (|| -> Result<Option<Message>, i64> {
                let (handler, data) = match event {
                    Event::Eof => {
                        if self.frames.len() > 1 {
                            self.frames.pop();
                            return Ok(None);
                        }
                        if !self.root || !self.tags.is_empty() {
                            return Err(3);
                        }
                        return Ok(None);
                    }
                    Event::Start(ref e) | Event::Empty(ref e) => {
                        let tag = text(e.name().as_ref())?;
                        if !name(&tag) {
                            return Err(4);
                        }
                        if self.tags.is_empty() {
                            if self.root {
                                return Err(9);
                            }
                            self.root = true;
                        }
                        if self.tags.len() >= 256 {
                            return Err(43);
                        }
                        let mut attrs = Vec::new();
                        for a in e.attributes() {
                            let a = a.map_err(|e| {
                                if matches!(
                                    e,
                                    quick_xml::events::attributes::AttrError::Duplicated(..)
                                ) {
                                    8
                                } else {
                                    4
                                }
                            })?;
                            if attrs.len() >= 4096 {
                                return Err(43);
                            }
                            let key = text(a.key.as_ref())?;
                            if !name(&key) {
                                return Err(4);
                            }
                            let raw = text(&a.value)?;
                            if raw.contains('<') {
                                return Err(4);
                            }
                            let normalized = raw.replace(['\t', '\n'], " ");
                            attrs.push((key, self.unescape(&normalized, 0)?));
                        }
                        if let Some(defaults) = self.defaults.get(&tag).cloned() {
                            for (key, collapse, default) in defaults {
                                if !attrs.iter().any(|(k, _)| k == &key)
                                    && let Some(default) = default
                                {
                                    let value =
                                        self.unescape(&default.replace(['\t', '\n'], " "), 0)?;
                                    attrs.push((key.clone(), value));
                                }
                                if collapse
                                    && let Some((_, v)) = attrs.iter_mut().find(|(k, _)| k == &key)
                                {
                                    *v = v.split_whitespace().collect::<Vec<_>>().join(" ");
                                }
                            }
                        }
                        if matches!(event, Event::Empty(_)) {
                            let position = self.position(at.index + at.count, 0);
                            self.pending.push_back(Message {
                                handler: 1,
                                data: Data::Text(tag.clone()),
                                raw: String::new(),
                                position,
                            });
                        } else {
                            self.tags.push(tag.clone());
                        }
                        (0, Data::Attributes(tag, attrs))
                    }
                    Event::End(e) => {
                        let tag = text(e.name().as_ref())?;
                        if self.tags.pop().as_deref() != Some(&tag) {
                            return Err(7);
                        }
                        (1, Data::Text(tag))
                    }
                    Event::Text(e) => {
                        let value = text(&e)?;
                        if value.contains("]]>") {
                            return Err(4);
                        }
                        if self.tags.is_empty() {
                            if !value.chars().all(char::is_whitespace) {
                                return Err(if self.root { 9 } else { 2 });
                            }
                            (7, Data::Text(raw.clone()))
                        } else {
                            (2, Data::Text(value))
                        }
                    }
                    Event::GeneralRef(e) => {
                        if self.tags.is_empty() {
                            return Err(2);
                        }
                        let key = text(&e)?;
                        if let Some(value) = self.entities.get(&key).cloned() {
                            if !expand {
                                (7, Data::Text(raw.clone()))
                            } else if let Some(value) = value {
                                if self.frames.len() >= 16
                                    || self.frames.iter().any(|f| f.name.as_ref() == Some(&key))
                                {
                                    return Err(12);
                                }
                                self.expanded = self.expanded.saturating_add(value.len());
                                if self.expanded > LIMIT {
                                    return Err(43);
                                }
                                self.frames.push(make_frame(
                                    Arc::from(value.into_bytes()),
                                    Some(at.clone()),
                                    Some(key),
                                ));
                                return Ok(None);
                            } else {
                                return Ok(None);
                            }
                        } else {
                            (2, Data::Text(self.resolve(&key, 0)?))
                        }
                    }
                    Event::CData(e) => {
                        if self.tags.is_empty() {
                            return Err(2);
                        }
                        let value = text(&e)?;
                        let mut body = self.position(at.index + 9, raw.len().saturating_sub(12));
                        self.pending.push_back(Message {
                            handler: 2,
                            data: Data::Text(value.clone()),
                            raw: value,
                            position: body.clone(),
                        });
                        body = self.position(body.index + body.count, 3);
                        self.pending.push_back(Message {
                            handler: 6,
                            data: Data::None,
                            raw: "]]>".to_owned(),
                            position: body,
                        });
                        (5, Data::None)
                    }
                    Event::Comment(e) => (4, Data::Text(text(&e)?)),
                    Event::PI(e) => {
                        let target = text(e.target())?;
                        if !name(&target) || target.eq_ignore_ascii_case("xml") {
                            return Err(17);
                        }
                        (
                            3,
                            Data::Pair(target, text(e.content())?.trim_start().to_owned()),
                        )
                    }
                    Event::Decl(e) => {
                        if start != 0 && !(start == 3 && self.source.starts_with(&[239, 187, 191]))
                        {
                            return Err(17);
                        }
                        if e.version().map_err(|_| 30)?.as_ref() != b"1.0" {
                            return Err(30);
                        }
                        if let Some(enc) = e.encoding() {
                            let enc = enc.map_err(|_| 30)?;
                            if !enc.eq_ignore_ascii_case(b"UTF-8")
                                && !enc.eq_ignore_ascii_case(b"US-ASCII")
                            {
                                return Err(19);
                            }
                        }
                        (7, Data::Text(raw.clone()))
                    }
                    Event::DocType(e) => {
                        if self.root || self.doctype {
                            return Err(2);
                        }
                        self.doctype = true;
                        self.declarations(&text(&e)?)?;
                        (7, Data::Text(raw.clone()))
                    }
                };
                let raw = if handler == 5 {
                    "<![CDATA[".to_owned()
                } else {
                    raw
                };
                Ok(Some(Message {
                    handler,
                    data,
                    raw,
                    position: if handler == 5 {
                        Position {
                            count: 9,
                            ..at.clone()
                        }
                    } else {
                        at.clone()
                    },
                }))
            })()
            .map_err(|code| Failure { code, position: at })?;
            if converted.is_some() {
                return Ok(converted);
            }
            if self.frames.len() == 1
                && self.frames[0].reader.buffer_position() as usize >= self.source.len()
            {
                if !self.root || !self.tags.is_empty() {
                    return Err(Failure {
                        code: 3,
                        position: self.end(),
                    });
                }
                return Ok(None);
            }
        }
    }
}
pub fn error_string(code: i64) -> &'static str {
    match code {
        0 => "",
        2 => "syntax error",
        3 => "no element found",
        4 => "not well-formed (invalid token)",
        5 => "unclosed token",
        7 => "mismatched tag",
        8 => "duplicate attribute",
        9 => "junk after document element",
        10 => "illegal parameter entity reference",
        11 => "undefined entity",
        12 => "recursive entity reference",
        14 => "reference to invalid character number",
        16 => "reference to external entity in attribute",
        17 => "XML or text declaration not at start of entity",
        19 => "encoding specified in XML declaration is incorrect",
        20 => "unclosed CDATA section",
        30 => "XML declaration not well-formed",
        43 => "limit on input amplification factor (from DTD and entities) breached",
        _ => "XML parse error",
    }
}
