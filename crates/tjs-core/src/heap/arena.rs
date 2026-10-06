//! Stable handles with a dense key list for resumable, linear-time sweeping.
//! Payload lookup still uses SlotMap directly; holes never cost sweep work.
use slotmap::{Key, SlotMap};
use std::ops::{Index, IndexMut};

pub(super) struct Arena<K: Key, T> {
    slots: SlotMap<K, T>,
    keys: Vec<K>,
}

impl<K: Key, T> Default for Arena<K, T> {
    fn default() -> Self {
        Self {
            slots: SlotMap::with_key(),
            keys: Vec::new(),
        }
    }
}

impl<K: Key, T> Arena<K, T> {
    pub fn insert(&mut self, value: T) -> K {
        let key = self.slots.insert(value);
        self.keys.push(key);
        key
    }

    pub fn len(&self) -> usize {
        self.keys.len()
    }

    pub fn capacity(&self) -> usize {
        self.slots.capacity()
    }

    pub fn get(&self, key: K) -> Option<&T> {
        self.slots.get(key)
    }

    pub fn get_mut(&mut self, key: K) -> Option<&mut T> {
        self.slots.get_mut(key)
    }

    pub fn get_disjoint_mut<const N: usize>(&mut self, keys: [K; N]) -> Option<[&mut T; N]> {
        self.slots.get_disjoint_mut(keys)
    }

    pub fn retain(&mut self, mut keep: impl FnMut(K, &mut T) -> bool) {
        // Full collection has no suspended sweep cursor. Rebuild the key list
        // during the contiguous scan instead of looking up every key again.
        self.keys.clear();
        let keys = &mut self.keys;
        self.slots.retain(|key, value| {
            let live = keep(key, value);
            if live {
                keys.push(key);
            }
            live
        });
    }

    pub fn values(&self) -> impl Iterator<Item = &T> {
        self.slots.values()
    }

    pub fn key_at(&self, index: usize) -> K {
        self.keys[index]
    }

    /// Sweep backwards. A swapped-in key has already been visited or was
    /// allocated during the cycle, so it needs no second visit.
    pub fn remove_at(&mut self, index: usize) -> T {
        let key = self.keys.swap_remove(index);
        self.slots.remove(key).expect("arena key")
    }
}

impl<K: Key, T> Index<K> for Arena<K, T> {
    type Output = T;
    fn index(&self, key: K) -> &T {
        &self.slots[key]
    }
}

impl<K: Key, T> IndexMut<K> for Arena<K, T> {
    fn index_mut(&mut self, key: K) -> &mut T {
        &mut self.slots[key]
    }
}
