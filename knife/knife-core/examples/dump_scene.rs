//! DEV: dump one allscr scene as text so the grade/soft-clip compatibility can be scanned.
fn main() {
    let mut args = std::env::args().skip(1);
    let path = args.next().expect("allscr.mrg path");
    let index: usize = args.next().unwrap_or_else(|| "0".into()).parse().unwrap();
    let data = std::fs::read(&path).expect("read allscr");
    let archive = knife_core::format::script::SceneArchive::parse(&data).expect("parse allscr");
    eprintln!("scenes={}", archive.scene_count());
    let scene = archive.scene(index).expect("scene index");
    print!("{}", String::from_utf8_lossy(scene));
}