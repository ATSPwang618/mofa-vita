use super::*;
use krkr_assets::{Vfs, name};

pub const EXTENSIONS: &[&str] = &[
    ".bmp", ".dib", ".jpeg", ".jpg", ".jif", ".png", ".tlg", ".tlg5", ".tlg6", ".webp", ".ktx",
    ".kbct",
];
fn find(vfs: &mut Vfs, name: &[u16], suggest: bool) -> Result<Option<ReadPlan>> {
    if !suggest {
        let placed = vfs.placed_path(name)?;
        return if placed.is_empty() {
            Ok(None)
        } else {
            Ok(Some(vfs.plan(&placed)?))
        };
    }
    let mut candidate = Vec::with_capacity(name.len() + 5);
    candidate.extend_from_slice(name);
    for extension in EXTENSIONS {
        candidate.truncate(name.len());
        candidate.extend(extension.encode_utf16());
        if let Some(plan) = find(vfs, &candidate, false)? {
            return Ok(Some(plan));
        }
    }
    Ok(None)
}
pub fn request(
    vfs: &mut Vfs,
    name: &[u16],
    key: u32,
    province_size: Option<Size>,
    budget: Budget,
) -> Result<Request> {
    let (base, ext) = name::split_ext(name);
    let main = find(vfs, name, ext.is_empty())?
        .ok_or_else(|| Error::Asset(krkr_assets::Error::Missing(String::from_utf16_lossy(name))))?;
    let scale_name: Vec<_> = main
        .name
        .iter()
        .copied()
        .chain(crate::scale::SUFFIX.encode_utf16())
        .collect();
    let scale = find(vfs, &scale_name, false)?;
    if province_size.is_some() {
        return Ok(Request {
            main,
            scale,
            mask: None,
            province: None,
            key,
            province_size,
            grayscale: false,
            budget,
        });
    }
    let mask_base: Vec<_> = base.iter().copied().chain("_m".encode_utf16()).collect();
    let mut mask = if ext.is_empty() {
        None
    } else {
        let candidate: Vec<_> = mask_base.iter().chain(ext.iter()).copied().collect();
        find(vfs, &candidate, false)?
    };
    if mask.is_none() {
        mask = find(vfs, &mask_base, true)?;
    }
    let province_base: Vec<_> = base.iter().copied().chain("_p".encode_utf16()).collect();
    let province = find(vfs, &province_base, true)?;
    Ok(Request {
        main,
        scale,
        mask,
        province,
        key,
        province_size,
        grayscale: false,
        budget,
    })
}
/// A transition rule is luminance, whereas a province plane contains palette
/// indices. Both use the same bounded one-channel decoding/upload pipeline.
pub fn grayscale(vfs: &mut Vfs, name: &[u16], size: Size, budget: Budget) -> Result<Request> {
    let mut request = request(vfs, name, 0x02ffffff, Some(size), budget)?;
    request.grayscale = true;
    Ok(request)
}
