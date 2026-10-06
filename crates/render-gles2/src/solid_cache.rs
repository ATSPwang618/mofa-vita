//! Blank script layers share an initialized plane until their first write.
//! Entries are weak and checked against texture writes, so remembering a clear
//! never pins GPU storage or mistakes edited pixels for a uniform image.
use crate::{Image, image::Plane};
use krkr_protocol::{
    budget::{Budget, Permit},
    graphics::Size,
};
use std::{
    collections::VecDeque,
    rc::{Rc, Weak},
};

struct Entry {
    color: u32,
    plane: Weak<Plane>,
    generations: Vec<u64>,
    _permit: Permit,
}
impl Entry {
    fn current(&self) -> Option<Rc<Plane>> {
        let plane = self.plane.upgrade()?;
        (plane.tiles.len() == self.generations.len()
            && plane
                .tiles
                .iter()
                .zip(&self.generations)
                .all(|(tile, generation)| tile.texture.generation.get() == *generation))
        .then_some(plane)
    }
}
#[derive(Default)]
pub(crate) struct Cache(VecDeque<Entry>);
impl Cache {
    pub fn trim(&mut self) {
        self.0.retain(|e| e.current().is_some());
    }
    pub fn get(&mut self, size: Size, color: u32) -> Option<Rc<Plane>> {
        self.trim();
        let index = self
            .0
            .iter()
            .position(|e| e.color == color && e.current().is_some_and(|p| p.size == size))?;
        let entry = self.0.remove(index)?;
        let plane = entry.current();
        self.0.push_back(entry);
        plane
    }
    pub fn color(&self, image: &Image) -> Option<u32> {
        let plane = image.main.as_ref()?;
        self.0
            .iter()
            .find(|e| Weak::ptr_eq(&e.plane, &Rc::downgrade(plane)) && e.current().is_some())
            .map(|e| e.color)
    }
    pub fn insert(&mut self, image: &Image, color: u32, resident: &Budget, metadata: &Budget) {
        let Some(plane) = image.main.as_ref() else {
            return;
        };
        if !image.canvas
            || plane.tiles.len() > 64
            || !plane.tiles.iter().all(|t| t.texture.belongs_to(resident))
        {
            return;
        }
        self.0
            .retain(|e| e.current().is_some_and(|p| !Rc::ptr_eq(&p, plane)));
        if self.0.len() == 32 {
            self.0.pop_front();
        }
        let Ok(permit) = metadata
            .reserve(std::mem::size_of::<Entry>() + plane.tiles.len() * std::mem::size_of::<u64>())
        else {
            return;
        };
        self.0.push_back(Entry {
            color,
            plane: Rc::downgrade(plane),
            generations: plane
                .tiles
                .iter()
                .map(|t| t.texture.generation.get())
                .collect(),
            _permit: permit,
        });
    }
}
