"""Generate a standalone LUT exporter from the production Rust grade math.

Only asset/cache/performance integrations are stubbed; grade tables and math are
copied verbatim, with a source hash. Compile the emitted source with rustc, then
feed lines: grade bright_bits_hex contrast_bits_hex. Output is one binary stream
of KGL1 records. This avoids independently reimplementing float32 color math.
"""
import argparse
import hashlib
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]


def generate():
    path = ROOT / 'knife/knife/src/grade.rs'
    data = path.read_bytes()
    source = data.decode('utf8')
    needle = '#[path = "grade_cache.rs"]\nmod cache;'
    source = source.replace('\r\n', '\n')
    if source.count(needle) != 1:
        raise ValueError('Production cache boundary changed; audit exporter')
    source = source.replace(needle, 'mod cache { pub fn apply(_: &super::Bake, _: &mut [u8], _: bool) -> bool { false } }')
    source = source.replace('#[cfg(any(test, feature = "offline-grade-tool"))]', '')
    # Keep inner module docs first; inject the dependency import before the cache module.
    source = source.replace('mod cache {', 'use crate::knife_perf;\nmod cache {', 1)
    prelude = '''#![allow(dead_code)]
mod assets { pub fn witch() -> bool { true } }
mod knife_perf { pub mod optimizations {
    pub const GRADE_CACHE:u64=0;
    pub fn enabled(_:u64)->bool { false }
} }
mod grade {
'''
    main = r'''
}
fn main() -> std::io::Result<()> {
    use std::io::{BufRead, Write};
    let mut out=std::io::BufWriter::new(std::io::stdout().lock());
    for line in std::io::stdin().lock().lines() {
        let line=line?;
        let fields:Vec<_>=line.split_whitespace().collect();
        assert_eq!(fields.len(),3);
        let g:i32=fields[0].parse().unwrap();
        let b=u32::from_str_radix(fields[1],16).unwrap();
        let c=u32::from_str_radix(fields[2],16).unwrap();
        assert!((0..=82).contains(&g));
        let bake=grade::Bake { grade:g,bright:f32::from_bits(b),contrast:f32::from_bits(c),raw:false };
        assert!(bake.bright.is_finite() && bake.contrast.is_finite());
        let (grey,lut)=bake.witch_vita_lut();
        out.write_all(b"KGL1")?;
        for v in [g as u32,b,c,grey as u32,lut.len() as u32] { out.write_all(&v.to_le_bytes())?; }
        for row in lut { out.write_all(&row)?; }
    }
    out.flush()
}
'''
    return '// grade.rs SHA256: '+hashlib.sha256(data).hexdigest()+'\n'+prelude+source+main


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args()
    args.output.write_text(generate(), encoding='utf8')
