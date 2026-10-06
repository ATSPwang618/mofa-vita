fn main() {
    // Check LiveArea's image constraints on the host before packaging the VPK.
    for (name, size, transparency) in [
        ("icon0.png", (128, 128), false),
        ("pic0.png", (960, 544), false),
        ("livearea/contents/bg0.png", (840, 500), false),
        ("livearea/contents/startup.png", (280, 158), true),
    ] {
        let path = format!("runtime/sce_sys/{name}");
        println!("cargo:rerun-if-changed={path}");
        let bytes = std::fs::read(&path).unwrap_or_else(|e| panic!("{path}: {e}"));
        check_png(&bytes, size, transparency, name == "pic0.png")
            .unwrap_or_else(|error| panic!("{path}: {error}"));
    }
    println!("cargo:rerun-if-changed=runtime/sce_sys/livearea/contents/template.xml");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("vita") {
        for symbol in [
            "sceLibcHeapSize",
            "sceUserMainThreadStackSize",
            "_newlib_heap_size_user",
        ] {
            println!("cargo:rustc-link-arg=-Wl,--undefined={symbol}");
        }
    }
}

fn check_png(
    bytes: &[u8],
    size: (u32, u32),
    transparency: bool,
    full_palette: bool,
) -> Result<(), &'static str> {
    if bytes.len() > 420 * 1024 || !bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        return Err("expected PNG, no larger than 420 KiB");
    }
    let integer = |b: &[u8]| u32::from_be_bytes(b.try_into().unwrap());
    let mut chunks = &bytes[8..];
    let mut header = false;
    let mut palette = false;
    let mut pixels = false;
    while chunks.len() >= 12 {
        let length = usize::try_from(integer(&chunks[..4])).map_err(|_| "oversized PNG chunk")?;
        if length > chunks.len() - 12 {
            return Err("truncated PNG chunk");
        }
        let kind = &chunks[4..8];
        let data = &chunks[8..8 + length];
        if !header && kind != b"IHDR" {
            return Err("missing PNG header");
        }
        match kind {
            b"IHDR" => {
                if header || data.len() != 13 {
                    return Err("invalid PNG header");
                }
                if (integer(&data[..4]), integer(&data[4..8])) != size
                    || data[8..] != [8, 3, 0, 0, 0]
                {
                    return Err("expected native dimensions, 8-bit indexed color, no interlacing");
                }
                header = true;
            }
            b"PLTE" => {
                if palette || pixels || length == 0 || length > 768 || length % 3 != 0 {
                    return Err("invalid PNG palette");
                }
                if full_palette && length != 768 {
                    return Err("pic0.png requires exactly 256 palette entries");
                }
                palette = true;
            }
            b"tRNS" if !transparency => {
                return Err("LiveArea permits transparency only in startup.png; remove tRNS");
            }
            b"IDAT" => {
                if !palette {
                    return Err("missing PNG palette");
                }
                pixels = true;
            }
            b"IEND" => {
                return if length == 0 && pixels && chunks.len() == 12 {
                    Ok(())
                } else {
                    Err("invalid PNG ending")
                };
            }
            _ => {}
        }
        chunks = &chunks[12 + length..];
    }
    Err("missing PNG ending")
}
