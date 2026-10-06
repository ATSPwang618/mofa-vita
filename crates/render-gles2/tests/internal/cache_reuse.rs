use crate::{Config, Gpu, adjust_cache, copy_cache, scene_damage::Version, test_support::Context};
use krkr_protocol::{budget::Budget, graphics::Size};

#[test]
fn cache_hits_reject_modified_outputs_and_targets() {
    let context = Context::new();
    let gpu = unsafe { Gpu::new(context.gl(), Config::default()).unwrap() };
    let size = Size {
        width: 4,
        height: 4,
    };
    let source = gpu.create_image(size, 0xff123456).unwrap();
    let target = gpu.create_image(size, 0xff987654).unwrap();
    let result = gpu.create_image(size, 0xff333333).unwrap();
    let budget = Budget::new(1 << 20);
    let mut adjust = adjust_cache::Cache::default();
    let mut copy = copy_cache::Cache::default();
    let key = copy_cache::Key {
        color: None,
        logical: size,
        stored: size,
        source: size.rect(),
        destination: size.rect(),
    };
    adjust.insert(
        adjust_cache::Key::Gray,
        size.rect(),
        Version::capture(&source).unwrap(),
        &result,
        &budget,
    );
    copy.insert(
        Version::capture(&source).unwrap(),
        Some(Version::capture(&target).unwrap()),
        key,
        &result,
        &budget,
    );
    assert!(
        adjust
            .get(&source, size.rect(), &adjust_cache::Key::Gray)
            .is_some()
    );
    assert!(copy.get(&source, &target, key).is_some());
    let texture = &target.main.as_ref().unwrap().tiles[0].texture;
    texture.generation.set(texture.generation.get() + 1);
    assert!(copy.get(&source, &target, key).is_none());
    let texture = &result.main.as_ref().unwrap().tiles[0].texture;
    texture.generation.set(texture.generation.get() + 1);
    assert!(
        adjust
            .get(&source, size.rect(), &adjust_cache::Key::Gray)
            .is_none()
    );
    assert_eq!(budget.used(), 0, "cache misses must release stale metadata");
}
