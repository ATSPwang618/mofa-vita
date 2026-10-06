//! Conservative whole-game selection. Names are resolved before normalization,
//! and companion masks in another XP3 protect their main image as well.
use crate::{
    media::{self, Result},
    normalize, psv,
};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
};

fn stem(name: &str) -> String {
    Path::new(name)
        .file_stem()
        .unwrap_or_default()
        .to_string_lossy()
        .to_lowercase()
}

fn reserved(name: &str) -> bool {
    let name = name.to_lowercase();
    [
        "mask",
        "rule",
        "font",
        "glyph",
        "button",
        "cursor",
        "system",
        "message",
        "icon",
        "window",
        "文字",
        "遮罩",
        "マスク",
    ]
    .iter()
    .any(|word| name.contains(word))
}

pub(crate) fn candidates(
    inventories: &[media::Report],
    options: &psv::Options,
) -> Result<Vec<psv::Selection>> {
    let links = inventories
        .iter()
        .map(normalize::read_links)
        .collect::<Result<Vec<_>>>()?;
    let mut protected = BTreeSet::new();
    for name in inventories
        .iter()
        .flat_map(|r| &r.entries)
        .filter(|e| e.media.as_ref().is_some_and(|m| m.kind == "image"))
        .map(|e| e.path.as_str())
        .chain(links.iter().flatten().map(|l| l.source.as_str()))
    {
        let base = stem(name);
        if let Some(main) = base.strip_suffix("_m").or_else(|| base.strip_suffix("_p")) {
            protected.insert(main.to_owned());
            protected.insert(base);
        }
    }
    // Literal rule/color-key references can be detected without executing game
    // scripts. Dynamic resource names remain a limitation of static selection.
    let mut special = BTreeSet::new();
    if options.texture_auto {
        for inventory in inventories {
            for entry in &inventory.entries {
                if !matches!(entry.extension.as_str(), "ks" | "tjs")
                    || entry.source_bytes > 8 * 1024 * 1024
                {
                    continue;
                }
                let bytes =
                    std::fs::read(inventory.root.join(&entry.path)).map_err(|e| e.to_string())?;
                let text = ["utf-8", "shift-jis"].into_iter().find_map(|encoding| {
                    krkr_assets::text::decode(
                        &bytes,
                        &krkr_assets::name::units(encoding),
                        8 * 1024 * 1024,
                    )
                    .ok()
                });
                let Some(text) = text else {
                    continue;
                };
                for line in String::from_utf16_lossy(&text).lines() {
                    let lower = line.to_lowercase();
                    if ["key", "rule", "province", "grayscale", "mask"]
                        .iter()
                        .any(|token| lower.contains(token))
                    {
                        for literal in line.split(['\'', '"']).skip(1).step_by(2) {
                            special.insert(stem(literal));
                        }
                    }
                }
            }
        }
    }
    inventories
        .iter()
        .zip(links)
        .map(|(inventory, links)| {
            let redirects: BTreeMap<_, _> = links
                .iter()
                .map(|l| (l.source.to_ascii_lowercase(), l.target.clone()))
                .collect();
            let mut protected_targets = BTreeSet::new();
            let mut reserved_targets = BTreeSet::new();
            for link in &links {
                let target = psv::follow(&link.source, &redirects)?.to_ascii_lowercase();
                if protected.contains(&stem(&link.source)) {
                    protected_targets.insert(target.clone());
                }
                if reserved(&link.source) || special.contains(&stem(&link.source)) {
                    reserved_targets.insert(target);
                }
            }
            let textures = if options.texture_auto {
                inventory
                    .entries
                    .iter()
                    .filter(|e| e.media.as_ref().is_some_and(|m| m.kind == "image"))
                    .map(|e| e.path.clone())
                    .collect()
            } else {
                psv::select_textures(inventory, &options.texture_globs)?
            };
            let mut selection = psv::Selection {
                textures,
                ..Default::default()
            };
            for name in selection.textures.clone() {
                let reason = if protected.contains(&stem(&name))
                    || protected_targets.contains(&name.to_ascii_lowercase())
                {
                    Some("mask/province image or companion in the game")
                } else if options.texture_auto
                    && (reserved(&name)
                        || special.contains(&stem(&name))
                        || reserved_targets.contains(&name.to_ascii_lowercase()))
                {
                    Some("reserved image name or literal rule/color-key reference")
                } else {
                    None
                };
                if let Some(reason) = reason {
                    if !options.texture_auto {
                        return Err(format!("lossy texture selection rejected {name}: {reason}"));
                    }
                    selection.textures.remove(&name);
                    selection.texture_skips.insert(name, reason.into());
                }
            }
            Ok(selection)
        })
        .collect()
}
