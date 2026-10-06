use krkr_protocol::graphics::{Blend, Node, Size};
use krkr_render::scene::Children;

fn node(parent: Option<usize>) -> Node {
    Node {
        cache: None,
        visible: true,
        parent,
        image: None,
        neutral_color: 0,
        rectangle: Size {
            width: 1,
            height: 1,
        }
        .rect(),
        image_left: 0,
        image_top: 0,
        blend: Blend::Opaque,
        opacity: 255,
    }
}

#[test]
fn empty_scenes_and_independent_roots_need_no_children() {
    assert!(Children::new(&[], 0).is_ok());
    let nodes = vec![node(None); 4];
    let children = Children::new(&nodes, 0).unwrap();
    for index in 0..nodes.len() {
        assert!(children[index].is_empty());
    }
}

#[test]
fn interleaved_subtrees_keep_sibling_order_and_hidden_nodes() {
    let mut nodes = [
        None,
        Some(0),
        None,
        Some(1),
        Some(0),
        Some(2),
        Some(1),
        Some(2),
        None,
        Some(0),
    ]
    .map(node);
    nodes[1].visible = false;
    nodes[2].opacity = 0;
    let children = Children::new(&nodes, 2).unwrap();
    assert_eq!(&children[0], &[1, 4, 9]);
    assert_eq!(&children[1], &[3, 6]);
    assert_eq!(&children[2], &[5, 7]);
    for index in 3..nodes.len() {
        assert!(children[index].is_empty());
    }
}

#[test]
fn invalid_parent_indices_are_rejected_even_when_hidden() {
    for parent in [1, 2, usize::MAX] {
        let mut nodes = [node(None), node(Some(parent)), node(Some(0))];
        nodes[1].visible = false;
        assert!(Children::new(&nodes, 128).is_err());
    }
    assert!(Children::new(&[node(Some(0))], 128).is_err());
}

#[test]
fn renderer_depth_limits_include_the_last_allowed_level() {
    let nodes: Vec<_> = (0usize..130).map(|i| node(i.checked_sub(1))).collect();
    for max_depth in [0, 1, 127, 128] {
        let children = Children::new(&nodes[..=max_depth], max_depth).unwrap();
        for index in 0..max_depth {
            assert_eq!(&children[index], &[index + 1]);
        }
        assert!(children[max_depth].is_empty());
        assert!(Children::new(&nodes[..=max_depth + 1], max_depth).is_err());
    }
}

#[test]
fn wide_trees_keep_every_child_in_scene_order() {
    let mut nodes = vec![node(None)];
    nodes.extend((1..1024).map(|_| node(Some(0))));
    let children = Children::new(&nodes, 1).unwrap();
    let expected: Vec<_> = (1..nodes.len()).collect();
    assert_eq!(&children[0], expected.as_slice());
    for index in 1..nodes.len() {
        assert!(children[index].is_empty());
    }
}
