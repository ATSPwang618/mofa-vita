use anyhow::{Context, Result};
use knife_core::format::script::SceneArchive;
use knife_core::vm::parse::Scene;
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::{collections::{BTreeMap, BTreeSet}, env, fs};
#[path = "../../../knife/knife/src/motion.rs"] mod motion;
#[path = "../../../knife/knife/src/motion_loops.rs"] mod motion_loops;

#[derive(Serialize)]
struct CommandRow {
    pc: usize,
    offset: usize,
    name: String,
    args: Vec<String>,
    raw: String,
}

#[derive(Serialize)]
struct SceneRow {
    index: usize,
    name: String,
    bytes: usize,
    sha256: String,
    command_count: usize,
    command_counts: BTreeMap<String, usize>,
    material_identities: Vec<serde_json::Value>,
    effect_modes: Vec<serde_json::Value>,
    dynamic_fields: Vec<serde_json::Value>,
}

fn sha256(bytes: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(bytes);
    format!("{:x}", h.finalize())
}

fn raw(name: &str, args: &[String]) -> String {
    format!("_{}({});", name, args.join(","))
}

fn dynamic(value: &str) -> bool {
    let value = value.trim();
    if value.is_empty() { return true; }
    // Signed integer/decimal literals (including the negative constants used
    // by STMA/STGS) are resolved static values.  Do this check before the
    // expression-character heuristic so a literal `-1` is not mislabeled as
    // a runtime branch.
    if value.parse::<i64>().is_ok() || value.parse::<f64>().is_ok() { return false; }
    value.as_bytes().first().is_some_and(|c| matches!(c, b'F' | b'G' | b'Z' | b'$'))
        || value.contains('+') || value.contains('-')
}

fn identity(name: &str, grade: Option<&str>, variant: Option<&str>) -> serde_json::Value {
    let material = format!("{}|{}|{}", name, grade.unwrap_or(""), variant.unwrap_or(""));
    serde_json::json!({
        "material": name,
        "grade": grade,
        "variant": variant,
        "identity_sha256": sha256(material.as_bytes()),
        "dynamic": dynamic(name)
    })
}

fn scene_row(index: usize, name: &str, bytes: &[u8]) -> SceneRow {
    let parsed = Scene::parse(bytes);
    let mut command_counts = BTreeMap::new();
    let mut mats = BTreeSet::new();
    let mut material_identities = Vec::new();
    let mut effect_modes = Vec::new();
    let mut dynamic_fields = Vec::new();

    for (pc, c) in parsed.commands.iter().enumerate() {
        *command_counts.entry(c.name.clone()).or_insert(0) += 1;
        if c.name == "STCH" || c.name == "SCH2" {
            let material = c.arg(1).unwrap_or("");
            let grade = c.arg(7);
            let variant = match (c.arg(9), c.arg(10)) {
                (Some(a), Some(b)) => Some(format!("{},{}", a, b)),
                (Some(a), None) => Some(a.to_owned()),
                (None, Some(b)) => Some(b.to_owned()),
                _ => None,
            };
            let key = format!("{}|{}|{}", material, grade.unwrap_or(""), variant.as_deref().unwrap_or(""));
            if mats.insert(key) { material_identities.push(identity(material, grade, variant.as_deref())); }
            for (field, value) in [("material", c.arg(1)), ("grade", grade), ("variant_a", c.arg(9)), ("variant_b", c.arg(10))] {
                if let Some(value) = value.filter(|v| dynamic(v)) {
                    dynamic_fields.push(serde_json::json!({"pc": pc, "command": c.name.as_str(), "field": field, "value": value, "reason": "runtime expression"}));
                }
            }
        }
        if c.name == "STBR" {
            let mode = c.int(1);
            effect_modes.push(serde_json::json!({
                "pc": pc,
                "layer": c.int(0),
                "mode": mode,
                "mode_status": if mode.is_some() { "known_numeric" } else { "dynamic_or_unknown" },
                "raw": raw(&c.name, &c.args)
            }));
            if mode.is_none() { dynamic_fields.push(serde_json::json!({"pc": pc, "command": "STBR", "field": "mode", "value": c.arg(1), "reason": "runtime expression"})); }
        }
    }
    SceneRow { index, name: name.to_owned(), bytes: bytes.len(), sha256: sha256(bytes), command_count: parsed.commands.len(), command_counts, material_identities, effect_modes, dynamic_fields }
}

fn command_row(pc: usize, c: &knife_core::vm::parse::Command) -> CommandRow {
    CommandRow { pc, offset: c.offset, name: c.name.clone(), args: c.args.clone(), raw: raw(&c.name, &c.args) }
}

fn main() -> Result<()> {
    let mut args = env::args().skip(1);
    let path = args.next().context("usage: script-census <allscr.mrg> [output.json]")?;
    let output = args.next();
    let bytes = fs::read(&path).with_context(|| format!("reading {path}"))?;
    let archive = SceneArchive::parse(&bytes).context("parsing allscr.mrg with production SceneArchive")?;
    let mut scenes = Vec::with_capacity(archive.scene_count());
    let nso = knife_core::exe::Nso::from_bundle(include_bytes!("../../../knife/knife/data/exe_tables_witch.bin"))?;
    let motions = motion::MotionTable::load(&nso)?;
    let mut motion_ids = BTreeSet::new();
    for i in 0..archive.scene_count() {
        let parsed = Scene::parse(archive.scene(i).unwrap_or_default());
        for command in &parsed.commands {
            if command.name == "STMA" {
                if let Some(id) = command.int(1).filter(|id| *id >= 0) { motion_ids.insert(id); }
            }
        }
        scenes.push(scene_row(i, archive.names.get(i).map(String::as_str).unwrap_or(""), archive.scene(i).unwrap_or_default()));
    }
    let motion_inventory: Vec<_> = motion_ids.into_iter().map(|id| {
        let length = motions.length(id) as f32;
        let samples: Vec<_> = [0.0, length * 0.5, length].into_iter().map(|time| {
            let mut state = motion::MotionState::new(id, 0.0);
            let fields = motions.advance(&mut state, time, &[0.0; motion::FIELDS]);
            serde_json::json!({"time_ms":time,"blend":fields.blend,"fill":fields.fill,
                "variant":fields.variant,"snx2":fields.snx2,"snx3":fields.snx3,
                "clip":fields.clip,"clip_mode":fields.clip_mode,"rot2":fields.rot2,
                "state_authority":"production motion parser with zero baseline; current/HOLD fields remain runtime-dependent"})
        }).collect();
        serde_json::json!({"id":id,"length_ms":length,"looping":motions.looping(id),"samples":samples})
    }).collect();
    let fixture_scene = 38usize;
    let fixture_commands = archive.scene(fixture_scene).map(Scene::parse).map(|scene| {
        let lo = 1221usize;
        let hi = 1247usize.min(scene.commands.len());
        (lo..hi).map(|pc| command_row(pc, &scene.commands[pc])).collect::<Vec<_>>()
    }).unwrap_or_default();
    let report = serde_json::json!({
        "schema": "knife-script-census-v1",
        "execution": {
            "status": "completed",
            "worker": "tools/script-census",
            "parser_authority": "production-parser-only",
            "scene_count": archive.scene_count(),
            "asset_bytes": bytes.len(),
            "asset_sha256": sha256(&bytes)
        },
        "source": {"path": path, "allscr_sha256": sha256(&bytes), "parser": "knife-core::format::script::SceneArchive + knife-core::vm::parse::Scene"},
        "scene_count": archive.scene_count(),
        "scenes": scenes,
        "motion_parser":"knife/knife/src/motion.rs + motion_loops.rs",
        "motion_table_sha256":sha256(include_bytes!("../../../knife/knife/data/exe_tables_witch.bin")),
        "motions":motion_inventory,
        "unknown_state_policy":"dynamic VM branches, expression evaluation and inherited/HOLD fields are not statically resolved; full renderer path remains authoritative",
        "fixture": {
            "scene_index": fixture_scene,
            "scene_name": archive.names.get(fixture_scene),
            "fixed_phases_ms": [1500, 2000, 2300, 2500, 3000, 3500, 4000, 4500, 5000],
            "pc_range": [1221, 1246],
            "commands": fixture_commands,
            "representative_contracts": [
                {"contract": "parent0_fullscreen_mask", "status": "candidate_requires_runtime_pixel_proof"},
                {"contract": "same_geometry_35_to_34", "status": "candidate_requires_rgba8_rounding_proof"},
                {"contract": "2_37_38_op7_chain", "status": "candidate_cost_gated"}
            ],
            "runtime_integration": {"status": "not_run", "required_entry": "Game::render_stage", "required_observations": ["GXM draw/scene/pass", "producer clear state", "pixel/mask content versions", "final frame diff"]}
        }
    });
    let text = serde_json::to_string_pretty(&report)?;
    if let Some(out) = output { fs::write(out, text)?; } else { println!("{text}"); }
    Ok(())
}
