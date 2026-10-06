use krkr_protocol::{
    budget::Budget,
    graphics::{ImageRef, Size},
    hit::{Data, Plane},
    image_cache::{Cache, Entry, Key},
    pixels::Bytes,
};
use std::sync::Arc;

#[test]
fn alpha_compaction_transfers_budget_and_keeps_uniform_planes_free() {
    let source = Budget::new(1024);
    let target = Budget::new(1024);
    let size = Size {
        width: 3,
        height: 1,
    };
    let mut pixels = Bytes::zeroed(12, &source).unwrap();
    pixels
        .as_mut_slice()
        .copy_from_slice(&[1, 2, 3, 0, 4, 5, 6, 127, 7, 8, 9, 255]);
    let plane = Plane::from_scaled_pixels(
        Size {
            width: 6,
            height: 2,
        },
        size,
        pixels,
        4,
        &target,
    )
    .unwrap();
    assert_eq!(source.used(), 0);
    assert_eq!(target.used(), 3);
    for (x, expected) in [0, 0, 127, 127, 255, 255].into_iter().enumerate() {
        assert_eq!(plane.sample(x as i64, 1), expected);
    }
    drop(plane);
    assert_eq!(target.used(), 0);
    let plane = Plane::from_pixels(size, Bytes::zeroed(12, &source).unwrap(), 4, &target).unwrap();
    assert!(matches!(plane.data, Data::Uniform(0)));
    assert_eq!((source.used(), target.used()), (0, 0));
    let mut pixels = Bytes::zeroed(12, &source).unwrap();
    pixels.as_mut_slice()[7] = 1;
    assert!(Plane::from_pixels(size, pixels, 4, &Budget::new(0)).is_err());
    assert_eq!(source.used(), 0);
}

#[test]
fn image_lru_replacement_eviction_and_generation_keep_ownership() {
    let cache = Cache::new(1 << 20);
    let key = |n: u16| Key {
        names: [Some(vec![n]), None, None, None],
        color_key: 0,
        rule_size: None,
    };
    let mut ids = slotmap::SlotMap::with_key();
    let entry = |image| Entry {
        image,
        size: Size {
            width: 1,
            height: 1,
        },
        tags: Arc::default(),
        bytes: 4,
    };
    let a = ImageRef {
        id: ids.insert(()),
        lifetime: Arc::default(),
    };
    let b = ImageRef {
        id: ids.insert(()),
        lifetime: Arc::default(),
    };
    let c = ImageRef {
        id: ids.insert(()),
        lifetime: Arc::default(),
    };
    let generation = cache.generation();
    cache.insert(key(1), entry(a.clone()), generation);
    cache.insert(key(2), entry(b.clone()), generation);
    assert_eq!(cache.get(&key(1)).unwrap().image.id, a.id);
    cache.insert(key(3), entry(c.clone()), generation);
    assert!(!cache.evict_where(|entry| entry.bytes > 4));
    assert!(cache.evict_where(|entry| entry.image.id == c.id));
    assert!(cache.get(&key(3)).is_none());
    // A selective removal must leave the existing LRU order intact.
    assert!(cache.evict());
    assert!(cache.get(&key(2)).is_none());
    cache.insert(key(1), entry(c.clone()), generation);
    assert_eq!(cache.get(&key(1)).unwrap().image.id, c.id);
    assert!(cache.evict());
    assert!(!cache.evict());
    cache.clear();
    cache.insert(key(1), entry(a), generation);
    assert!(cache.get(&key(1)).is_none());
}
