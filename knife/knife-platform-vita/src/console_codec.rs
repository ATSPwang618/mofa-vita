//! Keep each sceClibPrintf record below 1 KiB. Long JSON records are explicitly
//! fragmented, so a capture can detect a missing part rather than accept truncation.
use std::{fmt::Write, io};
pub const MAX_LINE: usize = 900;
const PAYLOAD: usize = 384;
pub fn emit_lines(line: &str, seq: u32, mut emit: impl FnMut(&str) -> io::Result<()>) -> io::Result<()> {
    if line.len() <= MAX_LINE { return emit(line); }
    if line.len() > 16 * 1024 { return Err(io::Error::other("performance record exceeds 16 KiB")); }
    let mut ranges = Vec::new();
    let mut start = 0;
    while start < line.len() {
        let mut end = (start + PAYLOAD).min(line.len());
        while !line.is_char_boundary(end) { end -= 1; }
        ranges.push(start..end); start = end;
    }
    for (part, range) in ranges.iter().enumerate() {
        let mut out = format!("{{\"kind\":\"audit_chunk\",\"seq\":{seq},\"part\":{part},\"total\":{},\"payload\":\"", ranges.len());
        for c in line[range.clone()].chars() {
            match c { '"' => out.push_str("\\\""), '\\' => out.push_str("\\\\"),
                c if c < ' ' => { let _ = write!(out, "\\u{:04x}", c as u32); }, _ => out.push(c) }
        }
        out.push_str("\"}\n");
        if out.len() > MAX_LINE { return Err(io::Error::other("performance chunk bound exceeded")); }
        emit(&out)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn unicode_and_escaped_long_records_fit_native_console_bound() {
        let line = format!("{{\"kind\":\"sample\",\"x\":\"{}\"}}\n", "图\\\"".repeat(1000));
        let mut rows = Vec::new();
        emit_lines(&line, 42, |row| { rows.push(row.to_string()); Ok(()) }).unwrap();
        assert!(rows.len() > 1);
        for (i, row) in rows.iter().enumerate() {
            assert!(row.len() <= MAX_LINE);
            assert!(row.contains(&format!("\"part\":{i},\"total\":{}", rows.len())));
            assert!(row.ends_with("\"}\n"));
        }
        let mut direct = String::new();
        emit_lines("{}\n", 43, |s| { direct.push_str(s); Ok(()) }).unwrap();
        assert_eq!(direct, "{}\n");
        assert!(emit_lines(&"x".repeat(16 * 1024 + 1), 44, |_| Ok(())).is_err());
    }
}
