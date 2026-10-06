use super::*;

#[test]
fn joined_dirty_rectangles_preserve_holes_and_subtractions() {
    let mut dirty = Dirty::default();
    let mut expected = [[false; 48]; 32];
    for area in [
        Rect {
            left: 1,
            top: 1,
            width: 7,
            height: 5,
        },
        Rect {
            left: 8,
            top: 1,
            width: 8,
            height: 5,
        },
        Rect {
            left: 16,
            top: 1,
            width: 3,
            height: 7,
        },
        Rect {
            left: 1,
            top: 6,
            width: 15,
            height: 2,
        },
    ] {
        dirty.add(area);
        for y in area.top..area.top + area.height as i32 {
            for x in area.left..area.left + area.width as i32 {
                expected[y as usize][x as usize] = true;
            }
        }
    }
    let removed = Rect {
        left: 7,
        top: 2,
        width: 4,
        height: 4,
    };
    dirty.remove(removed);
    for y in removed.top..removed.top + removed.height as i32 {
        for x in removed.left..removed.left + removed.width as i32 {
            expected[y as usize][x as usize] = false;
        }
    }
    for (y, row) in expected.iter().enumerate() {
        for (x, &value) in row.iter().enumerate() {
            assert_eq!(
                dirty.intersects(Rect {
                    left: x as i32,
                    top: y as i32,
                    width: 1,
                    height: 1
                }),
                value
            );
        }
    }
}

#[test]
#[ignore = "manual CPU benchmark for contiguous work-surface writes"]
fn dirty_write_benchmark() {
    use std::{hint::black_box, time::Instant};
    let mut samples = Vec::new();
    for _ in 0..9 {
        let start = Instant::now();
        for _ in 0..300 {
            let mut dirty = Dirty::default();
            for x in 0..960 {
                dirty.add(black_box(Rect {
                    left: x,
                    top: 0,
                    width: 1,
                    height: 16,
                }));
            }
            black_box(dirty.transfers());
        }
        samples.push(start.elapsed().as_secs_f64() * 1000.);
    }
    samples.sort_by(f64::total_cmp);
    println!(
        "dirty_300x960: median_ms={:.3} range_ms={:.3}..{:.3}",
        samples[4], samples[0], samples[8]
    );
}
