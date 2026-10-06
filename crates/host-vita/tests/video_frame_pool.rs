#[path = "../src/video_frame_pool.rs"]
mod pool;
use krkr_protocol::{budget::Budget, graphics::Size, pixels::Yuv420};
use pool::FramePool;
use std::sync::Arc;

const SIZE: Size = Size {
    width: 960,
    height: 544,
};

#[test]
fn live_presentation_is_not_overwritten_and_released_storage_is_reused() {
    let budget = Budget::new(8 * 1024 * 1024);
    let mut pool = FramePool::default();
    let mut first = pool.acquire(SIZE, &budget).unwrap();
    Arc::get_mut(&mut first)
        .unwrap()
        .data
        .as_mut_slice()
        .fill(11);
    let allocation = first.data.as_slice().as_ptr();
    pool.retain(&first);
    let second = pool.acquire(SIZE, &budget).unwrap();
    assert_ne!(second.data.as_slice().as_ptr(), allocation);
    pool.retain(&second);
    assert!(first.data.as_slice().iter().all(|&byte| byte == 11));
    drop(first);
    let mut recycled = pool.acquire(SIZE, &budget).unwrap();
    assert_eq!(recycled.data.as_slice().as_ptr(), allocation);
    Arc::get_mut(&mut recycled)
        .unwrap()
        .data
        .as_mut_slice()
        .fill(22);
    assert!(second.data.as_slice().iter().all(|&byte| byte == 0));
    assert_eq!(budget.used(), Yuv420::byte_len(SIZE).unwrap() * 2);
    pool.retain(&recycled);
    drop(recycled);
    drop(second);
    drop(pool);
    assert_eq!(budget.used(), 0);
}

#[test]
fn retained_storage_is_bounded_even_with_long_lived_consumers() {
    let length = Yuv420::byte_len(SIZE).unwrap();
    let budget = Budget::new(length * 12);
    let mut pool = FramePool::default();
    let frames: Vec<_> = (0..12)
        .map(|_| {
            let frame = pool.acquire(SIZE, &budget).unwrap();
            pool.retain(&frame);
            frame
        })
        .collect();
    assert_eq!(budget.used(), length * 12);
    assert!(pool.acquire(SIZE, &budget).is_err());
    drop(frames);
    assert_eq!(budget.used(), length * 8);
    let frame = pool.acquire(SIZE, &budget).unwrap();
    assert_eq!(budget.used(), length * 8);
    drop(frame);
    drop(pool);
    assert_eq!(budget.used(), 0);
}

#[test]
fn weak_observers_cannot_upgrade_to_an_overwritten_frame() {
    let budget = Budget::new(8 * 1024 * 1024);
    let mut pool = FramePool::default();
    let first = pool.acquire(SIZE, &budget).unwrap();
    let observer = Arc::downgrade(&first);
    pool.retain(&first);
    let allocation = first.data.as_slice().as_ptr();
    drop(first);
    let second = pool.acquire(SIZE, &budget).unwrap();
    assert_ne!(second.data.as_slice().as_ptr(), allocation);
    assert_eq!(
        observer.upgrade().unwrap().data.as_slice().as_ptr(),
        allocation
    );
    drop(observer);
    let mut recycled = pool.acquire(SIZE, &budget).unwrap();
    assert_eq!(recycled.data.as_slice().as_ptr(), allocation);
    assert!(Arc::get_mut(&mut recycled).is_some());
}
