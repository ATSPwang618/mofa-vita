use krkr_assets::name;

// Independent component-stack oracle, including TJS's N-1 parent rule.
fn archive_path(path: &[u16]) -> Option<Vec<u16>> {
    let path = name::fold(path);
    let mut parts = Vec::new();
    let mut trailing = path.last() == Some(&47);
    for part in path.split(|&u| u == 47).filter(|p| !p.is_empty()) {
        if part.iter().all(|&u| u == 46) {
            for _ in 1..part.len() {
                parts.pop()?;
            }
            trailing = true;
        } else {
            parts.push(part);
            trailing = path.last() == Some(&47);
        }
    }
    let mut result = parts.join(&47);
    if trailing && !result.is_empty() {
        result.push(47);
    }
    Some(result)
}

#[test]
fn path_compression_preserves_root_dot_and_utf16_semantics() {
    let components = ["", ".", "..", "...", "AB", ".hidden", "日本", "four...."].map(name::units);
    for mut choice in 0..4096 {
        let mut path = Vec::new();
        for _ in 0..4 {
            path.extend_from_slice(&components[choice % components.len()]);
            path.push(if choice & 1 == 0 { 47 } else { 92 });
            choice /= components.len();
        }
        for suffix in [&[][..], &[0xd800][..], &[0, 65][..]] {
            let mut input = path.clone();
            input.extend_from_slice(suffix);
            let expected = archive_path(&input);
            assert_eq!(name::archive(&input).ok(), expected);
            let absolute = [name::units("file://root/"), input].concat();
            assert_eq!(
                name::normalize(&absolute, &name::units("file://root/")).ok(),
                expected.map(|p| [name::units("file://root/"), p].concat())
            );
        }
    }
}
