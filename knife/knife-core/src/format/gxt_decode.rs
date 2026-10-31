//! Bounded CPU fallback for native encoded DXT1/DXT5 GXT textures.
//! Plain sprites retain the borrowed GPU upload path. This decoder is only for
//! operations which need pixels (STEC/SNX, language strips and raw sampling).
use std::fmt;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DecodeError { Header, Dimensions, Layout, Format, Length, Budget, Allocation }
impl fmt::Display for DecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { write!(f, "native GXT CPU decode: {self:?}") }
}
impl std::error::Error for DecodeError {}

struct Image<'a> { width: usize, height: usize, bw: usize, bh: usize, bpe: usize, bytes: &'a [u8] }
fn image(file: &[u8]) -> Result<Image<'_>, DecodeError> {
    if file.len() < 64 || file.len() > 16 * 1024 * 1024 { return Err(DecodeError::Length); }
    let word = |n| u32::from_le_bytes(file[n..n+4].try_into().unwrap());
    if word(0) != 0x0054_5847 || word(4) != 0x1000_0003 || word(8) != 1
        || word(20) != 0 || word(24) != 0 || word(28) != 0 || word(40) != u32::MAX
        || word(44) != 0 || file[61..64] != [0,0,0] { return Err(DecodeError::Header); }
    let offset = word(12) as usize; let length = word(16) as usize;
    if offset < 64 || offset % 16 != 0 || offset > file.len()
        || offset.checked_add(length) != Some(file.len()) || word(32) != word(12)
        || word(36) != word(16) || file[64..offset].iter().any(|&b| b != 0) { return Err(DecodeError::Length); }
    let width = u16::from_le_bytes(file[56..58].try_into().unwrap()) as usize;
    let height = u16::from_le_bytes(file[58..60].try_into().unwrap()) as usize;
    if width == 0 || height == 0 || width > 2048 || height > 2048 { return Err(DecodeError::Dimensions); }
    if file[60] != 1 || !matches!(word(48), 0 | 0xa000_0000)
        || (word(48) == 0 && (!width.is_power_of_two() || !height.is_power_of_two())) { return Err(DecodeError::Layout); }
    let bpe = match word(52) { 0x8500_0000 => 8, 0x8700_0000 => 16, _ => return Err(DecodeError::Format) };
    let bw = width.next_power_of_two().max(4) / 4;
    let bh = height.next_power_of_two().max(4) / 4;
    let payload = bw * bh * bpe;
    if length != (payload + 15) & !15 || file[offset+payload.min(length)..].iter().any(|&b| b != 0) {
        return Err(DecodeError::Length);
    }
    Ok(Image { width, height, bw, bh, bpe, bytes: &file[offset..offset+payload] })
}
fn spread(mut n: usize) -> usize {
    n = (n | (n << 8)) & 0x00ff_00ff; n = (n | (n << 4)) & 0x0f0f_0f0f;
    n = (n | (n << 2)) & 0x3333_3333; (n | (n << 1)) & 0x5555_5555
}
fn block_index(x: usize, y: usize, w: usize, h: usize) -> usize {
    // Vita block Morton order: X uses odd bits, Y even. The larger axis is
    // linear above the square min(w,h) tile; rectangular PoT images matter.
    let tile = w.min(h);
    ((x / tile) + (y / tile) * (w / tile)) * tile * tile
        + (spread(x % tile) << 1) + spread(y % tile)
}
fn rgb565(word: u16) -> [u8; 4] {
    let (r,g,b) = ((word >> 11) as u8, ((word >> 5) & 63) as u8, (word & 31) as u8);
    [(r << 3) | (r >> 2), (g << 2) | (g >> 4), (b << 3) | (b >> 2), 255]
}
fn decode_block(bytes: &[u8], dxt5: bool) -> [[u8; 4]; 16] {
    let color = if dxt5 { &bytes[8..] } else { bytes };
    let c0 = u16::from_le_bytes([color[0],color[1]]); let c1 = u16::from_le_bytes([color[2],color[3]]);
    let mut palette = [rgb565(c0),rgb565(c1),[0;4],[0;4]];
    if dxt5 || c0 > c1 {
        for c in 0..3 {
            palette[2][c] = ((2 * u16::from(palette[0][c]) + u16::from(palette[1][c])) / 3) as u8;
            palette[3][c] = ((u16::from(palette[0][c]) + 2 * u16::from(palette[1][c])) / 3) as u8;
        }
        palette[2][3] = 255; palette[3][3] = 255;
    } else {
        for c in 0..3 { palette[2][c] = ((u16::from(palette[0][c]) + u16::from(palette[1][c])) / 2) as u8; }
        palette[2][3] = 255; // DXT1 index3 is transparent black.
    }
    let indices = u32::from_le_bytes(color[4..8].try_into().unwrap());
    let mut out = std::array::from_fn(|i| palette[((indices >> (2*i)) & 3) as usize]);
    if dxt5 {
        let (a0,a1) = (u16::from(bytes[0]),u16::from(bytes[1]));
        let mut alpha = [bytes[0],bytes[1],0,0,0,0,0,255];
        if a0 > a1 {
            for i in 2..8 { alpha[i] = (((8-i) as u16 * a0 + (i-1) as u16 * a1) / 7) as u8; }
        } else {
            for i in 2..6 { alpha[i] = (((6-i) as u16 * a0 + (i-1) as u16 * a1) / 5) as u8; }
        }
        let packed = bytes[2..8].iter().enumerate().fold(0u64, |v,(i,b)| v | (u64::from(*b) << (8*i)));
        for i in 0..16 { out[i][3] = alpha[((packed >> (3*i)) & 7) as usize]; }
    }
    out
}

/// Decode only the selected physical rows. A strip never allocates the complete
/// tall image or PoT backing. The cap is checked before output allocation.
pub fn rgba8_rows(file: &[u8], top: usize, rows: usize, max_bytes: usize) -> Result<(u32,u32,Vec<u8>),DecodeError> {
    let image = image(file)?;
    if rows == 0 || top.checked_add(rows).is_none_or(|end| end > image.height) { return Err(DecodeError::Dimensions); }
    let len = image.width.checked_mul(rows).and_then(|n| n.checked_mul(4)).ok_or(DecodeError::Budget)?;
    if len > max_bytes { return Err(DecodeError::Budget); }
    let mut out = Vec::new(); out.try_reserve_exact(len).map_err(|_| DecodeError::Allocation)?; out.resize(len,0);
    for by in top/4..(top+rows).div_ceil(4) {
        for bx in 0..image.width.div_ceil(4) {
            let at = block_index(bx,by,image.bw,image.bh) * image.bpe;
            let block = decode_block(&image.bytes[at..at+image.bpe],image.bpe == 16);
            for y in 0..4 {
                let py = by*4+y;
                if py < top || py >= top+rows { continue; }
                for x in 0..4 {
                    let px = bx*4+x;
                    if px < image.width {
                        let dst = ((py-top)*image.width+px)*4;
                        out[dst..dst+4].copy_from_slice(&block[y*4+x]);
                    }
                }
            }
        }
    }
    Ok((image.width as u32,rows as u32,out))
}
pub fn rgba8(file: &[u8], max_bytes: usize) -> Result<(u32,u32,Vec<u8>),DecodeError> {
    let height = image(file)?.height; rgba8_rows(file,0,height,max_bytes)
}
pub fn dimensions(file: &[u8]) -> Result<(u32,u32),DecodeError> {
    let image = image(file)?; Ok((image.width as u32,image.height as u32))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture(w:u16,h:u16,dxt5:bool) -> Vec<u8> {
        let payload=(w.next_power_of_two().max(4) as usize)*(h.next_power_of_two().max(4) as usize)/(if dxt5{1}else{2});
        let mut v=vec![0;64+((payload+15)&!15)];
        for (at,n) in [(0,0x00545847),(4,0x10000003),(8,1),(12,64),(16,((payload+15)&!15) as u32),(32,64),(36,((payload+15)&!15) as u32),(40,u32::MAX),(48,0xa0000000),(52,if dxt5{0x87000000}else{0x85000000})] {v[at..at+4].copy_from_slice(&n.to_le_bytes());}
        v[56..58].copy_from_slice(&w.to_le_bytes());v[58..60].copy_from_slice(&h.to_le_bytes());v[60]=1;v
    }
    #[test]
    fn dxt1_four_colours_and_transparent_three_colour_mode() {
        let b=[0x00,0xf8,0xe0,0x07,0xe4,0xe4,0xe4,0xe4];let p=decode_block(&b,false);
        assert_eq!(&p[..4],&[[255,0,0,255],[0,255,0,255],[170,85,0,255],[85,170,0,255]]);
        let b=[0,0,0xff,0xff,0xe4,0,0,0];let p=decode_block(&b,false);
        assert_eq!(&p[..4],&[[0,0,0,255],[255,255,255,255],[127,127,127,255],[0,0,0,0]]);
    }
    #[test]
    fn dxt5_both_alpha_palettes_and_unconditional_four_colour_rgb() {
        for (ends,expected) in [([255,0],[255,0,218,182,145,109,72,36]),([0,255],[0,255,51,102,153,204,0,255])] {
            let mut b=[0;16];b[..2].copy_from_slice(&ends);
            let packed=(0..16).fold(0u64,|v,i|v|(((i%8) as u64)<<(3*i)));b[2..8].copy_from_slice(&packed.to_le_bytes()[..6]);
            b[10..12].copy_from_slice(&0xffffu16.to_le_bytes());b[12..].fill(0xff);
            let p=decode_block(&b,true);for i in 0..16 {assert_eq!(p[i][3],expected[i%8]);assert_eq!(p[i][0],170);}
        }
    }
    #[test]
    fn rectangular_morton_npot_and_row_crop_are_exact() {
        for (w,h) in [(9,5),(5,17),(32,8),(8,32)] {
            let mut v=fixture(w,h,false);let bw=w.next_power_of_two().max(4) as usize/4;let bh=h.next_power_of_two().max(4) as usize/4;
            for y in 0..bh {for x in 0..bw {let at=64+block_index(x,y,bw,bh)*8;let c=((x+y*bw) as u16)<<11;v[at..at+2].copy_from_slice(&c.to_le_bytes());}}
            let (_,_,all)=rgba8(&v,4*w as usize*h as usize).unwrap();
            for y in 0..h as usize {for x in 0..w as usize {assert_eq!(all[(y*w as usize+x)*4],rgb565(((x/4+(y/4)*bw) as u16)<<11)[0]);}}
            let (_,rows,crop)=rgba8_rows(&v,1,h as usize-2,usize::MAX).unwrap();assert_eq!(rows,h as u32-2);assert_eq!(crop,all[w as usize*4..(h as usize-1)*w as usize*4]);
        }
    }
    #[test]
    fn bounds_reject_invalid_headers_and_output_before_allocating() {
        let v=fixture(9,5,true);assert_eq!(rgba8(&v,9*5*4-1).unwrap_err(),DecodeError::Budget);
        assert_eq!(rgba8_rows(&v,usize::MAX,2,usize::MAX).unwrap_err(),DecodeError::Dimensions);
        for n in [0,63,v.len()-1] {assert!(rgba8(&v[..n],usize::MAX).is_err());}
        for at in [0,4,8,20,32,40,44,48,52,60] {let mut b=v.clone();b[at]^=1;assert!(rgba8(&b,usize::MAX).is_err(),"offset{at}");}
        let mut b=v.clone();b[56..58].copy_from_slice(&2049u16.to_le_bytes());assert_eq!(rgba8(&b,usize::MAX).unwrap_err(),DecodeError::Dimensions);
    }
    #[test]
    fn three_real_codec9_failures_match_independent_dds_decoder() {
        let Some(root)=std::env::var_os("KNIFE_NATIVE_GXT_TEST_FIXTURES")else{return};
        for name in ["IMG1207_60_60","IMG0181_01_01","IMG1205_02_02"] {
            let root=std::path::Path::new(&root);let file=std::fs::read(root.join(format!("{name}.gxt"))).unwrap();
            let expected=std::fs::read(root.join(format!("{name}.rgba"))).unwrap();
            let (w,h,actual)=rgba8(&file,4*960*1080).unwrap();assert_eq!(actual,expected,"{name}");
            let top=h as usize/3;let rows=h as usize/2;
            let (_,_,strip)=rgba8_rows(&file,top,rows,w as usize*rows*4).unwrap();
            assert_eq!(strip,expected[top*w as usize*4..(top+rows)*w as usize*4],"{name} strip");
        }
    }
}
