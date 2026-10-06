use crate::{Error, Result, name};

/// Use Kirikiri ASCII case folding, but reject ambiguous/nonportable names
/// before normalization can remove NULs, roots or parent components.
pub(super) fn portable_name(raw: &[u16]) -> Result<Vec<u16>> {
    let text = String::from_utf16(raw).map_err(|_| Error::Name("invalid UTF-16 XP3 name"))?;
    for part in text.split(['/', '\\']) {
        if part.is_empty()
            || part.chars().all(|c| c == '.')
            || part.ends_with(['.', ' '])
            || part
                .chars()
                .any(|c| c.is_control() || "<>:\"|?*".contains(c))
        {
            return Err(Error::Name("XP3 name is not a portable relative file path"));
        }
        let stem = part
            .split('.')
            .next()
            .unwrap_or_default()
            .to_ascii_uppercase();
        if matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
            || ["COM", "LPT"].iter().any(|prefix| {
                stem.strip_prefix(prefix).is_some_and(|n| {
                    matches!(
                        n,
                        "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "¹" | "²" | "³"
                    )
                })
            })
        {
            return Err(Error::Name("XP3 name uses a reserved device name"));
        }
    }
    name::archive(raw)
}
