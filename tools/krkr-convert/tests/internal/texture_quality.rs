use super::*;

#[test]
fn translucent_colors_are_checked_on_both_dark_and_light_backgrounds() {
    let source = [180, 120, 80, 80].repeat(64);
    assert!(
        assess(&source, &[140, 80, 40, 80].repeat(64), true)
            .unwrap()
            .contains("composite")
    );
    assert!(assess(&source, &[181, 119, 81, 80].repeat(64), true).is_none());
    // A large invisible border must not dilute the visible error.
    let mut padded = source.clone();
    padded.extend([0; 4].repeat(4096));
    let mut damaged = [140, 80, 40, 80].repeat(64);
    damaged.extend([255, 200, 100, 0].repeat(4096));
    assert!(assess(&padded, &damaged, true).is_some());
}

#[test]
fn rejects_wholesale_translucency_and_visible_color_loss() {
    let source = [48, 120, 201, 255].repeat(1024);
    assert!(
        assess(&source, &[48, 120, 201, 238].repeat(1024), false)
            .unwrap()
            .contains("opaque")
    );
    assert!(
        assess(&source, &[64, 120, 187, 255].repeat(1024), true)
            .unwrap()
            .contains("color")
    );
    assert!(assess(&source, &[49, 121, 200, 255].repeat(1024), true).is_none());
    let mut fringe = source.clone();
    fringe[3] = 250;
    assert!(assess(&source, &fringe, true).is_none());
}

#[test]
fn invisible_rgb_does_not_reject_a_texture_but_alpha_still_does() {
    let source = [255, 20, 200, 0].repeat(1024);
    assert!(assess(&source, &[0, 0, 0, 0].repeat(1024), true).is_none());
    assert!(
        assess(&source, &[0, 0, 0, 17].repeat(1024), true)
            .unwrap()
            .contains("alpha")
    );
}
