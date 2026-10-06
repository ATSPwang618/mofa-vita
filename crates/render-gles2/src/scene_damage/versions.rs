//! A full single-tile snapshot needs only a weak identity and generation.
//! Its rectangle is the plane extent; cropped views retain explicit records.
use crate::{
    device::Texture,
    image::{Plane, Tile},
};
use krkr_protocol::graphics::{Rect, Size};
use std::rc::{Rc, Weak};

type Record = (Rect, Weak<Texture>, u64, Option<Rect>);
type View<'a> = (Rect, &'a Weak<Texture>, u64, Option<Rect>);

#[derive(Clone)]
pub(super) enum Tiles {
    Whole(Weak<Texture>, u64),
    Parts(Vec<Record>),
}
impl Tiles {
    fn whole(plane: &Plane) -> Option<&Tile> {
        match plane.tiles.as_slice() {
            [tile] if tile.rectangle == plane.size.rect() && tile.backing.is_none() => Some(tile),
            _ => None,
        }
    }
    pub fn capture(plane: &Plane) -> Self {
        if let Some(tile) = Self::whole(plane) {
            Self::Whole(Rc::downgrade(&tile.texture), tile.texture.generation.get())
        } else {
            Self::Parts(
                plane
                    .tiles
                    .iter()
                    .map(|tile| {
                        (
                            tile.rectangle,
                            Rc::downgrade(&tile.texture),
                            tile.texture.generation.get(),
                            tile.backing,
                        )
                    })
                    .collect(),
            )
        }
    }
    pub fn allocation_bytes(plane: &Plane) -> usize {
        if Self::whole(plane).is_some() {
            0
        } else {
            plane.tiles.len() * std::mem::size_of::<Record>()
        }
    }
    pub fn bytes(&self) -> usize {
        match self {
            Self::Whole(..) => 0,
            Self::Parts(tiles) => tiles.capacity() * std::mem::size_of::<Record>(),
        }
    }
    pub fn len(&self) -> usize {
        match self {
            Self::Whole(..) => 1,
            Self::Parts(tiles) => tiles.len(),
        }
    }
    pub fn iter(&self, size: Size) -> impl Iterator<Item = View<'_>> {
        let (whole, parts) = match self {
            Self::Whole(texture, generation) => {
                (Some((size.rect(), texture, *generation, None)), &[][..])
            }
            Self::Parts(tiles) => (None, tiles.as_slice()),
        };
        whole.into_iter().chain(
            parts.iter().map(|(rect, texture, generation, backing)| {
                (*rect, texture, *generation, *backing)
            }),
        )
    }
}
