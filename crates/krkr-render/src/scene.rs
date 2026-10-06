//! Validated scene topology shared by the rendering backends.
use crate::{Error, Result};
use krkr_protocol::graphics::Node;
use std::ops::{Index, Range};

/// Direct children in scene order, with two buffers instead of one allocation
/// per parent. Node indices may contain multiple roots and interleaved subtrees.
pub struct Children {
    ranges: Vec<Range<usize>>,
    indices: Vec<usize>,
}
impl Children {
    /// Parents must precede their children. Roots have depth zero;
    /// `max_depth` is the greatest allowed depth, including invisible nodes.
    pub fn new(nodes: &[Node], max_depth: usize) -> Result<Self> {
        let mut ranges = vec![0..0; nodes.len()];
        // Temporarily store depth in start and child count in end. This avoids
        // a separate depth buffer during validation.
        for (index, node) in nodes.iter().enumerate() {
            if let Some(parent) = node.parent {
                if parent >= index {
                    return Err(Error::Message("scene parent must precede its children"));
                }
                let depth = ranges[parent].start + 1;
                if depth > max_depth {
                    return Err(Error::Message("scene nesting exceeds renderer limit"));
                }
                ranges[index].start = depth;
                ranges[parent].end += 1;
            }
        }
        let mut count = 0;
        for range in &mut ranges {
            let children = range.end;
            *range = count..count;
            count += children;
        }
        let mut indices = vec![0; count];
        // Advance each range's end while filling, preserving sibling order.
        for (index, node) in nodes.iter().enumerate() {
            if let Some(parent) = node.parent {
                let range = &mut ranges[parent];
                indices[range.end] = index;
                range.end += 1;
            }
        }
        Ok(Self { ranges, indices })
    }
}
impl Index<usize> for Children {
    type Output = [usize];

    fn index(&self, node: usize) -> &Self::Output {
        &self.indices[self.ranges[node].clone()]
    }
}
