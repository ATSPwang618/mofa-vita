use super::test_backend::{Control, Event};
use super::*;

/// Bytes one scene target occupies after the 256 KiB CDRAM granule, in either variant.
fn scene_bytes() -> usize {
    let pixels = RENDER_W as usize * RENDER_H as usize * 4;
    pixels.div_ceil(256 * 1024) * (256 * 1024)
}

fn setup() -> (Renderer, Control) {
    let gxm = Gxm::new().unwrap();
    let control = gxm.control.clone();
    let renderer = Renderer::new_with_gxm(gxm).unwrap();
    control.borrow_mut().events.clear();
    (renderer, control)
}

fn decode_deferred_test(bytes: &[u8]) -> Result<(u32, u32, Vec<u8>, bool)> {
    ensure!(bytes.len() == 2 && bytes[0] != 0, "invalid test encoded image");
    let size = u32::from(bytes[0]);
    Ok((size, size, vec![bytes[1]; (size * size * 4) as usize], true))
}

fn submit_deferred(r: &mut Renderer, canvas: &RenderTarget, textures: &[TexRef]) -> Result<()> {
    let quads = textures.iter().map(|t| Quad::rect(t.clone(), [0.0, 0.0, 2.0, 2.0], [1.0; 4])).collect::<Vec<_>>();
    let mut encoder = r.encoder();
    r.try_draw(&mut encoder, canvas, None, &quads)?;
    r.try_present(encoder, canvas)
}

fn gxt_fixture([width, height]: [u16; 2], format: u32) -> Vec<u8> {
    let backing = u32::from(width).next_power_of_two().max(4)
        * u32::from(height).next_power_of_two().max(4);
    let blocks = if format == 0x8500_0000 { backing / 2 } else { backing } as usize;
    let stored = (blocks + 15) & !15;
    let mut bytes = vec![0u8; 64 + stored];
    for (at, value) in [(0, 0x0054_5847u32), (4, 0x1000_0003), (8, 1),
        (12, 64), (16, stored as u32), (32, 64), (36, stored as u32),
        (40, u32::MAX), (48, 0xa000_0000), (52, format)]
    {
        bytes[at..at + 4].copy_from_slice(&value.to_le_bytes());
    }
    bytes[56..58].copy_from_slice(&width.to_le_bytes());
    bytes[58..60].copy_from_slice(&height.to_le_bytes());
    bytes[60] = 1;
    bytes[64..64 + blocks].fill(0x5a);
    bytes
}

fn extract_test_gxt(bytes: &[u8]) -> Result<DeferredGxt<'_>> {
    ensure!(bytes.len() >= 64, "short test GXT");
    let width = u32::from(u16::from_le_bytes(bytes[56..58].try_into()?));
    let height = u32::from(u16::from_le_bytes(bytes[58..60].try_into()?));
    Ok(DeferredGxt { logical: [width * 2, height * 2], physical: [width, height], file: bytes })
}

fn defer_test_gxt(r: &Renderer, bytes: &[u8], load: Rc<dyn Fn() -> Result<Vec<u8>>>) -> Result<TexRef> {
    let view = extract_test_gxt(bytes)?;
    r.defer_gxt_altered_reload(view.logical, view.physical, bytes, "gxt-test", load, extract_test_gxt)
}

#[test]
fn cache_admission_avoids_registry_walks_without_pressure_and_sorts_only_missing_images() {
    let (mut r, _) = setup();
    let canvas = r.create_target(2, 2, "canvas-test");
    let bytes = gxt_fixture([4, 4], 0x8500_0000);
    let make = || defer_test_gxt(&r, &bytes, {
        let bytes = bytes.clone(); Rc::new(move || Ok(bytes.clone()))
    }).unwrap();
    let a = make();
    let b = make();
    let c = make();
    submit_deferred(&mut r, &canvas, &[a.clone(), b.clone()]).unwrap();
    assert_eq!(r.device.resident_deferred_bytes.get(), 2 * CDRAM_GRANULE);
    assert_eq!(r.device.admission_scans.get(), 0, "available headroom needs no victim registry");
    assert_eq!(r.device.preparation_sorts.get(), 1);
    for _ in 0..8 {
        submit_deferred(&mut r, &canvas, &[a.clone(), b.clone()]).unwrap();
    }
    submit_deferred(&mut r, &canvas, &[a.clone(), b.clone(), c.clone()]).unwrap();
    let spare = r.try_create_target(2, 2, "spare target").unwrap();
    assert_eq!(r.device.preparation_sorts.get(), 1, "resident images and a single miss need no sort");
    assert_eq!(r.device.admission_scans.get(), 0);
    assert_eq!(r.device.resident_deferred_bytes.get(), 3 * CDRAM_GRANULE);
    // The same bookkeeping must still enforce LRU and the cache cap under pressure.
    r.device.deferred_budget.set(3 * CDRAM_GRANULE);
    let d = defer_test_gxt(&r, &bytes, { let bytes = bytes.clone(); Rc::new(move || Ok(bytes.clone())) }).unwrap();
    submit_deferred(&mut r, &canvas, &[a.clone(), c.clone(), d.clone()]).unwrap();
    assert_eq!(r.device.admission_scans.get(), 1);
    assert!(a.native.resident_id().is_ok() && c.native.resident_id().is_ok());
    assert!(b.native.resident_id().is_err(), "only the unpinned atlas can be reclaimed");
    drop((a, b, c, d, spare));
    assert_eq!(r.device.resident_deferred_bytes.get(), 0);
}

#[test]
fn transition_capture_is_pinned_by_commands_and_retires_before_the_next_frame() {
    let (mut r, control) = setup();
    let scene = r.create_scene_target(CANVAS_W, CANVAS_H, "scene");
    let canvas = r.create_scene_target(CANVAS_W, CANVAS_H, "canvas");
    let baseline = r.device.call(|g| g.available_cdram()).unwrap();
    let old = r.create_scene_target(CANVAS_W, CANVAS_H, "old frame");
    let id = old.tex.native.resident_id().unwrap().0;
    let charge = baseline - r.device.call(|g| g.available_cdram()).unwrap();
    assert_eq!(charge, scene_bytes(), "the scene target is charged at its physical size");
    let mut encoder = r.encoder();
    r.copy_target(&mut encoder, &scene, &old);
    r.transition(&mut encoder, &canvas, &old.tex, &scene.tex, None, 0.5, 64.0);
    drop(old);
    assert!(!control.borrow().events.iter().any(|e| matches!(e, Event::Destroy(v) if *v == id)));
    assert_eq!(r.device.call(|g| g.available_cdram()).unwrap(), baseline - charge);
    r.try_present(encoder, &canvas).unwrap();
    let trace = control.borrow();
    let copy = trace.events.iter().position(|e| matches!(e, Event::Copy { dst, .. } if *dst == id)).unwrap();
    let draw = trace.events.iter().position(|e| matches!(e,
        Event::Draw { shader: EffectShader::Transition, textures, .. } if textures.contains(&Some(id)))).unwrap();
    let destroy = trace.events.iter().position(|e| matches!(e, Event::Destroy(v) if *v == id)).unwrap();
    assert!(copy < draw && draw < destroy);
    assert_eq!(trace.events.iter().filter(|e| matches!(e, Event::Destroy(v) if *v == id)).count(), 1);
    drop(trace);
    assert_eq!(r.device.call(|g| g.available_cdram()).unwrap(), baseline);
    control.borrow_mut().events.clear();
    // Ordinary scene frames need no old surface. A later FADS creates and
    // copies a new snapshot before sampling it; it cannot reuse retired pixels.
    let mut ordinary = r.encoder();
    r.copy_target(&mut ordinary, &scene, &canvas);
    r.try_present(ordinary, &canvas).unwrap();
    assert!(!control.borrow().events.iter().any(|e| matches!(e, Event::Create { .. })));
    let next = r.create_scene_target(CANVAS_W, CANVAS_H, "next capture");
    assert_ne!(next.tex.native.resident_id().unwrap().0, id);
    let mut encoder = r.encoder();
    r.copy_target(&mut encoder, &scene, &next);
    r.transition(&mut encoder, &canvas, &next.tex, &scene.tex, None, 0.0, 64.0);
    r.try_present(encoder, &canvas).unwrap();
    drop(next);
    assert_eq!(r.device.call(|g| g.available_cdram()).unwrap(), baseline);
}

#[test]
fn recorded_scratch_copy_retires_after_sampling_without_releasing_persistent_surfaces() {
    let (mut r, control) = setup();
    let canvas = r.create_scene_target(CANVAS_W, CANVAS_H, "canvas");
    let pixel = r.create_scene_target(CANVAS_W, CANVAS_H, "group pixel");
    let mask = r.create_scene_target(CANVAS_W, CANVAS_H, "group mask");
    let baseline = r.device.call(|g| g.available_cdram()).unwrap();
    let scratch = r.create_scene_target(CANVAS_W, CANVAS_H, "group composite");
    let scratch_id = scratch.tex.native.resident_id().unwrap().0;
    let pixel_id = pixel.tex.native.resident_id().unwrap().0;
    let mask_id = mask.tex.native.resident_id().unwrap().0;
    let full = [0.0, 0.0, CANVAS_W as f32, CANVAS_H as f32];
    let mut enc = r.encoder();
    r.copy_target(&mut enc, &pixel, &scratch);
    r.draw(&mut enc, &scratch, None, &[
        Quad { blend: Blend::MaskAlpha, ..Quad::rect(mask.tex.clone(), full, [1.0; 4]) },
    ]);
    r.draw(&mut enc, &canvas, Some([0.0; 4]), &[
        Quad::rect(scratch.tex.clone(), full, [1.0; 4]),
    ]);
    drop(scratch);
    assert!(!control.borrow().events.iter().any(|e| matches!(e, Event::Destroy(id) if *id == scratch_id)));
    r.try_present(enc, &canvas).unwrap();
    let trace = control.borrow();
    let copy = trace.events.iter().position(|e| matches!(e, Event::Copy { src, dst }
        if *src == pixel_id && *dst == scratch_id)).unwrap();
    let masked = trace.events.iter().position(|e| matches!(e, Event::Draw { blend: EffectBlend::MaskAlpha, textures, .. }
        if textures.contains(&Some(mask_id)))).unwrap();
    let sampled = trace.events.iter().position(|e| matches!(e, Event::Draw { textures, .. }
        if textures.contains(&Some(scratch_id)))).unwrap();
    let retired = trace.events.iter().position(|e| matches!(e, Event::Destroy(id) if *id == scratch_id)).unwrap();
    assert!(copy < masked && masked < sampled && sampled < retired);
    assert!(!trace.events.iter().any(|e| matches!(e, Event::Destroy(id) if [pixel_id, mask_id].contains(id))));
    drop(trace);
    assert_eq!(r.device.call(|g| g.available_cdram()).unwrap(), baseline);
    // A later composite must first copy persistent pixels into a fresh scratch.
    let next = r.create_scene_target(CANVAS_W, CANVAS_H, "group composite");
    let next_id = next.tex.native.resident_id().unwrap().0;
    assert_ne!(next_id, scratch_id);
    let mut enc = r.encoder();
    r.copy_target(&mut enc, &pixel, &next);
    r.draw(&mut enc, &canvas, None, &[Quad::rect(next.tex.clone(), full, [1.0; 4])]);
    r.try_present(enc, &canvas).unwrap();
    drop(next);
    assert_eq!(r.device.call(|g| g.available_cdram()).unwrap(), baseline);
}

#[test]
fn scene_scratch_preserves_snapshot_and_orders_group_reads_before_scene_rebuild() {
    let (mut r, control) = setup();
    let [stage, scene, old, canvas, pixel, mask] =
        ["stage", "scene", "old", "canvas", "pixel", "mask"]
            .map(|name| r.create_scene_target(CANVAS_W, CANVAS_H, name));
    let ids = [&stage, &scene, &old, &canvas, &pixel, &mask]
        .map(|t| t.tex.native.resident_id().unwrap().0);
    let headroom = r.device.call(|g| g.available_cdram()).unwrap();
    control.borrow_mut().events.clear();
    let mut enc = r.encoder();
    r.copy_target(&mut enc, &scene, &old);
    for alpha in [0.37, 1.0] {
        r.try_group_composite_via_scratch(&mut enc, &stage, &pixel, &mask,
            &scene, Blend::Alpha, alpha).unwrap();
    }
    r.copy_target(&mut enc, &stage, &scene);
    r.transition(&mut enc, &canvas, &old.tex, &scene.tex, None, 0.5, 64.0);
    r.try_present(enc, &canvas).unwrap();
    assert_eq!(r.device.call(|g| g.available_cdram()).unwrap(), headroom);
    let trace = control.borrow();
    assert!(!trace.events.iter().any(|e| matches!(e, Event::Create { .. } | Event::Destroy(_))));
    let copies: Vec<_> = trace.events.iter().enumerate().filter_map(|(i, e)| match e {
        Event::Copy { src, dst } => Some((i, *src, *dst)), _ => None,
    }).collect();
    assert_eq!(copies.iter().map(|&(_, a, b)| (a,b)).collect::<Vec<_>>(),
        [(ids[1], ids[2]), (ids[4], ids[1]), (ids[4], ids[1]), (ids[0], ids[1])]);
    for index in 0..2 {
        let start = copies[index + 1].0;
        let end = copies[index + 2].0;
        let mut active = None;
        let draws: Vec<_> = trace.events[start + 1..end].iter().filter_map(|e| match e {
            Event::Begin { id, .. } => { active = Some(*id); None },
            Event::End => { active = None; None },
            Event::Draw { shader, blend, textures, vertices, .. } =>
                Some((active.unwrap(), *shader, *blend, textures[0], vertices[0].color[3])),
            _ => None,
        }).collect();
        assert_eq!(draws, [
            (ids[1], EffectShader::Sprite, EffectBlend::MaskAlpha,
                Some(ids[5]), 1.0),
            (ids[0], EffectShader::Sprite, EffectBlend::Alpha,
                Some(ids[1]), [0.37, 1.0][index]),
        ]);
    }
    let last_copy = copies.last().unwrap().0;
    assert!(trace.events[last_copy + 1..].iter().any(|e| matches!(e,
        Event::Draw { shader: EffectShader::Transition, textures, .. }
        if textures[..2] == [Some(ids[2]), Some(ids[1])])));
}

#[test]
fn scratch_group_rejects_aliases_and_bad_layout_before_recording_commands() {
    let (mut r, control) = setup();
    let [target, pixel, mask, scratch] = ["target", "pixel", "mask", "scratch"]
        .map(|name| r.create_scene_target(CANVAS_W, CANVAS_H, name));
    let bad = r.create_target(4, 4, "wrong size");
    control.borrow_mut().events.clear();
    for (px, mk, tmp, alpha) in [(&pixel, &mask, &target, 1.0),
        (&pixel, &mask, &pixel, 1.0), (&pixel, &mask, &mask, 1.0),
        (&target, &mask, &scratch, 1.0), (&pixel, &target, &scratch, 1.0),
        (&pixel, &mask, &bad, 1.0), (&pixel, &mask, &scratch, f32::NAN)]
    {
        let mut enc = r.encoder();
        assert!(r.try_group_composite_via_scratch(&mut enc, &target, px, mk, tmp,
            Blend::Alpha, alpha).is_err());
        assert!(enc.commands.is_empty());
    }
    assert!(control.borrow().events.is_empty());
}

#[test]
fn fused_group_composite_records_one_draw_without_scratch_or_copy_for_native_blends() {
    let (mut r, control) = setup();
    let canvas = r.create_scene_target(CANVAS_W, CANVAS_H, "canvas");
    let pixel = r.create_scene_target(CANVAS_W, CANVAS_H, "group pixel");
    let mask = r.create_scene_target(CANVAS_W, CANVAS_H, "group mask");
    let pixel_id = pixel.tex.native.resident_id().unwrap().0;
    let mask_id = mask.tex.native.resident_id().unwrap().0;
    let headroom = r.device.call(|g| g.available_cdram()).unwrap();
    control.borrow_mut().events.clear();
    for blend in [Blend::Alpha, Blend::Add, Blend::Sub, Blend::Multiply, Blend::Screen,
        Blend::Opaque, Blend::Over, Blend::Footprint, Blend::MaskAlpha]
    {
        let mut enc = r.encoder();
        r.try_group_composite(&mut enc, &canvas, &pixel, &mask, blend, 0.37).unwrap();
        assert_eq!(enc.commands.len(), 3);
        assert_eq!(r.device.call(|g| g.available_cdram()).unwrap(), headroom);
        r.try_present(enc, &canvas).unwrap();
    }
    let trace = control.borrow();
    assert!(!trace.events.iter().any(|e| matches!(e, Event::Create { .. } | Event::Copy { .. })));
    let draws: Vec<_> = trace.events.iter().filter_map(|e| match e {
        Event::Draw { shader: EffectShader::GroupComposite, textures, vertices, blend, .. } => {
            assert_eq!(textures[..2], [Some(pixel_id), Some(mask_id)]);
            assert!(vertices.iter().all(|v| v.color == [1., 1., 1., 0.37]));
            Some(blend)
        },
        _ => None,
    }).collect();
    assert_eq!(draws.len(), 9);
}

#[test]
fn fused_ps_group_uses_only_the_existing_per_draw_backdrop_and_keeps_the_mask_separate() {
    let (mut r, control) = setup();
    let canvas = r.create_scene_target(CANVAS_W, CANVAS_H, "canvas");
    let pixel = r.create_scene_target(CANVAS_W, CANVAS_H, "group pixel");
    let mask = r.create_scene_target(CANVAS_W, CANVAS_H, "group mask");
    let ids = [&canvas, &pixel, &mask].map(|t| t.tex.native.resident_id().unwrap().0);
    let headroom = r.device.call(|g| g.available_cdram()).unwrap();
    control.borrow_mut().events.clear();
    let mut enc = r.encoder();
    for op in [PsOp::Add, PsOp::Darken, PsOp::Lighten, PsOp::LinearBurn, PsOp::Burn,
        PsOp::Dodge, PsOp::DodgeAlpha, PsOp::Difference, PsOp::DifferenceAlpha,
        PsOp::Exclusion, PsOp::HardLight, PsOp::HardLightEx, PsOp::Overlay,
        PsOp::SoftLight, PsOp::Sub, PsOp::AddNoAlpha, PsOp::SubNoAlpha,
        PsOp::NoAlpha, PsOp::Normal, PsOp::DarkenColorAlpha, PsOp::LightenColorAlpha,
        PsOp::ScreenColorAlpha, PsOp::MultiplyDst, PsOp::Copy]
    {
        r.try_group_composite(&mut enc, &canvas, &pixel, &mask, Blend::Ps(op), 0.37).unwrap();
    }
    assert_eq!(enc.commands.len(), 24 * 4);
    assert_eq!(headroom - r.device.call(|g| g.available_cdram()).unwrap(), scene_bytes());
    r.try_present(enc, &canvas).unwrap();
    let trace = control.borrow();
    let backdrop: Vec<_> = trace.events.iter().filter_map(|e| match e {
        Event::Create { id, target: true, .. } => Some(*id), _ => None,
    }).collect();
    assert_eq!(backdrop.len(), 1, "one reusable backdrop, no group scratch");
    let bd = backdrop[0];
    assert_eq!(trace.events.iter().filter(|e| matches!(e, Event::Copy { src, dst }
        if *src == ids[0] && *dst == bd)).count(), 24);
    let mut seen = 0;
    for e in &trace.events {
        if let Event::Draw { shader: EffectShader::Ps, textures, params, blend, .. } = e {
            seen += 1;
            assert_eq!(textures[..3], [Some(ids[1]), Some(bd), Some(ids[2])]);
            assert_eq!(*blend, EffectBlend::Opaque);
            assert_eq!(*params, [seen as f32, 0., RENDER_W as f32, RENDER_H as f32, -1., 0., 0., 1.]);
        }
    }
    assert_eq!(seen, 24);
}

#[test]
fn fused_group_rejects_invalid_ownership_layout_and_feedback_before_any_allocation() {
    let (mut r, control) = setup();
    let (mut other, _) = setup();
    let target = r.create_scene_target(CANVAS_W, CANVAS_H, "target");
    let pixel = r.create_scene_target(CANVAS_W, CANVAS_H, "pixel");
    let mask = r.create_scene_target(CANVAS_W, CANVAS_H, "mask");
    let wrong_physical = r.create_target(CANVAS_W, CANVAS_H, "wrong physical");
    let wrong_logical = r.create_target(RENDER_W, RENDER_H, "wrong logical");
    let foreign = other.create_scene_target(CANVAS_W, CANVAS_H, "foreign");
    control.borrow_mut().events.clear();
    for (dst, px, mk, alpha) in [(&target,&pixel,&mask,f32::NAN),
        (&target,&pixel,&mask,f32::INFINITY), (&foreign,&pixel,&mask,1.),
        (&target,&foreign,&mask,1.), (&target,&pixel,&foreign,1.),
        (&target,&wrong_physical,&mask,1.), (&target,&pixel,&wrong_logical,1.),
        (&target,&target,&mask,1.), (&target,&pixel,&target,1.)]
    {
        let mut enc = r.encoder();
        assert!(r.try_group_composite(&mut enc, dst, px, mk, Blend::Ps(PsOp::Normal), alpha).is_err());
        assert!(enc.commands.is_empty());
    }
    let mut foreign_encoder = other.encoder();
    assert!(r.try_group_composite(&mut foreign_encoder, &target, &pixel, &mask,
        Blend::Ps(PsOp::Normal), 1.).is_err());
    assert!(control.borrow().events.is_empty());
}

#[test]
fn baked_ui_cache_does_not_consume_one_render_target_per_image() {
    let (mut r, control) = setup();
    let canvas = r.create_target(2, 2, "canvas-test");
    let baseline = r.device.call(|g| Ok(g.live_render_targets())).unwrap();
    // Only one extra target is available for the entire preparation pass.
    control.borrow_mut().render_target_limit = Some(baseline + 1);
    let images = (0..96).map(|i| r.defer_altered([2, 2], [2, 2],
        Arc::from([2, i as u8]), &format!("ui-{i}"), decode_deferred_test).unwrap()).collect::<Vec<_>>();
    submit_deferred(&mut r, &canvas, &images).unwrap();
    assert!(images.iter().all(|image| image.native.resident_id().is_ok()));
    assert_eq!(r.device.resident_deferred_bytes.get(), images.len() * CDRAM_GRANULE);
    assert_eq!(r.device.call(|g| Ok(g.live_render_targets())).unwrap(), baseline);
    let trace = control.borrow();
    let mut sealed = 0;
    for (i, event) in trace.events.iter().enumerate() {
        if let Event::Seal(id) = event {
            sealed += 1;
            assert!(matches!(trace.events[i - 1], Event::End), "finish the pass before attachment release");
            assert!(images.iter().any(|image| image.native.resident_id().unwrap().0 == *id));
        }
    }
    assert_eq!(sealed, images.len());
    drop(trace);
    let before = control.borrow().events.len();
    submit_deferred(&mut r, &canvas, &images).unwrap();
    assert!(!control.borrow().events[before..].iter().any(|event|
        matches!(event, Event::Create { .. } | Event::Draw { shader: EffectShader::Alter, .. })));
}

#[test]
fn attachment_release_failure_does_not_publish_baked_pixels_or_cache_charge() {
    let (mut r, control) = setup();
    let canvas = r.create_target(2, 2, "canvas-test");
    let baseline = r.device.call(|g| Ok(g.live_render_targets())).unwrap();
    let image = r.defer_altered([2, 2], [2, 2], Arc::from([2, 73]), "seal-failure", decode_deferred_test).unwrap();
    control.borrow_mut().fail_next_seal = true;
    let error = submit_deferred(&mut r, &canvas, &[image.clone()]).unwrap_err();
    assert!(format!("{error:#}").contains("sealing deferred atlas seal-failure"));
    assert!(image.native.resident_id().is_err());
    assert_eq!(r.device.resident_deferred_bytes.get(), 0);
    assert_eq!(r.device.call(|g| Ok(g.live_render_targets())).unwrap(), baseline);
    assert!(control.borrow().events.iter().any(|event| matches!(event, Event::End)));
    assert!(!control.borrow().events.iter().any(|event| matches!(event, Event::Seal(_))));
    // A transient failure in this double can be retried; the native SDK failure
    // tests separately ensure an uncertain teardown retains a closing C owner.
    submit_deferred(&mut r, &canvas, &[image.clone()]).unwrap();
    assert!(image.native.resident_id().is_ok());
    assert_eq!(r.device.resident_deferred_bytes.get(), CDRAM_GRANULE);
    assert_eq!(r.device.call(|g| Ok(g.live_render_targets())).unwrap(), baseline);
}

#[test]
fn deferred_resident_charge_lasts_until_the_final_encoder_reference_is_released() {
    let (mut r, control) = setup();
    let canvas = r.create_target(2, 2, "canvas-test");
    let texture = r.defer_altered([2, 2], [2, 2], Arc::from([2, 73]), "retained", decode_deferred_test).unwrap();
    submit_deferred(&mut r, &canvas, &[texture.clone()]).unwrap();
    let id = texture.native.resident_id().unwrap().0;
    let mut pending = r.encoder();
    r.try_draw(&mut pending, &canvas, None,
        &[Quad::rect(texture.clone(), [0.0, 0.0, 2.0, 2.0], [1.0; 4])]).unwrap();
    drop(texture);
    assert_eq!(r.device.resident_deferred_bytes.get(), CDRAM_GRANULE);
    drop(pending);
    assert_eq!(r.device.resident_deferred_bytes.get(), 0);
    assert_eq!(control.borrow().events.iter().filter(|e| matches!(e, Event::Destroy(v) if *v == id)).count(), 1);
}

#[test]
fn failed_uploads_and_bakes_never_commit_resident_cache_bytes() {
    let (mut r, control) = setup();
    let canvas = r.create_target(2, 2, "canvas-test");
    let bytes = gxt_fixture([4, 4], 0x8500_0000);
    let compressed = defer_test_gxt(&r, &bytes, { let b = bytes.clone(); Rc::new(move || Ok(b.clone())) }).unwrap();
    control.borrow_mut().fail_next_compressed_create = true;
    assert!(submit_deferred(&mut r, &canvas, &[compressed.clone()]).is_err());
    assert_eq!(r.device.resident_deferred_bytes.get(), 0);
    let rgba = r.defer_altered([2, 2], [2, 2], Arc::from([2, 12]), "failed-bake", decode_deferred_test).unwrap();
    control.borrow_mut().fail_next_draw = true;
    assert!(submit_deferred(&mut r, &canvas, &[rgba.clone()]).is_err());
    assert_eq!(r.device.resident_deferred_bytes.get(), 0);
    submit_deferred(&mut r, &canvas, &[rgba.clone(), compressed.clone()]).unwrap();
    assert_eq!(r.device.resident_deferred_bytes.get(), 2 * CDRAM_GRANULE);
    drop((rgba, compressed));
    assert_eq!(r.device.resident_deferred_bytes.get(), 0);
}

#[test]
fn frame_admission_reclaims_idle_cg_without_redecoding_or_submitting_partial_game_draws() {
    let (mut r, control) = setup();
    let canvas = r.create_target(2, 2, "canvas-test");
    let active = r.create_target(2, 2, "active CG");
    let mut idle = Some(r.create_target(2, 2, "idle CG"));
    let idle_id = idle.as_ref().unwrap().tex.native.resident_id().unwrap().0;
    let reads = Rc::new(Cell::new(0));
    let load = { let reads = reads.clone(); Rc::new(move || {
        reads.set(reads.get() + 1);
        Ok(vec![2, 73])
    }) };
    let texture = r.defer_altered_reload([2, 2], [2, 2], 2, "UI miss", load, decode_deferred_test).unwrap();
    let mut encoder = r.encoder();
    r.try_draw(&mut encoder, &canvas, None, &[
        Quad::rect(active.tex.clone(), [0.0, 0.0, 2.0, 2.0], [1.0; 4]),
        Quad::rect(texture.clone(), [0.0, 0.0, 2.0, 2.0], [1.0; 4]),
    ]).unwrap();
    let available = r.device.call(|g| g.available_cdram()).unwrap();
    control.borrow_mut().external_cdram_bytes = available - CDRAM_GRANULE;
    control.borrow_mut().events.clear();
    let mut admissions = 0;
    r.try_present_with_admission(encoder, &canvas, |reserve| {
        admissions += 1;
        assert!(reserve().is_err(), "the existing idle CG blocks the source + target peak");
        assert!(control.borrow().events.is_empty(), "no partial frame or temporary upload before admission");
        drop(idle.take());
        reserve()
    }).unwrap();
    assert_eq!(admissions, 1);
    assert_eq!(reads.get(), 1, "native pressure retries must not reload or decode the same image");
    assert!(active.tex.native.resident_id().is_ok());
    let trace = control.borrow();
    assert_eq!(trace.events.iter().filter(|e| matches!(e, Event::Destroy(id) if *id == idle_id)).count(), 1);
    assert_eq!(trace.events.iter().filter(|e| matches!(e, Event::Draw { shader: EffectShader::Alter, .. })).count(), 1);
    assert_eq!(trace.events.iter().filter(|e| matches!(e, Event::Present(_))).count(), 1);
    assert!(matches!(trace.events.first(), Some(Event::Destroy(id)) if *id == idle_id));
}

#[test]
fn frame_admission_keeps_complete_frame_pins_and_does_not_retry_native_upload_failure() {
    let (mut r, control) = setup();
    let canvas = r.create_target(2, 2, "canvas-test");
    let bytes = gxt_fixture([4, 4], 0x8500_0000);
    let a = defer_test_gxt(&r, &bytes, { let b = bytes.clone(); Rc::new(move || Ok(b.clone())) }).unwrap();
    let b = defer_test_gxt(&r, &bytes, { let b = bytes.clone(); Rc::new(move || Ok(b.clone())) }).unwrap();
    submit_deferred(&mut r, &canvas, &[a.clone()]).unwrap();
    r.device.deferred_budget.set(CDRAM_GRANULE);
    let record = |r: &mut Renderer| {
        let mut enc = r.encoder();
        r.try_draw(&mut enc, &canvas, None, &[
            Quad::rect(a.clone(), [0.0, 0.0, 2.0, 2.0], [1.0; 4]),
            Quad::rect(b.clone(), [0.0, 0.0, 2.0, 2.0], [1.0; 4]),
        ]).unwrap();
        enc
    };
    let mut calls = 0;
    control.borrow_mut().events.clear();
    let enc = record(&mut r);
    assert!(r.try_present_with_admission(enc, &canvas, |reserve| { calls += 1; reserve() }).is_err());
    assert_eq!(calls, 0, "a frame larger than its budget fails before cross-cache eviction");
    assert!(a.native.resident_id().is_ok());
    assert!(control.borrow().events.is_empty());
    r.device.deferred_budget.set(2 * CDRAM_GRANULE);
    control.borrow_mut().fail_next_compressed_create = true;
    let enc = record(&mut r);
    assert!(r.try_present_with_admission(enc, &canvas, |reserve| { calls += 1; reserve() }).is_err());
    assert_eq!(calls, 1, "only admission retries; a native upload failure is propagated");
    assert!(a.native.resident_id().is_ok() && b.native.resident_id().is_err());
    assert!(!control.borrow().events.iter().any(|e| matches!(e, Event::Present(_) | Event::Draw { shader: EffectShader::Sprite, .. })));
}

#[test]
fn gxt_deferred_upload_is_lazy_immutable_and_skips_decode_and_alter() {
    for (format, payload) in [(0x8500_0000, 8), (0x8700_0000, 16)] {
        let (mut r, control) = setup();
        let canvas = r.create_target(2, 2, "canvas-test");
        let reads = Rc::new(Cell::new(0));
        let bytes = gxt_fixture([4, 4], format);
        let load = { let reads = reads.clone(); let bytes = bytes.clone();
            Rc::new(move || { reads.set(reads.get() + 1); Ok(bytes.clone()) }) };
        control.borrow_mut().events.clear();
        let tex = defer_test_gxt(&r, &bytes, load).unwrap();
        assert_eq!(tex.physical_size(), [4, 4]);
        assert_eq!(tex.uv_extent(), [8.0, 8.0]);
        assert_eq!(tex.uv_max(), [1.0, 1.0]);
        assert_eq!(tex.native.allocation_bytes().unwrap(), CDRAM_GRANULE);
        let mut discarded = r.encoder();
        r.try_draw(&mut discarded, &canvas, None, &[Quad::rect(tex.clone(), [0.0, 0.0, 2.0, 2.0], [1.0; 4])]).unwrap();
        drop(discarded);
        assert_eq!(reads.get(), 0);
        assert!(control.borrow().events.is_empty());
        for _ in 0..2 { submit_deferred(&mut r, &canvas, &[tex.clone()]).unwrap(); }
        assert_eq!(reads.get(), 1);
        assert_eq!(r.device.encoded_bytes.get(), 0);
        let trace = control.borrow();
        assert_eq!(trace.events.iter().filter(|e| matches!(e, Event::CreateCompressed {
            payload_bytes, srgb: true, linear: true, size: [4, 4], .. } if *payload_bytes == payload)).count(), 1);
        assert!(!trace.events.iter().any(|e| matches!(e, Event::Create { .. }
            | Event::Draw { shader: EffectShader::Alter, .. })));
        drop(trace);
        assert!(r.try_write_region(&tex, [0, 0, 1, 1], 4, &[0; 4]).is_err());
        let id = tex.native.resident_id().unwrap();
        assert!(r.device.call(|g| g.read_texture(&id)).is_err());
        assert!(r.device.call(|g| g.copy_texture(&id, &id)).is_err());
    }
}

#[test]
fn gxt_payload_charges_backing_blocks_and_cache_pins_then_reloads_after_eviction() {
    let (mut r, control) = setup();
    let canvas = r.create_target(2, 2, "canvas-test");
    // 513x257 rounds to 1024x512 DXT1: one 256 KiB allocation, versus
    // three for pitched RGBA. The file's zero alignment bytes are not charged.
    let bytes = gxt_fixture([513, 257], 0x8500_0000);
    let reads = Rc::new(Cell::new(0));
    let load = { let reads = reads.clone(); let bytes = bytes.clone();
        Rc::new(move || { reads.set(reads.get() + 1); Ok(bytes.clone()) }) };
    let a = defer_test_gxt(&r, &bytes, load).unwrap();
    let b = defer_test_gxt(&r, &bytes, { let b = bytes.clone(); Rc::new(move || Ok(b.clone())) }).unwrap();
    assert_eq!(a.native.allocation_bytes().unwrap(), CDRAM_GRANULE);
    r.device.deferred_budget.set(CDRAM_GRANULE);
    submit_deferred(&mut r, &canvas, &[a.clone()]).unwrap();
    control.borrow_mut().events.clear();
    assert!(submit_deferred(&mut r, &canvas, &[a.clone(), b.clone()]).is_err());
    assert!(a.native.resident_id().is_ok());
    assert!(control.borrow().events.is_empty());
    submit_deferred(&mut r, &canvas, &[b]).unwrap();
    assert!(a.native.resident_id().is_err());
    submit_deferred(&mut r, &canvas, &[a]).unwrap();
    assert_eq!(reads.get(), 2);
}

#[test]
fn compressed_and_rgba_preparation_share_headroom_without_an_unneeded_bake_peak() {
    let (mut r, control) = setup();
    let canvas = r.create_target(2, 2, "canvas-test");
    let bytes = gxt_fixture([960, 540], 0x8500_0000); // 2 native blocks
    let compressed = defer_test_gxt(&r, &bytes, { let b = bytes.clone(); Rc::new(move || Ok(b.clone())) }).unwrap();
    let rgba = r.defer_altered([2, 2], [2, 2], Arc::from([2, 90]), "rgba", decode_deferred_test).unwrap();
    let available = r.device.call(|g| g.available_cdram()).unwrap();
    control.borrow_mut().external_cdram_bytes = available - 3 * CDRAM_GRANULE;
    control.borrow_mut().events.clear();
    // RGBA's temporary source must retire before the larger compressed image
    // occupies its final storage. Submission still follows this draw order.
    submit_deferred(&mut r, &canvas, &[compressed.clone(), rgba.clone()]).unwrap();
    assert_eq!(r.device.call(|g| g.available_cdram()).unwrap(), 0);
    let trace = control.borrow();
    let actual: Vec<_> = trace.events.iter().filter_map(|e| match e {
        Event::Draw { shader: EffectShader::Sprite, textures, .. } => textures[0], _ => None,
    }).collect();
    assert_eq!(actual, [compressed.native.resident_id().unwrap().0, rgba.native.resident_id().unwrap().0]);
}

#[test]
fn malformed_or_changed_gxt_fails_before_eviction_or_upload_and_failed_create_can_retry() {
    let (mut r, control) = setup();
    let canvas = r.create_target(2, 2, "canvas-test");
    let idle = r.defer_altered([2, 2], [2, 2], Arc::from([2, 45]), "idle", decode_deferred_test).unwrap();
    submit_deferred(&mut r, &canvas, &[idle.clone()]).unwrap();
    r.device.deferred_budget.set(CDRAM_GRANULE);
    let bytes = gxt_fixture([4, 4], 0x8700_0000);
    let never: Rc<dyn Fn() -> Result<Vec<u8>>> = Rc::new(|| panic!("registration must not read"));
    let mut malformed = bytes.clone(); malformed[52..56].copy_from_slice(&0x8900_0000u32.to_le_bytes());
    assert!(defer_test_gxt(&r, &malformed, never.clone()).is_err());
    assert!(r.defer_gxt_altered_reload([8, 8], [3, 4], &bytes, "mismatch", never, extract_test_gxt).is_err());
    let mut changed_size = bytes.clone(); changed_size[56..58].copy_from_slice(&3u16.to_le_bytes());
    let mut changed_layout = bytes.clone(); changed_layout[48..52].copy_from_slice(&0u32.to_le_bytes());
    let mut changed_outer = bytes.clone(); changed_outer.push(0);
    let mut changed_format = bytes.clone(); changed_format[52..56].copy_from_slice(&0x8500_0000u32.to_le_bytes());
    changed_format[72..80].fill(0); // valid DXT1, same storage/file length
    let loaders: Vec<Rc<dyn Fn() -> Result<Vec<u8>>>> = [changed_size, changed_layout, changed_outer, changed_format]
        .into_iter().map(|data| Rc::new(move || Ok(data.clone())) as _).chain(
            [Rc::new(|| bail!("synthetic read failure")) as Rc<dyn Fn() -> Result<Vec<u8>>>]).collect();
    for load in loaders {
        let tex = defer_test_gxt(&r, &bytes, load).unwrap();
        control.borrow_mut().events.clear();
        assert!(submit_deferred(&mut r, &canvas, &[tex.clone()]).is_err());
        assert!(idle.native.resident_id().is_ok());
        assert!(tex.native.resident_id().is_err());
        assert!(control.borrow().events.is_empty());
    }
    let tex = defer_test_gxt(&r, &bytes, { let b = bytes.clone(); Rc::new(move || Ok(b.clone())) }).unwrap();
    control.borrow_mut().fail_next_compressed_create = true;
    assert!(submit_deferred(&mut r, &canvas, &[tex.clone()]).is_err());
    assert!(tex.native.resident_id().is_err());
    assert!(control.borrow().active.is_none());
    submit_deferred(&mut r, &canvas, &[tex.clone()]).unwrap();
    assert!(tex.native.resident_id().is_ok());
}

#[test]
fn deferred_menu_batch_fits_native_peak_without_changing_draw_order() {
    fn decode_rect(bytes: &[u8]) -> Result<(u32, u32, Vec<u8>, bool)> {
        ensure!(bytes.len() == 5, "invalid encoded rectangle");
        let width = u32::from(u16::from_le_bytes([bytes[0], bytes[1]]));
        let height = u32::from(u16::from_le_bytes([bytes[2], bytes[3]]));
        Ok((width, height, vec![bytes[4]; (width * height * 4) as usize], true))
    }
    let (mut r, control) = setup();
    let canvas = r.create_target(2, 2, "canvas-test");
    let make = |[w, h]: [u16; 2], value| {
        let [wl, wh] = w.to_le_bytes();
        let [hl, hh] = h.to_le_bytes();
        r.defer_altered([u32::from(w), u32::from(h)], [u32::from(w), u32::from(h)],
            Arc::from([wl, wh, hl, hh, value]), "menu-test", decode_rect).unwrap()
    };
    let pinned = make([2, 2], 10);
    let small_a = make([2, 2], 20);
    let small_b = make([3, 3], 30);
    let large = make([960, 440], 40);
    submit_deferred(&mut r, &canvas, &[pinned.clone()]).unwrap();
    let pinned_id = pinned.native.resident_id().unwrap().0;
    assert_eq!(large.native.allocation_bytes().unwrap(), 7 * CDRAM_GRANULE);
    let available = r.device.call(|g| g.available_cdram()).unwrap();
    {
        let mut trace = control.borrow_mut();
        // Match the menu case: all destinations fit, but small-first uploads
        // leave less than the large atlas's simultaneous source + target peak.
        trace.external_cdram_bytes = available - 14 * CDRAM_GRANULE;
        trace.events.clear();
    }
    let frame = [small_a.clone(), small_b.clone(), large.clone(), pinned.clone()];
    submit_deferred(&mut r, &canvas, &frame).unwrap();
    assert_eq!(pinned.native.resident_id().unwrap().0, pinned_id);
    let expected: Vec<_> = frame.iter().map(|t| t.native.resident_id().unwrap().0).collect();
    let trace = control.borrow();
    let drawn: Vec<_> = trace.events.iter().filter_map(|event| match event {
        Event::Draw { shader: EffectShader::Sprite, textures, .. } => textures[0],
        _ => None,
    }).collect();
    assert_eq!(drawn, expected, "baking may be scheduled independently of game draw order");
    assert!(!trace.events.iter().any(|event| matches!(event, Event::Destroy(id) if *id == pinned_id)));
}

#[test]
fn deferred_native_headroom_includes_bake_peak_and_protects_all_frame_textures() {
    let (mut r, control) = setup();
    let canvas = r.create_target(2, 2, "canvas-test");
    let make = |n| r.defer_altered([2, 2], [2, 2], Arc::from([2, n]), "lazy", decode_deferred_test).unwrap();
    let a = make(11);
    let b = make(22);
    let c = make(33);
    submit_deferred(&mut r, &canvas, &[a.clone()]).unwrap();
    submit_deferred(&mut r, &canvas, &[b.clone()]).unwrap();
    let old_a = a.native.resident_id().unwrap().0;
    let old_b = b.native.resident_id().unwrap().0;
    let used = 48 * 1024 * 1024 - r.device.call(|g| g.available_cdram()).unwrap();
    // Only one block remains. Final C fits, but its source + destination need
    // two. A is older than B but the complete next frame requires A.
    {
        let mut trace = control.borrow_mut();
        trace.cdram_limit = Some(used + CDRAM_GRANULE);
        trace.events.clear();
    }
    submit_deferred(&mut r, &canvas, &[a.clone(), c.clone()]).unwrap();
    assert_eq!(a.native.resident_id().unwrap().0, old_a);
    assert!(b.native.resident_id().is_err());
    assert!(c.native.resident_id().is_ok());
    let trace = control.borrow();
    let retired = trace.events.iter().position(|e| matches!(e, Event::Destroy(id) if *id == old_b)).unwrap();
    let allocation = trace.events.iter().position(|e| matches!(e, Event::Create { .. })).unwrap();
    assert!(retired < allocation, "native peak must be reserved before the source allocation");
}

#[test]
fn deferred_native_headroom_failure_has_no_partial_bake_or_game_submission() {
    let (mut r, control) = setup();
    let canvas = r.create_target(2, 2, "canvas-test");
    let a = r.defer_altered([2, 2], [2, 2], Arc::from([2, 12]), "pinned", decode_deferred_test).unwrap();
    let b = r.defer_altered([2, 2], [2, 2], Arc::from([2, 24]), "new", decode_deferred_test).unwrap();
    submit_deferred(&mut r, &canvas, &[a.clone()]).unwrap();
    let available = r.device.call(|g| g.available_cdram()).unwrap();
    {
        let mut trace = control.borrow_mut();
        // Other native owners consume all but one allocation. Renderer policy
        // itself still has room, and no deferred texture may be evicted.
        trace.external_cdram_bytes = available - CDRAM_GRANULE;
        trace.events.clear();
    }
    let error = submit_deferred(&mut r, &canvas, &[a.clone(), b.clone()]).unwrap_err();
    assert!(format!("{error:#}").contains("CDRAM"));
    assert!(a.native.resident_id().is_ok());
    assert!(b.native.resident_id().is_err());
    assert!(control.borrow().events.is_empty(), "insufficient bake peak must fail before GPU allocation or passes");
}

#[test]
fn ordinary_target_reclaims_idle_cache_using_rounded_native_allocation() {
    let (mut r, control) = setup();
    let canvas = r.create_target(2, 2, "canvas-test");
    let a = r.defer_altered([2, 2], [2, 2], Arc::from([2, 12]), "idle", decode_deferred_test).unwrap();
    submit_deferred(&mut r, &canvas, &[a.clone()]).unwrap();
    let available = r.device.call(|g| g.available_cdram()).unwrap();
    control.borrow_mut().external_cdram_bytes = available;
    control.borrow_mut().events.clear();
    let target = r.try_create_target(9, 2, "new-ordinary-target").unwrap();
    assert_eq!(target.tex.physical_size(), [9, 2]);
    assert!(a.native.resident_id().is_err());
    assert_eq!(r.device.call(|g| g.available_cdram()).unwrap(), 0);
    let trace = control.borrow();
    assert!(matches!(trace.events[0], Event::Destroy(_)));
    assert!(matches!(trace.events[1], Event::Create { size: [9, 2], .. }));
}

#[test]
fn ordinary_rgba_and_r8_account_for_pitch_before_memblock_rounding() {
    // Both raw layouts fit one 256 KiB block, but the native 8-pixel pitch
    // crosses that boundary. Exactly one idle atlas must be reclaimed.
    for (size, bpp) in [([257, 255], 4), ([1025, 255], 1)] {
        assert!((size[0] * size[1] * bpp) < CDRAM_GRANULE as u32);
        assert_eq!(native_allocation_bytes(size, bpp).unwrap(), 2 * CDRAM_GRANULE);
        let (mut r, control) = setup();
        let canvas = r.create_target(2, 2, "canvas-test");
        let a = r.defer_altered([2, 2], [2, 2], Arc::from([2, 12]), "idle", decode_deferred_test).unwrap();
        submit_deferred(&mut r, &canvas, &[a.clone()]).unwrap();
        let available = r.device.call(|g| g.available_cdram()).unwrap();
        control.borrow_mut().external_cdram_bytes = available - CDRAM_GRANULE;
        control.borrow_mut().events.clear();
        assert!(r.try_create_writable(size[0], size[1], bpp == 1, "pitch boundary").is_ok());
        assert!(a.native.resident_id().is_err());
        assert!(matches!(control.borrow().events[0], Event::Destroy(_)));
    }
}

#[test]
fn immediate_alter_checks_combined_peak_before_allocating_its_source() {
    let (r, control) = setup();
    let available = r.device.call(|g| g.available_cdram()).unwrap();
    // The real IMG2328 case: a 900x540 source and destination cost 2 MiB
    // each. Three MiB fits the source alone but can never finish the bake.
    control.borrow_mut().external_cdram_bytes = available - 3 * 1024 * 1024;
    let pixels = vec![120; 900 * 540 * 4];
    let error = r.try_upload_altered(900, 540, Upload::Rgba8(&pixels), true, "IMG2328").err().unwrap();
    assert!(format!("{error:#}").contains("CDRAM"));
    assert!(control.borrow().events.is_empty(), "an impossible source + target peak must allocate neither");
}

#[test]
fn immediate_alter_reclaims_before_starting_and_accounts_for_r8_source() {
    let (mut r, control) = setup();
    let canvas = r.create_target(2, 2, "canvas-test");
    let idle = r.defer_altered([2, 2], [2, 2], Arc::from([2, 12]), "idle", decode_deferred_test).unwrap();
    submit_deferred(&mut r, &canvas, &[idle.clone()]).unwrap();
    let available = r.device.call(|g| g.available_cdram()).unwrap();
    control.borrow_mut().external_cdram_bytes = available - 2 * CDRAM_GRANULE;
    control.borrow_mut().events.clear();
    // R8 source fits one block; pitched RGBA target needs two. Reclaim one
    // block first. Counting two RGBA allocations would reject a valid bake.
    let pixels = vec![128; 257 * 255];
    let texture = r.try_upload_altered(257, 255, Upload::R8(&pixels), true, "R8-peak").unwrap();
    assert_eq!(texture.physical_size(), [257, 255]);
    assert!(idle.native.resident_id().is_err());
    let trace = control.borrow();
    assert!(matches!(trace.events[0], Event::Destroy(_)), "retirement must precede source allocation");
    assert!(matches!(trace.events[1], Event::Create { format: TextureFormat::R8, .. }));
}

#[test]
fn invalid_immediate_alter_does_not_reclaim_idle_atlases() {
    let (mut r, control) = setup();
    let canvas = r.create_target(2, 2, "canvas-test");
    let idle = r.defer_altered([2, 2], [2, 2], Arc::from([2, 12]), "idle", decode_deferred_test).unwrap();
    submit_deferred(&mut r, &canvas, &[idle.clone()]).unwrap();
    control.borrow_mut().external_cdram_bytes = r.device.call(|g| g.available_cdram()).unwrap();
    control.borrow_mut().events.clear();
    assert!(r.try_upload_altered(2, 2, Upload::Rgba8(&[1]), true, "short").is_err());
    assert!(r.try_upload_altered(2, 2, Upload::Bc1(&[1; 8]), true, "not-native-gxt").is_err());
    assert!(idle.native.resident_id().is_ok());
    assert!(control.borrow().events.is_empty());
}

#[test]
fn invalid_ordinary_allocation_does_not_evict_idle_cache() {
    let (mut r, control) = setup();
    let canvas = r.create_target(2, 2, "canvas-test");
    let a = r.defer_altered([2, 2], [2, 2], Arc::from([2, 12]), "idle", decode_deferred_test).unwrap();
    submit_deferred(&mut r, &canvas, &[a.clone()]).unwrap();
    control.borrow_mut().external_cdram_bytes = r.device.call(|g| g.available_cdram()).unwrap();
    control.borrow_mut().events.clear();
    assert!(r.try_create_target(2049, 2, "invalid dimensions").is_err());
    assert!(a.native.resident_id().is_ok());
    assert!(control.borrow().events.is_empty());
}

#[test]
fn failed_pressure_retirement_poison_prevents_new_allocations() {
    let (mut r, control) = setup();
    let canvas = r.create_target(2, 2, "canvas-test");
    let a = r.defer_altered([2, 2], [2, 2], Arc::from([2, 12]), "idle", decode_deferred_test).unwrap();
    submit_deferred(&mut r, &canvas, &[a.clone()]).unwrap();
    let available = r.device.call(|g| g.available_cdram()).unwrap();
    {
        let mut trace = control.borrow_mut();
        trace.external_cdram_bytes = available;
        trace.fail_next_retirement = true;
        trace.events.clear();
    }
    let error = r.try_create_target(2, 2, "retirement-error").err().expect("retirement must fail");
    assert!(format!("{error:#}").contains("retirement"));
    assert!(r.device.retirement_error.borrow().is_some(), "failed fence/retirement must poison the owner");
    assert!(r.try_create_target(2, 2, "poisoned-owner").is_err());
    assert!(control.borrow().events.is_empty());
}

#[test]
fn scene_owned_cg_can_release_residency_without_releasing_its_logical_handle() {
    let (mut r, control) = setup();
    let canvas = r.create_target(2, 2, "canvas-test");
    r.device.deferred_budget.set(2 * CDRAM_GRANULE);
    let reads = Rc::new(Cell::new(0));
    let make = |value| {
        let first = r.try_upload_altered(2, 2, Upload::Rgba8(&[value; 16]), true, "CG").unwrap();
        let load = { let reads = reads.clone(); Rc::new(move || {
            reads.set(reads.get() + 1); Ok(vec![2, value])
        }) };
        r.adopt_image_reload(first, 2, "CG", true, load, Rc::new(decode_deferred_test)).unwrap()
    };
    let hidden = make(12);
    let visible = make(24);
    let hidden_layer = hidden.clone();
    let original = hidden.native.resident_id().unwrap().0;
    assert_eq!(reads.get(), 0, "adopting a worker-decoded image must not decode it twice");
    submit_deferred(&mut r, &canvas, &[visible.clone()]).unwrap();
    let overlay = r.defer_altered([2, 2], [2, 2], Arc::from([2, 36]), "overlay", decode_deferred_test).unwrap();
    submit_deferred(&mut r, &canvas, &[visible.clone(), overlay.clone()]).unwrap();
    assert!(hidden.native.id.borrow().is_none(), "hidden stage ownership must not pin CDRAM");
    assert!(Arc::ptr_eq(&hidden, &hidden_layer));
    assert!(control.borrow().events.iter().any(|e| matches!(e, Event::Destroy(id) if *id == original)));
    submit_deferred(&mut r, &canvas, &[hidden_layer.clone(), visible.clone()]).unwrap();
    assert_eq!(reads.get(), 1);
    let scans = r.device.admission_scans.get();
    control.borrow_mut().events.clear();
    for _ in 0..120 {
        submit_deferred(&mut r, &canvas, &[hidden_layer.clone(), visible.clone()]).unwrap();
    }
    assert_eq!(reads.get(), 1, "resident scene must not repeatedly read/decode CGs");
    assert_eq!(r.device.admission_scans.get(), scans, "no per-frame LRU victim scans");
    assert!(!control.borrow().events.iter().any(|e| matches!(e, Event::Create { .. } | Event::Destroy(_))));
    drop((hidden, hidden_layer, visible, overlay));
    assert_eq!(r.device.resident_deferred_bytes.get(), 0);
}

#[test]
fn adopted_raw_and_cpu_baked_cg_reload_without_an_alter_pass() {
    for srgb in [false, true] {
        let (mut r, control) = setup();
        let canvas = r.create_target(2, 2, "canvas-test");
        r.device.deferred_budget.set(CDRAM_GRANULE);
        let pixels = [41u8; 16];
        let first = if srgb { r.try_upload_baked(2, 2, &pixels, "baked").unwrap() }
            else { r.try_upload(2, 2, Upload::Rgba8(&pixels), "raw").unwrap() };
        let decode = Rc::new(move |_: &[u8]| Ok((2, 2, pixels.to_vec(), srgb)));
        let image = r.adopt_image_reload(first, 1, "graded-or-raw", false,
            Rc::new(|| Ok(vec![1])), decode).unwrap();
        let other = r.defer_altered([2, 2], [2, 2], Arc::from([2, 12]), "other", decode_deferred_test).unwrap();
        submit_deferred(&mut r, &canvas, &[other]).unwrap();
        assert!(image.native.id.borrow().is_none());
        control.borrow_mut().events.clear();
        submit_deferred(&mut r, &canvas, &[image.clone()]).unwrap();
        assert!(control.borrow().events.iter().any(|e| matches!(e,
            Event::Create { srgb: actual, target: false, .. } if *actual == srgb)));
        assert!(!control.borrow().events.iter().any(|e| matches!(e,
            Event::Draw { shader: EffectShader::Alter, .. })));
        assert_eq!(r.device.call(|g| g.read_texture(&image.native.resident_id().unwrap())).unwrap(), pixels);
    }
}

#[test]
fn deferred_reload_is_lazy_and_rereads_only_after_eviction() {
    let (mut r, control) = setup();
    let canvas = r.create_target(2, 2, "canvas-test");
    r.device.deferred_budget.set(CDRAM_GRANULE);
    let reads = Rc::new(Cell::new(0));
    let load = {
        let reads = reads.clone();
        Rc::new(move || { reads.set(reads.get() + 1); Ok(vec![2, 77]) })
    };
    let reload = r.defer_altered_reload([2, 2], [2, 2], 2, "reload", load, decode_deferred_test).unwrap();
    let other = r.defer_altered([2, 2], [2, 2], Arc::from([2, 24]), "other", decode_deferred_test).unwrap();
    assert_eq!(reads.get(), 0);
    assert_eq!(r.device.encoded_bytes.get(), 2, "only retained encoded bytes count permanently");
    control.borrow_mut().events.clear();
    let mut discarded = r.encoder();
    r.try_draw(&mut discarded, &canvas, None, &[Quad::rect(reload.clone(), [0.0, 0.0, 2.0, 2.0], [1.0; 4])]).unwrap();
    drop(discarded);
    assert_eq!(reads.get(), 0);
    assert!(control.borrow().events.is_empty());
    submit_deferred(&mut r, &canvas, &[reload.clone()]).unwrap();
    submit_deferred(&mut r, &canvas, &[reload.clone()]).unwrap();
    assert_eq!(reads.get(), 1);
    submit_deferred(&mut r, &canvas, &[other]).unwrap();
    assert!(reload.native.resident_id().is_err());
    submit_deferred(&mut r, &canvas, &[reload.clone()]).unwrap();
    assert_eq!(reads.get(), 2);
    assert_eq!(r.device.encoded_bytes.get(), 0);
    drop(reload);
    assert_eq!(r.device.encoded_bytes.get(), 0);
}

#[test]
fn deferred_reload_length_loader_and_decode_errors_do_not_reach_gpu() {
    let (mut r, control) = setup();
    let canvas = r.create_target(2, 2, "canvas-test");
    let loaders: [Rc<dyn Fn() -> Result<Vec<u8>>>; 4] = [
        Rc::new(|| Ok(vec![2])),
        Rc::new(|| Ok(vec![2, 12, 99])),
        Rc::new(|| bail!("synthetic archive short read")),
        Rc::new(|| Ok(vec![0, 12])),
    ];
    for loader in loaders {
        let texture = r.defer_altered_reload([2, 2], [2, 2], 2, "bad reload", loader, decode_deferred_test).unwrap();
        control.borrow_mut().events.clear();
        assert!(submit_deferred(&mut r, &canvas, &[texture.clone()]).is_err());
        assert!(texture.native.resident_id().is_err());
        assert!(control.borrow().events.is_empty());
        assert_eq!(r.device.encoded_bytes.get(), 0);
    }
    let never: Rc<dyn Fn() -> Result<Vec<u8>>> = Rc::new(|| panic!("invalid descriptor must not load"));
    assert!(r.defer_altered_reload([2, 2], [2, 2], 0, "empty", never.clone(), decode_deferred_test).is_err());
    assert!(r.defer_altered_reload([2, 2], [2, 2], DEFERRED_SOURCE_BYTES + 1, "large", never, decode_deferred_test).is_err());
}

#[test]
fn deferred_images_allocate_only_when_submitted_and_remain_reusable() {
    let (mut r, control) = setup();
    let canvas = r.create_target(2, 2, "canvas-test");
    control.borrow_mut().events.clear();
    let texture = r.defer_altered([4, 4], [2, 2], Arc::from([2, 73]), "lazy", decode_deferred_test).unwrap();
    assert_eq!(texture.physical_size(), [2, 2]);
    assert_eq!(texture.uv_extent(), [4.0, 4.0]);
    assert!(control.borrow().events.is_empty());
    let quad = Quad::rect(texture.clone(), [0.0, 0.0, 2.0, 2.0], [1.0; 4]);
    let mut discarded = r.encoder();
    r.try_draw(&mut discarded, &canvas, None, &[quad.clone()]).unwrap();
    drop(discarded);
    assert!(control.borrow().events.is_empty());
    for _ in 0..2 {
        let mut encoder = r.encoder();
        r.try_draw(&mut encoder, &canvas, None, &[quad.clone()]).unwrap();
        r.try_present(encoder, &canvas).unwrap();
    }
    let events = &control.borrow().events;
    assert_eq!(events.iter().filter(|e| matches!(e, Event::Create { .. })).count(), 2);
    assert_eq!(events.iter().filter(|e| matches!(e, Event::Draw { shader: EffectShader::Alter, .. })).count(), 1);
}

#[test]
fn deferred_cache_evicts_inactive_resources_but_pins_the_whole_frame() {
    let (mut r, control) = setup();
    // Each tiny native allocation occupies a real 256 KiB CDRAM block.
    r.device.deferred_budget.set(2 * 256 * 1024);
    let canvas = r.create_target(2, 2, "canvas-test");
    let make = |n| r.defer_altered([2, 2], [2, 2], Arc::from([2, n]), "lazy", decode_deferred_test).unwrap();
    let a = make(11);
    let b = make(22);
    let c = make(33);
    let draw = |r: &mut Renderer, textures: &[TexRef]| {
        let quads = textures.iter().map(|t| Quad::rect(t.clone(), [0.0, 0.0, 2.0, 2.0], [1.0; 4])).collect::<Vec<_>>();
        let mut encoder = r.encoder();
        r.try_draw(&mut encoder, &canvas, None, &quads).unwrap();
        r.try_present(encoder, &canvas)
    };
    draw(&mut r, &[a.clone(), b.clone()]).unwrap();
    let first_a = a.native.resident_id().unwrap().0;
    draw(&mut r, &[b.clone(), c.clone()]).unwrap();
    assert!(a.native.resident_id().is_err());
    assert!(b.native.resident_id().is_ok());
    assert!(c.native.resident_id().is_ok());
    assert!(control.borrow().events.iter().any(|e| matches!(e, Event::Destroy(id) if *id == first_a)));
    draw(&mut r, &[a.clone()]).unwrap();
    assert_ne!(a.native.resident_id().unwrap().0, first_a);
    control.borrow_mut().events.clear();
    assert!(draw(&mut r, &[a.clone(), b.clone(), c.clone()]).unwrap_err().to_string().contains("deferred texture budget"));
    assert!(control.borrow().events.is_empty(), "over-budget frame must fail before any GPU commands");
}

#[test]
fn deferred_decode_and_bake_failures_roll_back_before_game_render_pass() {
    let (mut r, control) = setup();
    let canvas = r.create_target(2, 2, "canvas-test");
    let texture = r.defer_altered([2, 2], [2, 2], Arc::from([2, 12]), "lazy", decode_deferred_test).unwrap();
    let quad = Quad::rect(texture.clone(), [0.0, 0.0, 2.0, 2.0], [1.0; 4]);
    let mut encoder = r.encoder();
    r.try_draw(&mut encoder, &canvas, None, &[quad.clone()]).unwrap();
    control.borrow_mut().fail_next_draw = true;
    assert!(r.try_present(encoder, &canvas).is_err());
    assert!(texture.native.resident_id().is_err());
    assert!(control.borrow().active.is_none());
    control.borrow_mut().events.clear();
    let mut encoder = r.encoder();
    r.try_draw(&mut encoder, &canvas, None, &[quad]).unwrap();
    r.try_present(encoder, &canvas).unwrap();
    assert!(texture.native.resident_id().is_ok());
    let invalid = r.defer_altered([2, 2], [2, 2], Arc::from([0, 12]), "bad-image", decode_deferred_test).unwrap();
    let mut encoder = r.encoder();
    r.try_draw(&mut encoder, &canvas, None, &[Quad::rect(invalid, [0.0, 0.0, 2.0, 2.0], [1.0; 4])]).unwrap();
    control.borrow_mut().events.clear();
    assert!(r.try_present(encoder, &canvas).is_err());
    assert!(control.borrow().events.is_empty());
}

#[test]
fn deferred_source_budget_ownership_and_immutability_are_enforced() {
    let (r, control) = setup();
    let texture = r.defer_altered([2, 2], [2, 2], Arc::from([2, 12]), "lazy", decode_deferred_test).unwrap();
    assert!(r.owns_texture(&texture));
    let (other, _) = setup();
    assert!(!other.owns_texture(&texture));
    assert!(other.try_write_region(&texture, [0, 0, 1, 1], 4, &[0; 4]).is_err());
    assert!(r.try_write_region(&texture, [0, 0, 1, 1], 4, &[0; 4]).is_err());
    assert_eq!(r.device.encoded_bytes.get(), 2);
    r.device.encoded_bytes.set(DEFERRED_SOURCE_BYTES);
    assert!(r.defer_altered([2, 2], [2, 2], Arc::from([2, 12]), "over-budget", decode_deferred_test).is_err());
    r.device.encoded_bytes.set(2);
    drop(texture);
    assert_eq!(r.device.encoded_bytes.get(), 0);
    assert!(control.borrow().events.is_empty());
}

fn commands(control: &Control) -> Vec<&'static str> {
    control
        .borrow()
        .events
        .iter()
        .filter_map(|event| {
            Some(match event {
                Event::Begin { .. } => "begin",
                Event::End => "end",
                Event::Draw {
                    shader: EffectShader::Ps,
                    ..
                } => "ps",
                Event::Draw { .. } => "draw",
                Event::Copy { .. } => "copy",
                Event::Present(_) => "present",
                _ => return None,
            })
        })
        .collect()
}

#[test]
fn recording_is_deferred_and_each_quad_keeps_flat_uniforms() {
    let (mut r, control) = setup();
    let target = r.create_scene_target(CANVAS_W, CANVAS_H, "test");
    let mut a = Quad::rect(r.white.clone(), [0.0, 0.0, 50.0, 50.0], [1.0; 4]);
    a.haze = [[0.1, 2.0, 0.25, 1.0], [0.2, 3.0, 0.5, 0.0]];
    let mut b = a.clone();
    b.haze[0][0] = 0.9;
    let mut enc = r.encoder();
    r.try_draw(&mut enc, &target, Some([0.0; 4]), &[a.clone(), b.clone()])
        .unwrap();
    assert!(commands(&control).is_empty());
    r.try_present(enc, &target).unwrap();
    // A haze change is a shader uniform change, so these two quads must stay separate.
    assert_eq!(
        commands(&control),
        ["begin", "draw", "draw", "end", "present"]
    );
    let log = control.borrow();
    let draws: Vec<_> = log
        .events
        .iter()
        .filter_map(|e| {
            if let Event::Draw { vertices, .. } = e {
                Some(vertices)
            } else {
                None
            }
        })
        .collect();
    assert_eq!(draws.len(), 2);
    assert!(draws[0].iter().all(|v| v.haze == a.haze));
    assert!(draws[1].iter().all(|v| v.haze == b.haze));
}

#[test]
fn ps_snapshots_are_between_passes_and_follow_each_preceding_draw() {
    let (mut r, control) = setup();
    let target = r.create_scene_target(CANVAS_W, CANVAS_H, "test");
    let ordinary = Quad::rect(r.white.clone(), [0.0, 0.0, 10.0, 10.0], [1.0; 4]);
    let mut ps = ordinary.clone();
    ps.blend = Blend::Ps(PsOp::Overlay);
    ps.shade = Shade::NoAlpha;
    let mut mask = RuleMask::new(r.white.clone(), 0.375);
    mask.flip = 3;
    mask.invert = true;
    mask.soft = 128.0;
    ps.rule = Some(mask);
    let mut enc = r.encoder();
    r.try_draw(
        &mut enc,
        &target,
        Some([0.0; 4]),
        &[ordinary.clone(), ps.clone(), ps, ordinary],
    )
    .unwrap();
    r.try_present(enc, &target).unwrap();
    assert_eq!(
        commands(&control),
        [
            // One pass spans consecutive draws into the same target, so the trailing
            // ordinary quad joins the pass the second PS draw reopened.
            "begin", "draw", "end", "copy", "begin", "ps", "end", "copy", "begin", "ps", "draw",
            "end", "present"
        ]
    );
    let log = control.borrow();
    let copies: Vec<_> = log
        .events
        .iter()
        .filter_map(|e| {
            if let Event::Copy { src, dst } = e {
                Some((*src, *dst))
            } else {
                None
            }
        })
        .collect();
    assert_eq!(copies.len(), 2);
    assert_eq!(copies[0], copies[1]);
    for e in &log.events {
        if let Event::Draw {
            shader: EffectShader::Ps,
            blend,
            params,
            textures,
            ..
        } = e
        {
            assert_eq!(*blend, EffectBlend::Opaque);
            assert_eq!(
                *params,
                [
                    PsOp::Overlay as u32 as f32,
                    0.0,
                    RENDER_W as f32,
                    RENDER_H as f32,
                    0.375,
                    1031.0,
                    1.0,
                    0.0
                ]
            );
            assert_eq!(textures[1], Some(copies[0].1));
        }
    }
}

#[test]
fn all_ps_operations_preserve_their_numeric_contract() {
    let ops = [
        PsOp::Add,
        PsOp::Darken,
        PsOp::Lighten,
        PsOp::LinearBurn,
        PsOp::Burn,
        PsOp::Dodge,
        PsOp::DodgeAlpha,
        PsOp::Difference,
        PsOp::DifferenceAlpha,
        PsOp::Exclusion,
        PsOp::HardLight,
        PsOp::HardLightEx,
        PsOp::Overlay,
        PsOp::SoftLight,
        PsOp::Sub,
        PsOp::AddNoAlpha,
        PsOp::SubNoAlpha,
        PsOp::NoAlpha,
        PsOp::Normal,
        PsOp::DarkenColorAlpha,
        PsOp::LightenColorAlpha,
        PsOp::ScreenColorAlpha,
        PsOp::MultiplyDst,
        PsOp::Copy,
    ];
    let (mut r, control) = setup();
    let target = r.create_target(2, 2, "test");
    let quads: Vec<_> = ops
        .into_iter()
        .map(|op| {
            let mut q = Quad::rect(r.white.clone(), [0.0, 0.0, 2.0, 2.0], [1.0; 4]);
            q.blend = Blend::Ps(op);
            q
        })
        .collect();
    let mut enc = r.encoder();
    r.try_draw(&mut enc, &target, None, &quads).unwrap();
    r.try_present(enc, &target).unwrap();
    let log = control.borrow();
    let values: Vec<_> = log
        .events
        .iter()
        .filter_map(|e| {
            if let Event::Draw {
                shader: EffectShader::Ps,
                params,
                ..
            } = e
            {
                Some(params[0] as u32)
            } else {
                None
            }
        })
        .collect();
    assert_eq!(values, (1..=24).collect::<Vec<_>>());
}

#[test]
fn clipping_preserves_uvs_for_either_polygon_winding() {
    let (r, _) = setup();
    let mut q = Quad::rect(r.white.clone(), [0.0, 0.0, 100.0, 100.0], [1.0; 4]);
    let clip = [[25.0, 0.0], [75.0, 0.0], [75.0, 100.0], [25.0, 100.0]];
    for polygon in [clip, [clip[3], clip[2], clip[1], clip[0]]] {
        q.clip = Some(polygon);
        let vertices = quad_vertices(&q, [100.0, 100.0], [1.0; 2]).unwrap();
        assert_eq!(vertices.len(), 6);
        assert!(vertices.iter().all(|v| (-0.5..=0.5).contains(&v.pos[0])));
        for v in vertices {
            assert!((v.uv[0] - (v.pos[0] + 1.0) * 0.5).abs() < 1e-6);
        }
    }
}

#[test]
fn logical_density_soft_clip_and_texture_coordinates_are_independent() {
    let (r, _) = setup();
    let image = r.upload(65, 33, Upload::Rgba8(&vec![0; 65 * 33 * 4]), "image");
    let image = GpuTexture::with_logical_size(image, [130, 66]);
    assert_eq!(image.physical_size(), [65, 33]);
    assert_eq!(image.uv_max(), [1.0; 2]);
    assert_eq!(image.uv_extent(), [130.0, 66.0]);
    assert_eq!(scene_size(481, 271), [481u32.div_ceil(CANVAS_W / RENDER_W),
        271u32.div_ceil(CANVAS_H / RENDER_H)]);
    let mut q = Quad::rect(image, [0.0, 0.0, 1920.0, 1080.0], [1.0; 4]);
    q.soft = Some(SoftClip {
        inner: [-80.0, -40.0, 80.0, 40.0],
        pivot: [960.0, 540.0],
        rot: 0.0,
        radius: 20.0,
        row: 2,
        texels: 65,
        fill: 0.75,
    });
    let vertices = quad_vertices(&q, [1920.0, 1080.0], [0.5; 2]).unwrap();
    assert_eq!(
        vertices[0].soft,
        [
            [-40.0, -20.0, 40.0, 20.0],
            [480.0, 270.0, 0.0, 1.0],
            [10.0, 2.0, 65.0, 0.25]
        ]
    );
    assert_eq!(vertices[0].pos, [-1.0, 1.0]);
    assert_eq!(vertices[2].pos, [1.0, -1.0]);
}

#[test]
fn scene_upscale_then_native_ui_keeps_capture_size_and_rejects_aliases() {
    let (mut r, control) = setup();
    // A half-logical-size scene target is 480x270 physically, so the scaled copy
    // reject path still has a size mismatch to exercise.
    let scene = r.create_scene_target(CANVAS_W / 2, CANVAS_H / 2, "half scene");
    let baseline = r.device.call(|g| g.available_cdram()).unwrap();
    let canvas = r.create_composite_target(CANVAS_W, CANVAS_H, "canvas");
    assert_eq!(canvas.tex.physical_size(), [960, 540]);
    assert_eq!(canvas.tex.uv_extent(), [1920.0, 1080.0]);
    assert_eq!(baseline - r.device.call(|g| g.available_cdram()).unwrap(), 2 * 1024 * 1024);
    let freeze = r.create_composite_target(CANVAS_W, CANVAS_H, "screen capture");
    let mut enc = r.encoder();
    assert!(r.try_copy_target(&mut enc, &scene, &canvas).is_err(), "scaled copies must be explicit draws");
    assert!(r.try_blit_target(&mut enc, &canvas, &canvas).is_err());
    assert!(enc.commands.is_empty(), "rejected operations do not append partial passes");
    r.try_blit_target(&mut enc, &scene, &canvas).unwrap();
    let glyph = Quad { shade: Shade::Text, ..Quad::rect(r.white.clone(), [20.0, 40.0, 40.0, 40.0], [1.0; 4]) };
    r.try_draw(&mut enc, &canvas, None, &[glyph]).unwrap();
    r.try_copy_target(&mut enc, &canvas, &freeze).unwrap();
    r.try_present(enc, &canvas).unwrap();
    assert_eq!(r.try_read_target(&freeze).unwrap().len(), 960 * 540 * 4);
    let trace = control.borrow();
    let draws: Vec<_> = trace.events.iter().filter_map(|e| match e {
        Event::Draw { shader, blend, vertices, .. } => Some((shader, blend, vertices)), _ => None,
    }).collect();
    assert_eq!(*draws[0].0, EffectShader::Sprite);
    assert_eq!(*draws[0].1, EffectBlend::Opaque);
    assert_eq!(draws[0].2[0].pos, [-1.0, 1.0]);
    assert_eq!(draws[0].2[2].pos, [1.0, -1.0]);
    assert_eq!(*draws[1].0, EffectShader::Text);
    assert!(trace.events.iter().any(|e| matches!(e, Event::Copy { .. })));
}

#[test]
fn vita_fullscreen_is_native_540p_with_original_script_coordinates() {
    let (mut r, control) = setup();
    let target = r.create_scene_target(CANVAS_W, CANVAS_H, "native scene");
    #[cfg(feature = "assets-540p")]
    assert_eq!([RENDER_W, RENDER_H], [960, 540]);
    #[cfg(not(feature = "assets-540p"))]
    assert_eq!([RENDER_W, RENDER_H], [480, 270]);
    assert_eq!(target.tex.uv_extent(), [1920.0, 1080.0]);
    assert_eq!(target.tex.physical_size(), [RENDER_W, RENDER_H]);
    assert_eq!(target.tex.uv_max(), [1.0, 1.0]);
    assert!(control.borrow().events.iter().any(|e| matches!(e,
        Event::Create { size: [RENDER_W, RENDER_H], target: true, .. })));
    let enc = r.encoder();
    r.try_present(enc, &target).unwrap();
    assert!(control.borrow().events.iter().any(|e| matches!(e, Event::Present(_))));
}

#[test]
fn empty_draw_still_clears_and_hidden_frame_submits_without_scanout() {
    let (mut r, control) = setup();
    let target = r.create_target(3, 3, "test");
    r.hidden = true;
    let mut enc = r.encoder();
    r.try_draw(&mut enc, &target, Some([0.1, 0.2, 0.3, 0.4]), &[])
        .unwrap();
    r.try_present(enc, &target).unwrap();
    assert_eq!(commands(&control), ["begin", "end"]);
    assert!(control.borrow().events.iter().any(|e| matches!(
        e,
        Event::Begin {
            clear: Some([0.1, 0.2, 0.3, 0.4]),
            ..
        }
    )));
}

#[test]
fn source_drop_and_encoder_drop_have_bounded_real_ownership() {
    let (mut r, control) = setup();
    let target = r.create_target(2, 2, "test");
    let source = r.upload(1, 1, Upload::Rgba8(&[1, 2, 3, 4]), "source");
    let id = source.native.resident_id().unwrap().0;
    let mut enc = r.encoder();
    r.try_transition(
        &mut enc,
        &target,
        &source,
        &r.white.clone(),
        None,
        0.5,
        128.0,
    )
    .unwrap();
    drop(source);
    assert!(
        !control
            .borrow()
            .events
            .iter()
            .any(|e| matches!(e, Event::Destroy(v) if *v == id))
    );
    r.try_present(enc, &target).unwrap();
    let log = control.borrow();
    let retired = log
        .events
        .iter()
        .position(|e| matches!(e, Event::Destroy(v) if *v == id))
        .unwrap();
    let presented = log
        .events
        .iter()
        .position(|e| matches!(e, Event::Present(_)))
        .unwrap();
    assert!(retired > presented);
    drop(log);
    let mut discarded = r.encoder();
    let q = Quad::rect(r.white.clone(), [0.0, 0.0, 1.0, 1.0], [1.0; 4]);
    r.try_draw(&mut discarded, &target, None, &[q]).unwrap();
    let before = commands(&control);
    drop(discarded);
    assert_eq!(commands(&control), before);
    let last_encoder = r.encoder();
    drop(target);
    drop(r);
    assert!(
        !control
            .borrow()
            .events
            .iter()
            .any(|e| matches!(e, Event::Close))
    );
    drop(last_encoder);
    assert!(matches!(control.borrow().events.last(), Some(Event::Close)));
}

#[test]
fn native_draw_failure_closes_pass_and_propagates_original_error() {
    let (mut r, control) = setup();
    let target = r.create_target(2, 2, "test");
    let mut enc = r.encoder();
    r.try_draw(
        &mut enc,
        &target,
        Some([0.0; 4]),
        &[Quad::rect(r.white.clone(), [0.0, 0.0, 2.0, 2.0], [1.0; 4])],
    )
    .unwrap();
    control.borrow_mut().fail_next_draw = true;
    let err = r.try_present(enc, &target).unwrap_err().to_string();
    assert!(err.contains("injected native draw failure"));
    assert_eq!(control.borrow().active, None);
    assert_eq!(commands(&control), ["begin", "end"]);
}

#[test]
fn unsupported_blocks_truncated_data_regions_and_foreign_resources_are_errors() {
    let (mut r, _) = setup();
    for data in [
        Upload::Bc1(&[0; 8]),
        Upload::Bc3(&[0; 16]),
        Upload::Bc7(&[0; 16]),
    ] {
        assert!(
            r.try_upload(4, 4, data, "bad")
                .err()
                .unwrap()
                .chain()
                .any(|e| e.to_string().contains("unsupported"))
        );
    }
    assert!(
        r.try_upload(2, 2, Upload::Rgba8(&[0; 15]), "short")
            .is_err()
    );
    assert!(r.try_create_target(0, 2, "zero").is_err());
    let writable = r.create_writable(4, 4, true, "glyph");
    assert!(
        r.try_write_region(&writable, [3, 0, 2, 1], 1, &[0; 2])
            .is_err()
    );
    assert!(
        r.try_write_region(&writable, [0, 0, 1, 1], 4, &[0; 4])
            .is_err()
    );
    let (other, _) = setup();
    let target = r.create_target(2, 2, "target");
    let mut encoder = other.encoder();
    assert!(r.try_draw(&mut encoder, &target, None, &[]).is_err());
    let mut encoder = r.encoder();
    assert!(
        r.try_draw(
            &mut encoder,
            &target,
            None,
            &[Quad::rect(
                other.white.clone(),
                [0.0, 0.0, 1.0, 1.0],
                [1.0; 4]
            )]
        )
        .is_err()
    );
    assert!(encoder.commands.is_empty());
}

#[test]
fn r8_atlas_and_exact_targets_are_not_downscaled_scene_targets_are() {
    let (r, control) = setup();
    let atlas = r.create_writable(2048, 2048, true, "glyphs");
    assert_eq!(atlas.physical_size(), [2048, 2048]);
    let exact = r.create_target(1920, 1080, "choice glow");
    assert_eq!(exact.tex.physical_size(), [1920, 1080]);
    let scene = r.create_scene_target(1920, 1080, "canvas");
    assert_eq!([scene.tex.width, scene.tex.height], [1920, 1080]);
    assert_eq!(scene.tex.physical_size(), [RENDER_W, RENDER_H]);
    r.try_write_region(&atlas, [20, 31, 2, 2], 1, &[1, 2, 3, 4])
        .unwrap();
    assert!(control.borrow().events.iter().any(|e| matches!(
        e,
        Event::Update {
            origin: [20, 31],
            size: [2, 2],
            stride: 2,
            ..
        }
    )));
}

#[test]
fn transition_blur_rule_and_alter_keep_bindings_color_space_and_density() {
    let (mut r, control) = setup();
    let a = r.create_scene_target(1920, 1080, "a");
    let b = r.create_scene_target(1920, 1080, "b");
    let mut enc = r.encoder();
    r.try_transition(&mut enc, &b, &a.tex, &r.white.clone(), None, 0.4, 72.0)
        .unwrap();
    r.try_blur(&mut enc, &b.tex, &a, [2.0, -2.0], 3.5, BlurMode::White)
        .unwrap();
    let mut q = Quad::rect(r.white.clone(), [0.0, 0.0, 1.0, 1.0], [1.0; 4]);
    q.rule = Some(RuleMask::new(r.white.clone(), 0.3));
    q.shade = Shade::Text;
    r.try_draw(&mut enc, &b, None, &[q]).unwrap();
    r.try_present(enc, &b).unwrap();
    let log = control.borrow();
    let mut draws = log.events.iter().filter_map(|e| {
        if let Event::Draw {
            shader,
            params,
            textures,
            ..
        } = e
        {
            Some((*shader, *params, *textures))
        } else {
            None
        }
    });
    let trans = draws.next().unwrap();
    assert_eq!(trans.0, EffectShader::Transition);
    assert_eq!(trans.1, [0.4, 576.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0]);
    let blur = draws.next().unwrap();
    assert_eq!(blur.0, EffectShader::Blur);
    // The blur step is one destination pixel expressed in the 960-wide reference.
    let step = RENDER_W as f32 / 960.0;
    assert_eq!(blur.1, [step, -step, 3.5, 11.0, 2.0, 0.0, 0.0, 0.0]);
    let rule = draws.next().unwrap();
    assert_eq!(rule.0, EffectShader::Rule);
    assert_eq!(rule.1, [0.3, 1024.0, RENDER_W as f32, RENDER_H as f32, 0.0, 0.0, 0.0, 0.0]);
    assert!(trans.2[3].is_some() && blur.2[3].is_some() && rule.2[3].is_some());
    drop(log);
    control.borrow_mut().events.clear();
    let altered = r
        .try_upload_altered(1, 1, Upload::Rgba8(&[10, 20, 30, 255]), true, "alter")
        .unwrap();
    assert!(altered.native.srgb && altered.native.render_target);
    assert_eq!(commands(&control), ["begin", "draw", "end"]);
    assert!(control.borrow().events.iter().any(|e| matches!(
        e,
        Event::Draw {
            shader: EffectShader::Alter,
            blend: EffectBlend::Opaque,
            ..
        }
    )));
    assert!(control.borrow().events.iter().any(|e| matches!(
        e,
        Event::Create {
            srgb: true,
            target: false,
            ..
        }
    )));
}

#[test]
fn soft_lut_zero_fills_truncated_rows_and_preserves_nearest_format() {
    let (r, control) = setup();
    r.try_set_soft_lut(&[vec![7, 8], vec![9; SOFT_LUT_W as usize + 5]])
        .unwrap();
    let data = r.device.call(|g| g.read_texture(&r.soft_lut.resident_id().unwrap())).unwrap();
    assert_eq!(&data[..4], &[7, 8, 0, 0]);
    assert!(
        data[SOFT_LUT_W as usize..2 * SOFT_LUT_W as usize]
            .iter()
            .all(|v| *v == 9)
    );
    assert!(data[2 * SOFT_LUT_W as usize..].iter().all(|v| *v == 0));
    assert!(control.borrow().events.iter().any(|e| matches!(
        e,
        Event::Update {
            size: [SOFT_LUT_W, SOFT_LUT_ROWS],
            stride: SOFT_LUT_W,
            ..
        }
    )));
}

#[test]
fn every_simple_shader_and_blend_maps_to_native_variant() {
    let (mut r, control) = setup();
    let target = r.create_target(1, 1, "test");
    let shades = [
        Shade::Sprite,
        Shade::NoAlpha,
        Shade::ForceAlpha,
        Shade::Fill,
        Shade::Text,
        Shade::RimText,
        Shade::Mono,
        Shade::Nega,
    ];
    let expected = [
        EffectShader::Sprite,
        EffectShader::NoAlpha,
        EffectShader::ForceAlpha,
        EffectShader::Fill,
        EffectShader::Text,
        EffectShader::RimText,
        EffectShader::Mono,
        EffectShader::Nega,
    ];
    let blends = [
        Blend::Alpha,
        Blend::Add,
        Blend::Sub,
        Blend::Multiply,
        Blend::Screen,
        Blend::Opaque,
        Blend::Over,
        Blend::Footprint,
        Blend::MaskAlpha,
    ];
    let expected_blends = [
        EffectBlend::Alpha,
        EffectBlend::Add,
        EffectBlend::Sub,
        EffectBlend::Multiply,
        EffectBlend::Screen,
        EffectBlend::Opaque,
        EffectBlend::Over,
        EffectBlend::Footprint,
        EffectBlend::MaskAlpha,
    ];
    let quads: Vec<_> = shades
        .into_iter()
        .map(|shade| {
            let mut q = Quad::rect(r.white.clone(), [0.0, 0.0, 1.0, 1.0], [1.0; 4]);
            q.shade = shade;
            q
        })
        .collect();
    let mut enc = r.encoder();
    r.try_draw(&mut enc, &target, None, &quads).unwrap();
    for blend in blends {
        let mut q = quads[0].clone();
        q.blend = blend;
        r.try_draw(&mut enc, &target, None, &[q]).unwrap();
    }
    r.try_present(enc, &target).unwrap();
    let log = control.borrow();
    let actual: Vec<_> = log
        .events
        .iter()
        .filter_map(|e| {
            if let Event::Draw { shader, blend, .. } = e {
                Some((*shader, *blend))
            } else {
                None
            }
        })
        .collect();
    assert_eq!(
        actual[..8].iter().map(|v| v.0).collect::<Vec<_>>(),
        expected
    );
    assert_eq!(
        actual[8..].iter().map(|v| v.1).collect::<Vec<_>>(),
        expected_blends
    );
}

#[test]
fn readback_layout_errors_and_frame_budget_fail_without_partial_submission() {
    let (mut r, control) = setup();
    let target = r.create_target(3, 2, "test");
    assert_eq!(r.try_read_target(&target).unwrap().len(), 3 * 2 * 4);
    control.borrow_mut().wrong_readback_length = true;
    assert!(r.try_read_target(&target).is_err());
    let mut enc = r.encoder();
    enc.vertex_bytes = MAX_FRAME_VERTEX_BYTES;
    let q = Quad::rect(r.white.clone(), [0.0, 0.0, 1.0, 1.0], [1.0; 4]);
    assert!(r.try_draw(&mut enc, &target, None, &[q]).is_err());
    assert!(enc.commands.is_empty());
    assert!(commands(&control).is_empty());
}

#[test]
fn output_curve_matches_desktop_levels() {
    assert_eq!(
        [0, 8, 12, 21, 63, 255].map(output_level),
        [0, 2, 4, 13, 61, 255]
    );
    for v in 0..=255 {
        assert_eq!(target_level(v), crate::desktop::target_level(v));
    }
}

#[test]
fn r8_ignores_srgb_hints_and_numeric_overflow_never_records_a_partial_pass() {
    let (mut r, control) = setup();
    let altered = r
        .try_upload_altered(1, 1, Upload::R8(&[128]), true, "coverage")
        .unwrap();
    assert!(altered.native.srgb);
    assert!(control.borrow().events.iter().any(|e| matches!(
        e,
        Event::Create {
            format: TextureFormat::R8,
            srgb: false,
            ..
        }
    )));
    let target = r.create_target(2, 2, "target");
    let mut enc = r.encoder();
    assert!(
        r.try_blur(
            &mut enc,
            &r.white.clone(),
            &target,
            [1.0, 0.0],
            f32::MAX,
            BlurMode::Uv,
        )
        .is_err()
    );
    assert!(enc.commands.is_empty());
    let q = Quad::rect(r.white.clone(), [f32::MAX, 0.0, 0.0, 1.0], [1.0; 4]);
    assert!(
        r.try_draw(&mut enc, &r.create_target(1, 1, "tiny"), None, &[q])
            .is_err()
    );
    assert!(enc.commands.is_empty());
}

#[test]
fn fully_clipped_draw_still_records_the_clear() {
    let (mut r, _) = setup();
    let target = r.create_target(2, 2, "culled-clear-target");
    let mut q = Quad::rect(r.white.clone(), [0.0, 0.0, 1.0, 1.0], [1.0; 4]);
    q.clip = Some([[3.0, 3.0], [4.0, 3.0], [4.0, 4.0], [3.0, 4.0]]);
    let mut enc = r.encoder();
    r.try_draw(&mut enc, &target, Some([0.25; 4]), &[q]).unwrap();
    // The clear is still recorded; the pass stays open for later draws and is closed
    // by the next non-draw command or by present.
    assert!(matches!(enc.commands.as_slice(),
        [Command::Begin { clear: Some(_), .. }]), "{} commands", enc.commands.len());
}

#[test]
fn visible_ps_copies_before_its_draw_and_keeps_following_quads() {
    let (mut r, _) = setup();
    let target = r.create_target(4, 4, "ps-order-target");
    let mut ps = Quad::rect(r.white.clone(), [0.0, 0.0, 2.0, 2.0], [1.0; 4]);
    ps.blend = Blend::Ps(PsOp::Overlay);
    let normal = Quad::rect(r.white.clone(), [0.0, 0.0, 1.0, 1.0], [1.0; 4]);
    let mut enc = r.encoder();
    r.try_draw(&mut enc, &target, None, &[ps, normal]).unwrap();
    let kinds: Vec<&str> = enc.commands.iter().map(|c| match c {
        Command::Begin { .. } => "begin",
        Command::End => "end",
        Command::Copy { .. } => "copy",
        Command::Draw { .. } => "draw",
    }).collect();
    let copy = kinds.iter().position(|k| *k == "copy").expect("copy recorded");
    let ps_draw = kinds.iter().rposition(|k| *k == "draw").expect("draw recorded");
    assert!(copy < ps_draw, "{kinds:?}");
    assert_eq!(r.backdrop_count(), 1);
    assert_eq!(kinds.iter().filter(|k| **k == "draw").count(), 2, "{kinds:?}");
}

#[test]
fn identical_draws_in_one_run_are_coalesced() {
    let (mut r, control) = setup();
    let target = r.create_scene_target(CANVAS_W, CANVAS_H, "merge-test");
    let a = Quad::rect(r.white.clone(), [0.0, 0.0, 50.0, 50.0], [1.0; 4]);
    let b = Quad::rect(r.white.clone(), [10.0, 10.0, 20.0, 20.0], [1.0; 4]);
    let mut enc = r.encoder();
    r.try_draw(&mut enc, &target, None, &[a, b]).unwrap();
    r.try_present(enc, &target).unwrap();
    // Same shader, blend, textures and haze: one submission carries both quads.
    assert_eq!(commands(&control), ["begin", "draw", "end", "present"]);
    let log = control.borrow();
    let vertices = log.events.iter().find_map(|e| match e {
        Event::Draw { vertices, .. } => Some(vertices.len()),
        _ => None,
    });
    assert_eq!(vertices, Some(12), "two quads of six vertices");
}
