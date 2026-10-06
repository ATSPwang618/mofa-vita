use super::*;
use crate::gles_test_support as support;

#[test]
fn keyboard_renders_once_and_selection_patches_release_on_close() {
    use glow::HasContext;
    let context = support::Context::sized(960, 544);
    let gpu = unsafe { Gpu::new(context.gl(), Default::default()).unwrap() };
    let background = gpu
        .create_image(crate::window::DISPLAY, 0xff648088)
        .unwrap();
    gpu.present(
        &background,
        crate::window::DISPLAY,
        crate::window::DISPLAY.rect(),
    )
    .unwrap();
    let baseline = gpu.resident.used();
    let mut panel = Panel::new(Bindings::default(), Language::Chinese);
    assert!(panel.surface.is_none());
    assert!(panel.refresh(&gpu).unwrap());
    panel.draw(&gpu).unwrap();
    assert!(!panel.refresh(&gpu).unwrap());
    let first = gpu.resident.used();
    assert!(first - baseline < 2 * 1024 * 1024);
    panel.poll(
        Sample {
            buttons: RIGHT,
            ..Default::default()
        },
        Instant::now(),
    );
    assert!(panel.refresh(&gpu).unwrap());
    gpu.present(
        &background,
        crate::window::DISPLAY,
        crate::window::DISPLAY.rect(),
    )
    .unwrap();
    panel.draw(&gpu).unwrap();
    let mut rgba = vec![0; 960 * 544 * 4];
    unsafe {
        context.gl().read_pixels(
            0,
            0,
            960,
            544,
            glow::RGBA,
            glow::UNSIGNED_BYTE,
            glow::PixelPackData::Slice(Some(&mut rgba)),
        );
    }
    if let Some(directory) = std::env::var_os("KRKR_LAUNCHER_PREVIEW") {
        let top_down: Vec<_> = rgba
            .chunks_exact(960 * 4)
            .rev()
            .flatten()
            .copied()
            .collect();
        std::fs::write(
            std::path::Path::new(&directory).join("vita-keyboard.rgba"),
            top_down,
        )
        .unwrap();
    }
    assert!(
        rgba.chunks_exact(4)
            .any(|p| p[0] > 220 && p[1] > 220 && p[2] > 220)
    );
    drop(panel);
    gpu.collect().unwrap();
    assert_eq!(gpu.resident.used(), baseline);
}
#[test]
fn touch_ctrl_s_and_mapping_capture_do_not_send_editor_keys_to_game() {
    let mut panel = Panel::new(Bindings::default(), Language::Chinese);
    let now = Instant::now();
    let tap = |p: &Panel, key: u16, id: u8| {
        let r = p.keys.iter().find(|k| k.code == key).unwrap().rectangle;
        let (x, y) = center(r);
        Sample {
            touch: Some((id, (x, y + p.top()))),
            ..Default::default()
        }
    };
    let event = panel.poll(tap(&panel, 17, 1), now);
    assert!(event.output.keys.contains(17));
    panel.poll(Sample::default(), now);
    let event = panel.poll(tap(&panel, 83, 2), now);
    assert!(event.output.keys.contains(17));
    assert!(event.output.keys.contains(83));
    let event = panel.poll(Sample::default(), now);
    assert!(event.output.keys.contains(17));
    assert!(!event.output.keys.contains(83));
    let event = panel.poll(
        Sample {
            buttons: SQUARE,
            ..Default::default()
        },
        now,
    );
    assert_eq!(event.output, Output::default());
    panel.row = panel.bindings.entries.len();
    panel.poll(
        Sample {
            buttons: CIRCLE,
            ..Default::default()
        },
        now,
    );
    assert!(panel.mode == Mode::Capture);
    panel.poll(Sample::default(), now);
    panel.poll(
        Sample {
            buttons: L,
            ..Default::default()
        },
        now,
    );
    panel.poll(
        Sample {
            buttons: L | CIRCLE,
            ..Default::default()
        },
        now,
    );
    let event = panel.poll(Sample::default(), now);
    assert_eq!(event.output, Output::default());
    assert!(panel.mode == Mode::Edit);
    assert_eq!(panel.trigger, L | CIRCLE);
    panel.poll(tap(&panel, 17, 3), now);
    panel.poll(Sample::default(), now);
    let event = panel.poll(tap(&panel, 83, 4), now);
    assert_eq!(event.output, Output::default());
    panel.poll(Sample::default(), now);
    let event = panel.poll(
        Sample {
            buttons: TRIANGLE,
            ..Default::default()
        },
        now,
    );
    assert!(event.save);
    assert_eq!(
        panel.bindings.entries.last().unwrap().action,
        Action::Key {
            key: 83,
            modifiers: 4
        }
    );
}
