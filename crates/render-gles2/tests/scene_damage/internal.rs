use super::*;
use krkr_protocol::budget::Budget;

const SCREEN: Size = Size {
    width: 960,
    height: 544,
};
const GLYPH: Rect = Rect {
    left: 200,
    top: 400,
    width: 32,
    height: 32,
};

fn node(parent: Option<usize>, clip: Rect, color: u32) -> Node {
    Node {
        geometry: Geometry {
            parent,
            visible: true,
            size: Size {
                width: clip.width,
                height: clip.height,
            },
            clip: Some(clip),
            origin: (clip.left.into(), clip.top.into()),
            image_origin: (clip.left.into(), clip.top.into()),
            blend: Blend::Opaque,
            opacity: 255,
            neutral: color,
        },
        image: None,
        transition_input: false,
        transition_children: false,
    }
}
fn stamp(nodes: Vec<Node>) -> Stamp {
    Stamp {
        logical: SCREEN,
        physical: SCREEN,
        nodes,
        transitions: Vec::new(),
        _permit: Budget::new(0).reserve(0).unwrap(),
    }
}
fn root() -> Node {
    node(None, SCREEN.rect(), 0xff000000)
}
fn overlay() -> Node {
    let mut n = node(Some(0), SCREEN.rect(), 0x80000000);
    n.geometry.blend = Blend::Alpha;
    n
}

#[test]
fn inserting_and_removing_a_glyph_preserves_unchanged_overlay_pixels() {
    let old = stamp(vec![root(), overlay()]);
    let new = stamp(vec![root(), node(Some(0), GLYPH, 0xffffffff), overlay()]);
    assert_eq!(new.damage(&old), Some(GLYPH));
    assert_eq!(old.damage(&new), Some(GLYPH));
    assert_eq!(new.damage(&new), None);
}

#[test]
fn fading_a_glyph_and_inserting_another_does_not_damage_later_siblings() {
    let second = Rect { left: 232, ..GLYPH };
    let far = Rect {
        left: 800,
        top: 20,
        ..GLYPH
    };
    let old = stamp(vec![
        root(),
        node(Some(0), GLYPH, 0xffffffff),
        node(Some(0), far, 0xffabcdef),
        overlay(),
    ]);
    let mut fading = node(Some(0), GLYPH, 0xffffffff);
    fading.geometry.opacity = 102;
    let new = stamp(vec![
        root(),
        fading,
        node(Some(0), second, 0xff123456),
        node(Some(0), far, 0xffabcdef),
        overlay(),
    ]);
    assert_eq!(new.damage(&old), Some(union(Some(GLYPH), second)));
}

#[test]
fn reordered_overlapping_siblings_still_repaint_both_extents() {
    let second = Rect { left: 216, ..GLYPH };
    let old = stamp(vec![
        root(),
        node(Some(0), GLYPH, 0xff123456),
        node(Some(0), second, 0xffabcdef),
        overlay(),
    ]);
    let new = stamp(vec![
        root(),
        node(Some(0), second, 0xffabcdef),
        node(Some(0), GLYPH, 0xff123456),
        overlay(),
    ]);
    assert_eq!(new.damage(&old), Some(union(Some(GLYPH), second)));
}

#[test]
fn equal_parent_indices_in_edited_range_cannot_hide_reparenting() {
    // The child's parent is index 1 in both snapshots, but insertion changes
    // which group occupies that slot. Its pixels must stay in the comparison.
    let group = Rect {
        left: 100,
        top: 300,
        width: 300,
        height: 180,
    };
    let old = stamp(vec![
        root(),
        node(Some(0), group, 0xff000000),
        node(Some(1), GLYPH, 0xffffffff),
    ]);
    let new = stamp(vec![
        root(),
        node(Some(0), group, 0xff444444),
        node(Some(0), group, 0xff000000),
        node(Some(1), GLYPH, 0xffffffff),
    ]);
    let (a, b) = changed_nodes(&new.nodes, &old.nodes);
    assert_eq!((a.len(), b.len()), (3, 2));
    assert_eq!(new.damage(&old), Some(group));
}

#[test]
fn changed_ancestor_still_invalidates_the_screen() {
    let mut changed_root = root();
    changed_root.geometry.opacity = 150;
    let old = stamp(vec![root(), node(Some(0), GLYPH, 0xffffffff), overlay()]);
    let new = stamp(vec![
        changed_root,
        node(Some(0), GLYPH, 0xffffffff),
        overlay(),
    ]);
    assert_eq!(new.damage(&old), Some(SCREEN.rect()));
}
