//! Bounded admission of cached images under external GPU pressure.
//! Independent of the Vita renderer so the policy can be tested on a host.

use std::collections::HashMap;
use std::sync::Arc;

#[derive(Default)]
pub struct ImageCachePolicy {
    last_use: HashMap<String, u128>,
    clock: u128,
}

impl ImageCachePolicy {
    pub fn touch(&mut self, key: &str) {
        self.clock = self.clock.saturating_add(1);
        self.last_use.insert(key.to_owned(), self.clock);
    }

    pub fn retain_keys(&mut self, live: impl Fn(&str) -> bool) {
        self.last_use.retain(|key, _| live(key));
    }

    #[cfg(test)]
    pub fn admit<T, E>(
        &mut self,
        images: &mut HashMap<String, Option<Arc<T>>>,
        reserve: impl FnMut() -> Result<(), E>,
    ) -> Result<usize, E> {
        self.admit_if(images, reserve, |_| true)
    }

    pub fn admit_if<T, E>(
        &mut self,
        images: &mut HashMap<String, Option<Arc<T>>>,
        mut reserve: impl FnMut() -> Result<(), E>,
        is_pressure: impl Fn(&E) -> bool,
    ) -> Result<usize, E> {
        let mut last_error = match reserve() {
            Ok(()) => return Ok(0),
            Err(error) if !is_pressure(&error) => return Err(error),
            Err(error) => error,
        };
        // Collect keys only: cloning an Arc here would make an idle entry active.
        let mut idle: Vec<_> = images
            .iter()
            .filter_map(|(key, image)| match image {
                Some(image) if Arc::strong_count(image) == 1 => {
                    Some((self.last_use.get(key).copied().unwrap_or(0), key.clone()))
                }
                _ => None,
            })
            .collect();
        idle.sort_unstable(); // last use, then key: deterministic for never-touched entries
        let mut evicted = 0;
        for (_, key) in idle {
            // The removed Arc is dropped before probing native headroom again.
            if images.remove(&key).is_none() {
                continue;
            }
            evicted += 1;
            self.last_use.remove(&key);
            match reserve() {
                Ok(()) => return Ok(evicted),
                Err(error) if !is_pressure(&error) => return Err(error),
                Err(error) => last_error = error,
            }
        }
        Err(last_error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::rc::Rc;

    #[test]
    fn unrelated_driver_error_never_evicts_images() {
        let mut p = ImageCachePolicy::default();
        let mut images = HashMap::from([("idle".into(), Some(Arc::new(1)))]);
        assert_eq!(p.admit_if(&mut images, || Err("driver"), |e| *e == "pressure"), Err("driver"));
        assert_eq!(images.len(), 1);
    }

    #[test]
    fn no_pressure_keeps_every_cached_entry_and_probes_once() {
        let mut p = ImageCachePolicy::default();
        let mut images =
            HashMap::from([("idle".into(), Some(Arc::new(7))), ("missing".into(), None)]);
        let mut attempts = 0;
        assert_eq!(
            p.admit(&mut images, || {
                attempts += 1;
                Ok::<_, ()>(())
            }),
            Ok(0)
        );
        assert_eq!(attempts, 1);
        assert_eq!(images.len(), 2);
        assert!(images.contains_key("idle") && images.contains_key("missing"));
    }

    #[test]
    fn pressure_evicts_idle_even_below_old_400_entry_threshold() {
        let mut p = ImageCachePolicy::default();
        let mut images = HashMap::from([("idle".into(), Some(Arc::new(1u8)))]);
        let mut attempts = 0;
        let evicted = p.admit(&mut images, || {
            attempts += 1;
            (attempts > 1).then_some(()).ok_or("pressure")
        });
        assert_eq!(evicted, Ok(1));
        assert_eq!(attempts, 2);
        assert!(!images.contains_key("idle"));
    }

    #[test]
    fn least_recently_used_first_and_stop_at_sufficient_headroom() {
        let mut p = ImageCachePolicy::default();
        let mut images = HashMap::from([
            ("a".into(), Some(Arc::new(1))),
            ("b".into(), Some(Arc::new(2))),
            ("c".into(), Some(Arc::new(3))),
            ("missing".into(), None),
        ]);
        p.touch("a");
        p.touch("b");
        p.touch("c");
        p.touch("a");
        let mut attempts = 0;
        assert_eq!(
            p.admit(&mut images, || {
                attempts += 1;
                (attempts >= 3).then_some(()).ok_or("pressure")
            }),
            Ok(2)
        );
        assert_eq!(attempts, 3);
        assert!(images.contains_key("a"));
        assert!(!images.contains_key("b") && !images.contains_key("c"));
        assert!(images.contains_key("missing"));
    }

    #[test]
    fn unobserved_stamp_ties_break_in_key_order() {
        let mut p = ImageCachePolicy::default();
        let mut images = HashMap::from([
            ("z".into(), Some(Arc::new(1))),
            ("a".into(), Some(Arc::new(2))),
        ]);
        let mut attempts = 0;
        assert_eq!(
            p.admit(&mut images, || {
                attempts += 1;
                (attempts > 1).then_some(()).ok_or("pressure")
            }),
            Ok(1)
        );
        assert!(!images.contains_key("a"));
        assert!(images.contains_key("z"));
    }

    #[test]
    fn stage_held_image_is_never_evicted_even_when_older() {
        let mut p = ImageCachePolicy::default();
        let active = Arc::new(1u8);
        let held = active.clone();
        let mut images = HashMap::from([
            ("active".into(), Some(active)),
            ("idle".into(), Some(Arc::new(2))),
        ]);
        p.touch("active");
        p.touch("idle");
        let mut attempts = 0;
        assert_eq!(
            p.admit(&mut images, || {
                attempts += 1;
                (attempts > 1).then_some(()).ok_or("pressure")
            }),
            Ok(1)
        );
        assert_eq!(Arc::strong_count(&held), 2);
        assert!(images.contains_key("active"));
        assert!(!images.contains_key("idle"));
    }

    #[test]
    fn external_gpu_owner_can_keep_allocation_alive_so_probe_continues() {
        struct Image(Rc<()>);
        let first_gpu = Rc::new(());
        let retained_gpu = first_gpu.clone();
        let first_image = Image(first_gpu);
        assert_eq!(Rc::strong_count(&first_image.0), 2);
        let second_gpu = Rc::new(());
        let second_probe = Rc::downgrade(&second_gpu);
        let mut images = HashMap::from([
            ("first".into(), Some(Arc::new(first_image))),
            ("second".into(), Some(Arc::new(Image(second_gpu)))),
        ]);
        let mut p = ImageCachePolicy::default();
        p.touch("first");
        p.touch("second");
        let mut attempts = 0;
        assert_eq!(
            p.admit(&mut images, || {
                attempts += 1;
                second_probe
                    .upgrade()
                    .is_none()
                    .then_some(())
                    .ok_or("still held")
            }),
            Ok(2)
        );
        assert_eq!(attempts, 3); // initial, after still externally held first, after second
        assert_eq!(Rc::strong_count(&retained_gpu), 1);
        assert!(images.is_empty());
    }

    #[test]
    fn failed_admission_returns_last_error_without_negative_cache_and_can_retry() {
        let mut p = ImageCachePolicy::default();
        let active = Arc::new(1);
        let mut images = HashMap::from([
            ("active".into(), Some(active.clone())),
            ("idle".into(), Some(Arc::new(2))),
            ("absent".into(), None),
        ]);
        let mut attempts = 0;
        let outcome = p.admit(&mut images, || {
            attempts += 1;
            Err::<(), _>(attempts)
        });
        assert_eq!(outcome, Err(2));
        assert!(!images.contains_key("idle"));
        assert!(images.contains_key("active") && images.contains_key("absent"));
        assert_eq!(p.admit(&mut images, || Ok::<_, i32>(())), Ok(0));
        images.insert("idle".into(), Some(Arc::new(3)));
        assert_eq!(images.get("idle").unwrap().as_ref().map(|v| **v), Some(3));
    }

    #[test]
    fn retain_keys_discards_removed_stamp_without_touching_cache_entries() {
        let mut p = ImageCachePolicy::default();
        p.touch("gone");
        p.touch("live");
        p.retain_keys(|key| key == "live");
        assert!(!p.last_use.contains_key("gone"));
        assert!(p.last_use.contains_key("live"));
    }

}
