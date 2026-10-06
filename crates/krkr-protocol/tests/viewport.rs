use krkr_protocol::{graphics::Size, viewport::Viewport};

#[test]
fn zoom_uses_rounded_primary_extent_and_truncates_inverse_coordinates() {
    let mut viewport = Viewport::default().zoom(6, 4).unwrap();
    viewport.left = 10;
    viewport.top = -5;
    let size = Size {
        width: 3,
        height: 5,
    };
    let rect = viewport.destination(size);
    assert_eq!((viewport.numer(), viewport.denom()), (3, 2));
    assert_eq!((rect.width, rect.height), (5, 8));
    assert_eq!(viewport.to_layer(size, (14, 2)), (2, 4));
    assert_eq!(viewport.to_layer(size, (9, -6)), (0, 0));
    assert_eq!(viewport.to_window(size, (3, 5)), (15, 3));
    assert_eq!(
        Viewport::default()
            .zoom(1, 1000)
            .unwrap()
            .destination(size)
            .width,
        1
    );
}

#[test]
fn signed_and_degenerate_zoom_ratios_follow_reference_reduction_and_minimum_extent() {
    let size = Size {
        width: 100,
        height: 50,
    };
    for (n, d, expected) in [
        (0, 1, (0, 1)),
        (1, 0, (1, 0)),
        (-6, -4, (3, 2)),
        (-6, 4, (3, -2)),
        (6, -4, (3, -2)),
    ] {
        let view = Viewport::default().zoom(n, d).unwrap();
        assert_eq!((view.numer(), view.denom()), expected);
        assert_eq!(
            view.destination(size).width,
            if n < 0 && d < 0 { 150 } else { 1 }
        );
    }
    assert!(Viewport::default().zoom(0, 0).is_err());
    assert_eq!(
        Viewport::default()
            .zoom(i32::MAX, 1)
            .unwrap()
            .destination(size)
            .width,
        1
    );
}
