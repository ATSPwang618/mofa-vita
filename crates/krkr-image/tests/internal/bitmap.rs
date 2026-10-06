use super::*;

#[test]
fn indexed_palette_matches_stock_quantization_at_every_phase() {
    const DITHER: [[u16; 4]; 4] = [[0, 12, 2, 14], [8, 4, 10, 6], [3, 15, 1, 13], [11, 7, 9, 5]];
    for y in 0..4 {
        for x in 0..4 {
            for channel in 0..3 {
                for value in 0..=255 {
                    let mut pixel = [0; 4];
                    pixel[channel] = value;
                    let (count, weight, threshold) = match channel {
                        0 => (5, 1, DITHER[(x + 1) % 2][(y + 1) % 2]),
                        1 => (6, 6, DITHER[x][(y + 1) % 2]),
                        _ => (5, 42, DITHER[x][y]),
                    };
                    let scaled = u16::from(value) * count;
                    let expected =
                        (scaled / 255 + u16::from(scaled % 255 * 16 > threshold * 255)) * weight;
                    assert_eq!(u16::from(index(&pixel, x, y)), expected);
                }
            }
        }
    }
}

#[test]
fn bitmap_rows_keep_bottom_up_channels_alpha_and_zero_padding() {
    for width in 1..=17 {
        let size = Size { width, height: 7 };
        let input: Vec<u8> = (0..size.rgba_bytes().unwrap())
            .map(|i| (i * 73) as u8)
            .collect();
        for depth in [BitmapDepth::Indexed, BitmapDepth::Rgb, BitmapDepth::Rgba] {
            let channels = match depth {
                BitmapDepth::Indexed => 1,
                BitmapDepth::Rgb => 3,
                BitmapDepth::Rgba => 4,
            };
            let mut output = Vec::new();
            encode(&mut output, &input, size, depth).unwrap();
            let offset = u32::from_le_bytes(output[10..14].try_into().unwrap()) as usize;
            let stride = (width as usize * channels).div_ceil(4) * 4;
            for y in 0..7usize {
                let row = &output[offset + (6 - y) * stride..][..stride];
                for x in 0..width as usize {
                    let source = &input[(y * width as usize + x) * 4..][..4];
                    let got = &row[x * channels..][..channels];
                    match depth {
                        BitmapDepth::Indexed => assert_eq!(got, [index(source, x, y)]),
                        BitmapDepth::Rgb => assert_eq!(got, [source[2], source[1], source[0]]),
                        BitmapDepth::Rgba => {
                            assert_eq!(got, [source[2], source[1], source[0], source[3]])
                        }
                    }
                }
                assert!(row[width as usize * channels..].iter().all(|&b| b == 0));
            }
        }
    }
}
