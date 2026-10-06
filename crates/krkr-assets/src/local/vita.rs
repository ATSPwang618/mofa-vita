//! Map Vita device mounts to storage domains so `..` cannot escape a mount.
use crate::{Error, Result};

fn valid_mount(mount: &str) -> bool {
    !mount.is_empty()
        && mount
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'_')
}
pub(super) fn storage(path: &str) -> Option<String> {
    let (mount, tail) = path.split_once(':')?;
    if tail.starts_with("//") {
        return None;
    }
    valid_mount(mount).then(|| format!("file://{mount}/{}", tail.trim_start_matches('/')))
}
pub(super) fn native(domain: &[u16], tail: &[u16]) -> Result<Vec<u16>> {
    if domain.is_empty()
        || !domain
            .iter()
            .all(|&u| matches!(u, 48..=57 | 65..=90 | 97..=122 | 95))
    {
        return Err(Error::Name("invalid Vita mount"));
    }
    let mut path = Vec::with_capacity(domain.len() + 1 + tail.len());
    path.extend_from_slice(domain);
    path.push(58);
    path.extend_from_slice(tail);
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::name;
    #[test]
    fn device_roots_round_trip_and_bound_parent_paths() {
        for raw in ["ux0:data/Game", "ux0:/data/Game", "app0:/resources"] {
            let storage = storage(raw).unwrap();
            let normalized =
                name::normalize(&name::units(&storage), &name::units("file://./")).unwrap();
            let body = normalized
                .strip_prefix(name::units("file://").as_slice())
                .unwrap();
            let split = body.iter().position(|&u| u == 47).unwrap();
            let native =
                String::from_utf16(&native(&body[..split], &body[split..]).unwrap()).unwrap();
            assert_eq!(
                storage.to_ascii_lowercase(),
                super::storage(&native).unwrap()
            );
        }
        assert!(
            name::normalize(&name::units("../../save"), &name::units("file://ux0/data/")).is_err()
        );
        assert!(storage("file://ux0/data").is_none());
        assert!(native(&name::units(".."), &name::units("/data")).is_err());
        assert!(storage("relative/game").is_none());
    }
}
