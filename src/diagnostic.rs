use std::{fmt::Write, ops::Range};

use codespan_reporting::{
    diagnostic::{Diagnostic as Report, Label},
    files::SimpleFiles,
    term::{self, Config},
};
use tjs_core::{Diagnostic, SourceMap};

pub fn render(sources: &SourceMap, error: &Diagnostic) -> String {
    let mut files = SimpleFiles::new();
    let mut report = Report::error()
        .with_code(format!("{:?}", error.phase))
        .with_message(&error.message);
    if let Some((span, source)) = error
        .span
        .and_then(|span| sources.get(span.source()).map(|source| (span, source)))
    {
        let (text, range) = display_source(source.units(), span.range());
        let id = files.add(source.name(), text);
        report.labels.push(Label::primary(id, range));
        let (line, column) = source.line_column(span.start()).expect("validated span");
        report.notes.push(format!(
            "{}:{line}:{column} (UTF-16 column); range {}..{}",
            source.name(),
            span.start().get(),
            span.end().get()
        ));
    }
    if !error.trace.is_empty() {
        let mut trace = "stack (innermost first; UTF-16 columns):".to_owned();
        for frame in &error.trace {
            write!(&mut trace, "\n  at {} (pc={}", frame.function, frame.pc)
                .expect("String formatting");
            if let Some((source, line, column)) = frame.span.and_then(|span| {
                let source = sources.get(span.source())?;
                let (line, column) = source.line_column(span.start())?;
                Some((source, line, column))
            }) {
                write!(&mut trace, ", {}:{line}:{column}", source.name())
                    .expect("String formatting");
            }
            trace.push(')');
        }
        report.notes.push(trace);
    }
    term::emit_into_string(&Config::default(), &files, &report)
        .expect("display ranges belong to their diagnostic source")
}

/// Convert only when reporting an error. The compiler/VM keep their UTF-16 spans.
/// A label touching half a surrogate pair covers the whole displayed scalar.
fn display_source(units: &[u16], range: Range<usize>) -> (String, Range<usize>) {
    let mut text = String::new();
    let mut characters = char::decode_utf16(units.iter().copied()).peekable();
    let (mut position, mut start, mut end) = (0, 0, 0);
    while let Some(character) = characters.next() {
        let mut width = character.as_ref().map_or(1, |c| c.len_utf16());
        if position <= range.start && range.start < position + width {
            start = text.len();
        }
        match character {
            Ok('\r') => {
                // codespan indexes lines by LF; preserve the source's CR/CRLF lines.
                if characters.peek() == Some(&Ok('\n')) {
                    characters.next();
                    width += 1;
                    if range.start == position + 1 {
                        start = text.len();
                    }
                }
                text.push('\n');
            }
            Ok(c @ ('\n' | '\t')) => text.push(c),
            Ok(c) if c.is_control() => {
                write!(&mut text, "\\u{{{:04X}}}", c as u32).expect("String formatting");
            }
            Ok(c) => text.push(c),
            Err(e) => {
                write!(&mut text, "\\u{{{:04X}}}", e.unpaired_surrogate())
                    .expect("String formatting");
            }
        }
        if position < range.end && range.end <= position + width {
            end = text.len();
        }
        position += width;
    }
    if range.start == units.len() {
        start = text.len();
    }
    if range.is_empty() {
        end = start;
    }
    (text, start..end)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tjs_core::Phase;

    #[test]
    fn diagnostics_map_unicode_and_retain_original_utf16_coordinates() {
        let mut sources = SourceMap::new();
        let id = sources.add_utf8("sample.tjs", "/*😀中*/ 1 + ;").unwrap();
        let span = sources.span(id, 12..13).unwrap();
        let (text, range) = display_source(sources.get(id).unwrap().units(), span.range());
        assert_eq!(&text[range], ";");
        let output = render(
            &sources,
            &Diagnostic::new(Phase::Parse, span, "expected expression"),
        );
        assert!(output.contains("sample.tjs:1:13 (UTF-16 column)"));
        assert!(output.contains("/*😀中*/ 1 + ;"));
        assert!(output.contains('^'));
    }

    #[test]
    fn display_mapping_handles_surrogates_control_characters_and_line_endings() {
        let units = [0xd800, 0, 65, 13, 10, 0xd83d, 0xde00, 13, 66];
        let (text, range) = display_source(&units, 2..3);
        assert_eq!(text, "\\u{D800}\\u{0000}A\n😀\nB");
        assert_eq!(&text[range], "A");
        for range in [5..6, 6..7, 5..7] {
            let (text, range) = display_source(&units, range);
            assert_eq!(&text[range], "😀");
        }
        let (text, range) = display_source(&units, 9..9);
        assert_eq!(range, text.len()..text.len());
        let (text, range) = display_source(&units, 4..5);
        assert_eq!(&text[range], "\n");

        let mut sources = SourceMap::new();
        let id = sources.add_utf16("raw.tjs", units.to_vec()).unwrap();
        for span in [0..1, 3..9, 9..9] {
            let error = Diagnostic::new(Phase::Lex, sources.span(id, span).unwrap(), "test");
            assert!(render(&sources, &error).contains("error[Lex]"));
        }
        sources.remove(id);
        let error = Diagnostic::new(Phase::Runtime, None, "no source");
        assert!(render(&sources, &error).contains("no source"));
    }
}
