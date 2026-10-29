//! DEV: list images that a scene ever grades (STEC / SNX2 / SNX3 on their layer).
//! Those images must stay decode-capable: the codec8 path cannot run a bake.
use std::collections::{BTreeMap, BTreeSet};

fn field(token: &str, index: usize) -> Option<&str> {
    let inner = token.split_once('(')?.1.rsplit_once(')')?.0;
    inner.split(',').nth(index).map(str::trim)
}

fn main() {
    let path = std::env::args().nth(1).expect("allscr.mrg path");
    let out = std::env::args().nth(2).expect("output json path");
    let data = std::fs::read(&path).expect("read allscr");
    let archive = knife_core::format::script::SceneArchive::parse(&data).expect("parse allscr");
    let mut graded: BTreeSet<String> = BTreeSet::new();
    let mut per_scene: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for index in 0..archive.scene_count() {
        let text = String::from_utf8_lossy(archive.scene(index).unwrap()).into_owned();
        let mut bindings: BTreeMap<u32, String> = BTreeMap::new();
        let mut scene_hits: BTreeSet<String> = BTreeSet::new();
        for token in text.split(';') {
            let token = token.trim();
            if token.starts_with("_STCH(") || token.starts_with("_SCH2(") {
                if let (Some(layer), Some(name)) = (field(token, 0), field(token, 1)) {
                    if let Ok(layer) = layer.parse::<u32>() {
                        if !name.is_empty() {
                            bindings.insert(layer, name.to_string());
                        }
                    }
                }
                continue;
            }
            let graded_op = token.starts_with("_STEC(")
                || token.starts_with("_SNX2(")
                || token.starts_with("_SNX3(");
            if !graded_op { continue; }
            let (Some(layer), Some(value)) = (field(token, 0), field(token, 1)) else { continue };
            let Ok(layer) = layer.parse::<u32>() else { continue };
            let nonzero = value.parse::<i64>().map(|v| v != 0).unwrap_or(true);
            if !nonzero { continue; }
            if let Some(name) = bindings.get(&layer) {
                scene_hits.insert(name.clone());
            }
        }
        for name in &scene_hits { graded.insert(name.clone()); }
        if !scene_hits.is_empty() {
            per_scene.insert(format!("{index}"), scene_hits.into_iter().collect());
        }
    }
    // Plain text: one image name per line, then `#scene <index> <name>...` lines
    // for the per-scene trail. A text list needs no serializer dependency.
    let mut body = String::new();
    for name in &graded {
        body.push_str(name);
        body.push('\n');
    }
    for (scene, names) in &per_scene {
        body.push_str(&format!("#scene {scene} {}\n", names.join(" ")));
    }
    std::fs::write(&out, body).expect("write list");
    println!("scenes={} graded_images={}", archive.scene_count(), graded.len());
}