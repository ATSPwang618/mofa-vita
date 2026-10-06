use super::*;
use std::io::Cursor;

#[derive(Default)]
struct Output {
    data: Cursor<Vec<u8>>,
    writes: usize,
}
impl Write for Output {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.writes += 1;
        self.data.write(bytes)
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
impl Seek for Output {
    fn seek(&mut self, from: SeekFrom) -> io::Result<u64> {
        self.data.seek(from)
    }
}

#[test]
fn bmp_thumbnail_writes_once_with_accounted_bounded_buffer() {
    let size = Size {
        width: 301,
        height: 171,
    };
    let format = Format::Bmp(BitmapDepth::Rgba);
    let input_len = size.rgba_bytes().unwrap();
    let scratch = format.scratch(size).unwrap();
    let budget = Budget::new(input_len + scratch);
    let mut main = Bytes::zeroed(input_len, &budget).unwrap();
    for (index, value) in main.as_mut_slice().iter_mut().enumerate() {
        *value = index as u8;
    }
    let last_row = main.as_slice()[input_len - size.width as usize * 4..].to_vec();
    let permit = budget.reserve(scratch).unwrap();
    let mut output = Output::default();
    encode_buffered(
        &mut output,
        format,
        main,
        size,
        vec![],
        &AtomicBool::new(false),
        format.buffer_size(size),
    )
    .unwrap();
    assert_eq!(output.writes, 1);
    let bmp = output.data.into_inner();
    assert_eq!(&bmp[..2], b"BM");
    assert_eq!(bmp.len(), input_len + 54);
    for (rgba, bgra) in last_row
        .as_chunks::<4>()
        .0
        .iter()
        .zip(bmp[54..].as_chunks::<4>().0.iter())
    {
        assert_eq!(*bgra, [rgba[2], rgba[1], rgba[0], rgba[3]]);
    }
    drop(permit);
    assert_eq!(budget.used(), 0);
    assert_eq!(
        format.buffer_size(Size {
            width: 4096,
            height: 4096
        }),
        256 * 1024
    );
}

#[test]
fn cancelled_thumbnail_does_not_reach_the_underlying_writer() {
    let size = Size {
        width: 301,
        height: 171,
    };
    let budget = Budget::new(1024 * 1024);
    let main = Bytes::zeroed(size.rgba_bytes().unwrap(), &budget).unwrap();
    let mut output = Output::default();
    assert!(
        encode_buffered(
            &mut output,
            Format::Bmp(BitmapDepth::Rgba),
            main,
            size,
            vec![],
            &AtomicBool::new(true),
            Format::Bmp(BitmapDepth::Rgba).buffer_size(size)
        )
        .is_err()
    );
    assert_eq!(output.writes, 0);
    assert_eq!(budget.used(), 0);
}
