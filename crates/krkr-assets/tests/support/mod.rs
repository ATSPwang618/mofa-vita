use std::io::Write;
pub fn chunk(tag: &[u8; 4], data: &[u8]) -> Vec<u8> {
    [tag.as_slice(), &(data.len() as u64).to_le_bytes(), data].concat()
}
pub fn zlib(data: &[u8]) -> Vec<u8> {
    let mut encoder = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
    encoder.write_all(data).unwrap();
    encoder.finish().unwrap()
}
/// Independent fixture assembler: mixed raw/compressed data, two index blocks,
/// optional MZ prefix. Segment offsets remain relative to the XP3 signature.
pub fn archive(name: &str, data: &[u8], compressed_index: bool, embedded: bool) -> Vec<u8> {
    archive_with_hash(name, data, compressed_index, embedded, 0x12345678)
}
pub fn archive_with_hash(
    name: &str,
    data: &[u8],
    compressed_index: bool,
    embedded: bool,
    hash: u32,
) -> Vec<u8> {
    let middle = data.len() / 2;
    let packed = zlib(&data[middle..]);
    let mut file = krkr_assets::xp3::SIGNATURE.to_vec();
    let index_at = 19 + middle + packed.len();
    file.extend_from_slice(&(index_at as u64).to_le_bytes());
    file.extend_from_slice(&data[..middle]);
    file.extend_from_slice(&packed);
    let mut segments = Vec::new();
    for (flags, offset, len, stored) in [
        (0u32, 19, middle, middle),
        (1, 19 + middle, data.len() - middle, packed.len()),
    ] {
        segments.extend_from_slice(&flags.to_le_bytes());
        for field in [offset, len, stored] {
            segments.extend_from_slice(&(field as u64).to_le_bytes());
        }
    }
    let name: Vec<_> = name.encode_utf16().collect();
    let mut info = 0x80000000u32.to_le_bytes().to_vec();
    info.extend_from_slice(&(data.len() as u64).to_le_bytes());
    info.extend_from_slice(&((middle + packed.len()) as u64).to_le_bytes());
    info.extend_from_slice(&(name.len() as u16).to_le_bytes());
    info.extend(name.iter().flat_map(|u| u.to_le_bytes()));
    let body = [
        chunk(b"adlr", &hash.to_le_bytes()),
        chunk(b"segm", &segments),
        chunk(b"info", &info),
    ]
    .concat();
    let index = [chunk(b"junk", b"skip me"), chunk(b"File", &body)].concat();
    // An empty first index followed by the real index exercises CONTINUE.
    file.push(0x80);
    file.extend_from_slice(&0u64.to_le_bytes());
    file.extend_from_slice(&((index_at + 17) as u64).to_le_bytes());
    file.push(u8::from(compressed_index));
    let packed = if compressed_index {
        zlib(&index)
    } else {
        index.clone()
    };
    file.extend_from_slice(&(packed.len() as u64).to_le_bytes());
    if compressed_index {
        file.extend_from_slice(&(index.len() as u64).to_le_bytes());
    }
    file.extend(packed);
    if embedded {
        let mut exe = vec![0; 32];
        exe[..2].copy_from_slice(b"MZ");
        exe.extend(file);
        exe
    } else {
        file
    }
}
