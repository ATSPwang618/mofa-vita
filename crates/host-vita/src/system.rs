//! Vita runs one instance of this application. Named locks arbitrate engine
//! sessions within that process and are released when the owning engine drops.
use krkr_engine::system::SystemHost;
use std::{
    collections::HashSet,
    sync::{LazyLock, Mutex},
};

static LOCKS: LazyLock<Mutex<HashSet<Vec<u16>>>> = LazyLock::new(Mutex::default);

#[derive(Default)]
pub struct VitaSystem {
    locks: Vec<Vec<u16>>,
}

impl SystemHost for VitaSystem {
    fn create_app_lock(&mut self, name: &[u16]) -> Result<bool, String> {
        let mut locks = LOCKS.lock().map_err(|_| "application locks poisoned")?;
        if !locks.insert(name.to_vec()) {
            return Ok(false);
        }
        self.locks.push(name.to_vec());
        Ok(true)
    }
}

impl Drop for VitaSystem {
    fn drop(&mut self) {
        let mut locks = LOCKS.lock().unwrap_or_else(|e| e.into_inner());
        for name in &self.locks {
            locks.remove(name);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_exclusive_and_released_by_their_owner() {
        let mut owner = VitaSystem::default();
        let mut other = VitaSystem::default();
        let name = [0xd800, 0x61];
        assert!(owner.create_app_lock(&name).unwrap());
        assert!(!owner.create_app_lock(&name).unwrap());
        assert!(!other.create_app_lock(&name).unwrap());
        assert!(other.create_app_lock(&[0xd800, 0x62]).unwrap());
        drop(owner);
        assert!(other.create_app_lock(&name).unwrap());
    }
}
