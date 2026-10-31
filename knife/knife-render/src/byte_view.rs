//! Movable immutable encoded allocation and bounded, zero-copy atlas ranges.
use std::{ops::{Deref, Range}, sync::Arc};

#[derive(Clone, Debug)]
pub struct ByteView {
    owner: Arc<Vec<u8>>,
    range: Range<usize>,
}
impl ByteView {
    pub fn owned(bytes: Vec<u8>) -> Self {
        let len = bytes.len();
        Self { owner: Arc::new(bytes), range: 0..len }
    }
    pub fn range(&self, offset: usize, len: usize) -> Option<Self> {
        let end = offset.checked_add(len)?;
        if end > self.len() { return None; }
        Some(Self { owner: self.owner.clone(), range: self.range.start + offset..self.range.start + end })
    }
    /// Charge the whole allocation, once per cache owner, rather than each view.
    pub fn allocation_len(&self) -> usize { self.owner.capacity() }
    pub fn is_unpinned(&self) -> bool { Arc::strong_count(&self.owner) == 1 }
}
impl Deref for ByteView {
    type Target = [u8];
    fn deref(&self) -> &[u8] { &self.owner[self.range.clone()] }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn ranges_keep_same_allocation_and_check_nested_bounds() {
        let mut bytes = Vec::with_capacity(128);
        bytes.extend(0..32);
        let address = bytes.as_ptr();
        let root = ByteView::owned(bytes);
        assert_eq!(root.as_ptr(), address);
        assert_eq!(root.allocation_len(), 128);
        assert!(root.is_unpinned());
        let a = root.range(4, 8).unwrap();
        assert_eq!(a.as_ptr(), unsafe { address.add(4) });
        assert!(!root.is_unpinned());
        assert_eq!(&*a.range(2, 3).unwrap(), &[6, 7, 8]);
        assert!(a.range(7, 2).is_none());
        assert!(a.range(usize::MAX, 1).is_none());
        drop(a);
        assert!(root.is_unpinned());
    }
}

/// A CPU pixel allocation with its original allocator's destructor. The renderer
/// borrows it through synchronous native upload; no C pointer becomes a Rust Vec.
pub struct PixelOwner(Box<dyn AsRef<[u8]>>);
impl PixelOwner {
    pub fn new(owner: impl AsRef<[u8]> + 'static) -> Self { Self(Box::new(owner)) }
}
impl From<Vec<u8>> for PixelOwner { fn from(bytes: Vec<u8>) -> Self { Self::new(bytes) } }
impl Deref for PixelOwner {
    type Target = [u8];
    fn deref(&self) -> &[u8] { self.0.as_ref().as_ref() }
}

/// Immutable shared CPU pixels with an allocator-owned backing block. A cropped
/// view keeps the whole block charged until the last CPU upload/cache pin ends.
#[derive(Clone)]
pub struct PixelView {
    owner: Arc<dyn AsRef<[u8]> + Send + Sync>,
    range: Range<usize>, allocation: usize,
}
impl PixelView {
    pub fn owned(owner: impl AsRef<[u8]> + Send + Sync + 'static, allocation: usize) -> Self {
        let length=owner.as_ref().len();
        assert!(allocation >= length, "pixel owner charge must cover every initialized byte");
        Self { owner:Arc::new(owner), range:0..length, allocation }
    }
    pub fn range(&self, offset: usize, len: usize) -> Option<Self> {
        let end=offset.checked_add(len)?;
        if end>self.range.len() {return None;}
        Some(Self {owner:self.owner.clone(),range:self.range.start+offset..self.range.start+end,allocation:self.allocation})
    }
    pub fn allocation_len(&self) -> usize { self.allocation }
    pub fn is_unpinned(&self) -> bool { Arc::strong_count(&self.owner)==1 }
}
impl AsRef<[u8]> for PixelView { fn as_ref(&self)->&[u8] { &self.owner.as_ref().as_ref()[self.range.clone()] } }
impl Deref for PixelView { type Target=[u8];fn deref(&self)->&[u8] { self.as_ref() } }
