#![allow(dead_code)]
use std::io::Write;
pub fn word(v: u16) -> Vec<u8> {
    v.to_be_bytes().to_vec()
}
pub fn dword(v: u32) -> Vec<u8> {
    v.to_be_bytes().to_vec()
}
pub fn block(data: &[u8]) -> Vec<u8> {
    [dword(data.len() as u32), data.to_vec()].concat()
}
pub fn unicode(s: &str) -> Vec<u8> {
    [
        dword(s.encode_utf16().count() as u32),
        s.encode_utf16().flat_map(u16::to_be_bytes).collect(),
    ]
    .concat()
}
fn id(s: &str) -> Vec<u8> {
    [
        dword(if s.len() == 4 { 0 } else { s.len() as u32 }),
        s.as_bytes().to_vec(),
    ]
    .concat()
}
pub fn desc(items: &[(&str, [u8; 4], Vec<u8>)]) -> Vec<u8> {
    let mut data = [unicode(""), id("null"), dword(items.len() as u32)].concat();
    for (key, kind, value) in items {
        data.extend(id(key));
        data.extend(kind);
        data.extend(value);
    }
    data
}
fn list(items: &[[u8; 4]], values: &[Vec<u8>]) -> Vec<u8> {
    let mut data = dword(items.len() as u32);
    for (kind, value) in items.iter().zip(values) {
        data.extend(kind);
        data.extend(value);
    }
    data
}
pub fn tag(key: &[u8; 4], data: &[u8]) -> Vec<u8> {
    let mut result = [b"8BIM".to_vec(), key.to_vec(), block(data)].concat();
    if !data.len().is_multiple_of(2) {
        result.push(0);
    }
    result
}
fn resource(id: u16, data: &[u8]) -> Vec<u8> {
    let mut result = [b"8BIM".to_vec(), word(id), vec![0, 0], block(data)].concat();
    if !data.len().is_multiple_of(2) {
        result.push(0);
    }
    result
}
fn compress(raw: &[u8], depth: u16, compression: u16, width: usize, height: usize) -> Vec<u8> {
    let row = (width * depth as usize).div_ceil(8);
    let mut encoded = raw.to_vec();
    if compression == 3 {
        for bytes in encoded.chunks_exact_mut(row) {
            match depth {
                8 => {
                    for i in (1..bytes.len()).rev() {
                        bytes[i] = bytes[i].wrapping_sub(bytes[i - 1]);
                    }
                }
                16 => {
                    for i in (2..bytes.len()).step_by(2).rev() {
                        let current = u16::from_be_bytes([bytes[i], bytes[i + 1]]);
                        let previous = u16::from_be_bytes([bytes[i - 2], bytes[i - 1]]);
                        bytes[i..i + 2]
                            .copy_from_slice(&current.wrapping_sub(previous).to_be_bytes());
                    }
                }
                32 => {
                    let raw = bytes.to_vec();
                    for x in 0..width {
                        for c in 0..4 {
                            bytes[c * width + x] = raw[x * 4 + c];
                        }
                    }
                    for i in (1..bytes.len()).rev() {
                        bytes[i] = bytes[i].wrapping_sub(bytes[i - 1]);
                    }
                }
                _ => panic!("prediction depth"),
            }
        }
    }
    match compression {
        0 => encoded,
        1 => {
            let mut counts = Vec::new();
            let mut rows = Vec::new();
            for line in raw.chunks_exact(row) {
                let mut packed = vec![128]; // PackBits no-op must not advance output.
                for part in line.chunks(128) {
                    packed.push((part.len() - 1) as u8);
                    packed.extend(part);
                }
                counts.extend(word(packed.len() as u16));
                rows.extend(packed);
            }
            assert_eq!(counts.len(), height * 2);
            [counts, rows].concat()
        }
        _ => {
            let mut z = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
            z.write_all(&encoded).unwrap();
            z.finish().unwrap()
        }
    }
}
fn samples(values: &[u8], depth: u16) -> Vec<u8> {
    values
        .iter()
        .flat_map(|&n| match depth {
            8 => vec![n],
            16 => (u16::from(n) * 257).to_be_bytes().to_vec(),
            32 => (f32::from(n) / 255.0).to_be_bytes().to_vec(),
            _ => unreachable!(),
        })
        .collect()
}
pub const RGBA: [u8; 24] = [
    255, 0, 0, 255, 0, 255, 0, 128, 0, 0, 255, 64, 255, 255, 255, 255, 40, 80, 120, 128, 10, 20,
    30, 255,
];
pub const MASK: [u8; 6] = [255, 128, 0, 64, 255, 128];
fn layer_record(
    name: &str,
    id: u32,
    section: Option<u32>,
    channels: &[(i16, Vec<u8>)],
    bounds: [i32; 4],
    metadata: bool,
) -> Vec<u8> {
    let mut extra = Vec::new();
    let mask = if channels.iter().any(|(id, _)| *id == -2) {
        [
            bounds
                .iter()
                .flat_map(|n| n.to_be_bytes())
                .collect::<Vec<_>>(),
            vec![0, 0, 0, 0],
        ]
        .concat()
    } else {
        Vec::new()
    };
    extra.extend(block(&mask));
    extra.extend(dword(0));
    extra.extend([3, b'o', b'l', b'd']);
    extra.extend(tag(b"luni", &unicode(name)));
    extra.extend(tag(b"lyid", &dword(id)));
    if let Some(section) = section {
        extra.extend(tag(
            b"lsct",
            &[dword(section), b"8BIMpass".to_vec()].concat(),
        ));
    }
    if metadata {
        extra.extend(tag(b"iOpa", &[170]));
        let offsets = desc(&[
            ("Hrzn", *b"long", dword(7)),
            ("Vrtc", *b"long", dword((-3i32) as u32)),
        ]);
        let comp = desc(&[
            ("compList", *b"VlLs", list(&[*b"long"], &[dword(9)])),
            ("Ofst", *b"Objc", offsets),
            ("enab", *b"bool", vec![0]),
        ]);
        let settings = [
            dword(16),
            desc(&[("layerSettings", *b"VlLs", list(&[*b"Objc"], &[comp]))]),
        ]
        .concat();
        let shmd = [
            dword(2),
            b"8BIMxxxx".to_vec(),
            vec![0; 4],
            block(&[1, 2, 3]),
            b"8BIMcmls".to_vec(),
            vec![0; 4],
            block(&settings),
        ]
        .concat();
        extra.extend(tag(b"shmd", &shmd));
    }
    let mut record: Vec<u8> = bounds.iter().flat_map(|n| n.to_be_bytes()).collect();
    record.extend(word(channels.len() as u16));
    for (id, data) in channels {
        record.extend(word(*id as u16));
        record.extend(dword(data.len() as u32));
    }
    record.extend(b"8BIMnorm");
    record.extend([200, 0, 0, 0]);
    record.extend(block(&extra));
    record
}
pub fn layered(depth: u16, compression: u16) -> Vec<u8> {
    let mut resources = resource(
        1032,
        &[
            dword(1),
            dword(576),
            dword(640),
            dword(2),
            dword(32),
            vec![0],
            dword(64),
            vec![1],
        ]
        .concat(),
    );
    let comp = desc(&[
        ("compID", *b"long", dword(9)),
        ("capturedInfo", *b"long", dword(7)),
        ("Nm  ", *b"TEXT", unicode("Scene")),
        ("comment", *b"TEXT", unicode("note")),
    ]);
    resources.extend(resource(
        1065,
        &[
            dword(16),
            desc(&[
                ("lastAppliedComp", *b"long", dword(9)),
                ("list", *b"VlLs", list(&[*b"Objc"], &[comp])),
            ]),
        ]
        .concat(),
    ));
    let mut slices = [
        dword(6),
        dword(1),
        dword(2),
        dword(4),
        dword(4),
        unicode("Slices"),
        dword(1),
        dword(5),
        dword(6),
        dword(1),
        dword(42),
        unicode("slice"),
        dword(0),
        dword(1),
        dword(2),
        dword(4),
        dword(4),
    ]
    .concat();
    for text in ["url", "target", "message", "alt"] {
        slices.extend(unicode(text));
    }
    slices.push(1);
    slices.extend(unicode("cell"));
    slices.extend(dword(2));
    slices.extend(dword(3));
    slices.extend([255, 10, 20, 30]);
    resources.extend(resource(1050, &slices));
    let mut channels = Vec::new();
    for (id, values) in (0..4)
        .map(|c| {
            (
                if c == 3 { -1 } else { c as i16 },
                RGBA.as_chunks::<4>()
                    .0
                    .iter()
                    .map(|p| p[c])
                    .collect::<Vec<_>>(),
            )
        })
        .chain([(-2, MASK.to_vec())])
    {
        let raw = samples(&values, depth);
        let data = [word(compression), compress(&raw, depth, compression, 3, 2)].concat();
        channels.push((id, data));
    }
    let bounds = [2, 1, 4, 4];
    let mut info = word((-5i16) as u16);
    info.extend(layer_record("end", 100, Some(3), &[], [0; 4], false));
    info.extend(layer_record("end inner", 101, Some(3), &[], [0; 4], false));
    info.extend(layer_record("彩/色", 42, None, &channels, bounds, true));
    info.extend(layer_record("Inner", 43, Some(1), &[], [0; 4], false));
    info.extend(layer_record("Group", 44, Some(1), &[], [0; 4], false));
    for (_, data) in &channels {
        info.extend(data);
    }
    if !info.len().is_multiple_of(2) {
        info.push(0);
    }
    let mut section = if depth == 8 {
        [block(&info), dword(0)].concat()
    } else {
        [
            dword(0),
            dword(0),
            tag(if depth == 16 { b"Lr16" } else { b"Lr32" }, &info),
        ]
        .concat()
    };
    if depth != 8 {
        while (section.len() - 8) % 4 != 0 {
            section.push(0);
        }
    }
    [
        header(4, 5, 6, depth, 3),
        block(&[]),
        block(&resources),
        block(&section),
        // Stored merged image deliberately differs from layer pixels.
        merged_image(4, 5, 6, depth, compression),
    ]
    .concat()
}
pub fn header(channels: u16, height: u32, width: u32, depth: u16, mode: u16) -> Vec<u8> {
    [
        b"8BPS".to_vec(),
        word(1),
        vec![0; 6],
        word(channels),
        dword(height),
        dword(width),
        word(depth),
        word(mode),
    ]
    .concat()
}
fn merged_image(
    channels: usize,
    height: usize,
    width: usize,
    depth: u16,
    compression: u16,
) -> Vec<u8> {
    let raw: Vec<u8> = (0..channels)
        .flat_map(|c| samples(&vec![[12, 34, 56, 255][c]; height * width], depth))
        .collect();
    [
        word(compression),
        compress(&raw, depth, compression, width, height * channels),
    ]
    .concat()
}
pub fn merged(
    mode: u16,
    depth: u16,
    channels: u16,
    size: (usize, usize),
    raw: &[u8],
    compression: u16,
    palette: &[u8],
) -> Vec<u8> {
    let (width, height) = size;
    [
        header(channels, height as u32, width as u32, depth, mode),
        block(palette),
        block(&[]),
        block(&[]),
        word(compression),
        compress(raw, depth, compression, width, height * channels as usize),
    ]
    .concat()
}
