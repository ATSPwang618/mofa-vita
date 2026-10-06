//! UTF-16 storage syntax, including the original archive delimiter and ASCII
//! case folding. This deliberately does not use URL percent decoding.
use crate::{Error, Result};
use std::borrow::Cow;
const FILE: &[u16] = &[102, 105, 108, 101];
const FILE_PREFIX: &[u16] = &[102, 105, 108, 101, 58, 47, 47];

pub fn units(text: &str) -> Vec<u16> {
    text.encode_utf16().collect()
}
pub fn c_string(text: &[u16]) -> &[u16] {
    &text[..text.iter().position(|&u| u == 0).unwrap_or(text.len())]
}
pub fn fold(text: &[u16]) -> Vec<u16> {
    fold_input(text).into_owned()
}
fn fold_input(text: &[u16]) -> Cow<'_, [u16]> {
    let text = c_string(text);
    if !text.iter().any(|u| matches!(u, 65..=90 | 92)) {
        return Cow::Borrowed(text);
    }
    Cow::Owned(
        text.iter()
            .map(|&u| match u {
                65..=90 => u + 32,
                92 => 47,
                _ => u,
            })
            .collect(),
    )
}
pub fn split_name(text: &[u16]) -> (&[u16], &[u16]) {
    let text = c_string(text);
    let at = text
        .iter()
        .rposition(|u| matches!(u, 47 | 92 | 62))
        .map_or(0, |i| i + 1);
    text.split_at(at)
}
pub fn split_ext(text: &[u16]) -> (&[u16], &[u16]) {
    let text = c_string(text);
    let (path, file) = split_name(text);
    let at = file
        .iter()
        .rposition(|&u| u == 46)
        .map_or(text.len(), |i| path.len() + i);
    text.split_at(at)
}
pub fn split_archive(text: &[u16]) -> (&[u16], Option<&[u16]>) {
    match text.iter().position(|&u| u == 62) {
        Some(at) => (&text[..at], Some(&text[at + 1..])),
        None => (text, None),
    }
}
pub fn directory(text: &[u16]) -> Result<()> {
    if c_string(text)
        .last()
        .is_some_and(|u| matches!(u, 47 | 92 | 62))
    {
        Ok(())
    } else {
        Err(Error::Name("directory requires a trailing delimiter"))
    }
}
fn compress_into(path: &[u16], absolute: bool, out: &mut Vec<u16>) -> Result<()> {
    // Build directly into the result. Ordinary storage names need neither a
    // component stack nor repeated growth; parent components truncate it.
    let start = out.len();
    let root = start + usize::from(absolute);
    if absolute {
        out.push(47);
    }
    let mut trailing = path.last() == Some(&47);
    for part in path.split(|&u| u == 47).filter(|p| !p.is_empty()) {
        if part.iter().all(|&u| u == 46) {
            // TJS treats an all-dot component of length N as N-1 parents.
            for _ in 1..part.len() {
                if out.len() == root {
                    return Err(Error::Name("path crosses its root or archive boundary"));
                }
                let end = out[root..]
                    .iter()
                    .rposition(|&u| u == 47)
                    .map_or(root, |i| root + i);
                out.truncate(end.max(root));
            }
            trailing = true;
        } else {
            if out.len() > root {
                out.push(47);
            }
            out.extend_from_slice(part);
            trailing = path.last() == Some(&47);
        }
    }
    if trailing && out.len() > start && out.last() != Some(&47) {
        out.push(47);
    }
    Ok(())
}
pub fn archive(text: &[u16]) -> Result<Vec<u16>> {
    let input = fold_input(text);
    let mut out = Vec::with_capacity(input.len());
    compress_into(&input, false, &mut out)?;
    Ok(out)
}

/// Normalize non-file media using Kirikiri's domain/path syntax, without URL
/// decoding. The domain is a boundary: dot components cannot escape it.
pub(crate) fn medium_path(text: &[u16], current: &[u16]) -> Result<Option<Vec<u16>>> {
    let text = c_string(text);
    let mut input = if text.contains(&92) {
        Cow::Owned(
            text.iter()
                .map(|&u| if u == 92 { 47 } else { u })
                .collect::<Vec<_>>(),
        )
    } else {
        Cow::Borrowed(text)
    };
    if input.is_empty() {
        return Ok(None);
    }
    #[cfg(target_os = "vita")]
    if crate::local::vita_storage(&input).is_some() {
        return Ok(None);
    }
    let marker = [58, 47, 47];
    let explicit = input.windows(3).position(|w| w == marker);
    if let Some(at) = explicit
        && input[..at].iter().any(|u| (65..=90).contains(u))
    {
        for unit in &mut input.to_mut()[..at] {
            if (65..=90).contains(unit) {
                *unit += 32;
            }
        }
    }
    if explicit == Some(4) && input.starts_with(FILE) {
        return Ok(None);
    }
    if input.get(1) == Some(&58) {
        return Ok(None);
    }
    let current_at = current.windows(3).position(|w| w == marker);
    let full = if explicit.is_some() {
        input
    } else if current_at.is_some() && !current.starts_with(FILE_PREFIX) {
        let domain_start = current_at.unwrap() + 3;
        let root = current[domain_start..]
            .iter()
            .position(|&u| u == 47)
            .map(|i| domain_start + i)
            .ok_or(Error::Name("missing medium domain"))?;
        let prefix = if input.first() == Some(&47) {
            &current[..root]
        } else {
            current
        };
        Cow::Owned([prefix, &input].concat())
    } else {
        return Ok(None);
    };
    let at = full
        .windows(3)
        .position(|w| w == marker)
        .ok_or(Error::Name("invalid medium"))?;
    if at == 0 {
        return Err(Error::Name("empty medium name"));
    }
    let start = at + 3;
    let end = full[start..]
        .iter()
        .position(|&u| u == 47)
        .map_or(full.len(), |i| start + i);
    if end == start {
        return Err(Error::Name("empty medium domain"));
    }
    let mut out = Vec::with_capacity(full.len() + 1);
    out.extend_from_slice(&full[..end]);
    compress_into(&full[end..], true, &mut out)?;
    Ok(Some(out))
}

/// `current` is an absolute normalized directory, including its final delimiter.
pub fn normalize(text: &[u16], current: &[u16]) -> Result<Vec<u16>> {
    let mut input = fold_input(text);
    #[cfg(target_os = "vita")]
    if let Some(mounted) = crate::local::vita_storage(&input) {
        input = Cow::Owned(mounted);
    }
    if input.is_empty() {
        return Ok(Vec::new());
    }
    if input.len() >= 2 && (97..=122).contains(&input[0]) && input[1] == 58 {
        let mut absolute = Vec::with_capacity(input.len() + FILE_PREFIX.len() + 1);
        absolute.extend_from_slice(FILE_PREFIX);
        absolute.extend([46, 47, input[0]]);
        absolute.extend_from_slice(&input[2..]);
        input = Cow::Owned(absolute);
    }
    let (outer, inner) = split_archive(&input);
    if outer.is_empty() {
        return Err(Error::Name("missing archive name"));
    }
    let current_outer = split_archive(current).0;
    let prefix = FILE_PREFIX;
    let current_body = current_outer
        .strip_prefix(prefix)
        .ok_or(Error::Name("invalid current directory"))?;
    let split = current_body
        .iter()
        .position(|&u| u == 47)
        .ok_or(Error::Name("missing current domain"))?;
    let current_domain = &current_body[..split];
    let (mut domain, mut path) = (current_domain, outer);
    if let Some(colon) = outer.iter().position(|&u| u == 58) {
        if &outer[..colon] != FILE {
            return Err(Error::Name("unregistered storage medium"));
        }
        path = &outer[colon + 1..];
    }
    if path.starts_with(&[47, 47, 47]) {
        path = &path[2..];
    } else if path.starts_with(&[47, 47]) {
        path = &path[2..];
        let at = path
            .iter()
            .position(|&u| u == 47)
            .ok_or(Error::Name("missing domain separator"))?;
        domain = &path[..at];
        path = &path[at..];
    }
    let joined;
    if path.is_empty() {
        path = &[47];
    } else if path[0] != 47 {
        // A current directory may itself be inside an archive.
        let current_path = &current[prefix.len() + split..];
        joined = [current_path, path].concat();
        path = &joined;
    }
    let (path, inherited_inner) = split_archive(path);
    if inner.is_some() && inherited_inner.is_some() {
        return Err(Error::Name("nested archive path"));
    }
    let mut out = Vec::with_capacity(
        prefix.len()
            + domain.len()
            + path.len()
            + inner.or(inherited_inner).map_or(0, |s| s.len() + 1)
            + 1,
    );
    out.extend_from_slice(prefix);
    out.extend_from_slice(domain);
    compress_into(path, true, &mut out)?;
    if let Some(inner) = inner.or(inherited_inner) {
        out.push(62);
        compress_into(&fold_input(inner), false, &mut out)?;
    }
    Ok(out)
}
