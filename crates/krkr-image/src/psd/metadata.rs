use super::{
    Document, Error, Layer, Meta, Result, blend,
    descriptor::{descriptor, int, map},
    read_bounds, slice,
};

pub(super) fn layer(layer: &mut Layer, data: &[u8]) -> Result<()> {
    if data.is_empty() {
        return Ok(());
    }
    let mut r = slice(data);
    let mask_end = r.block_end()?;
    if mask_end != r.pos {
        let saved = r.end;
        r.end = mask_end;
        layer.mask.bounds = read_bounds(&mut r)?;
        layer.mask.default_color = r.u8()?;
        let flags = r.u8()?;
        if flags & 16 != 0 {
            let parameters = r.u8()?;
            if parameters & 1 != 0 {
                r.skip(1)?;
            }
            if parameters & 2 != 0 {
                r.skip(8)?;
            }
            if parameters & 4 != 0 {
                r.skip(1)?;
            }
            if parameters & 8 != 0 {
                r.skip(8)?;
            }
        }
        if r.remaining() >= 18 {
            r.u8()?;
            let background = r.u8()?;
            layer.mask.real = Some((read_bounds(&mut r)?, background));
        }
        r.end = saved;
        r.seek(mask_end)?;
    }
    let ranges_end = r.block_end()?;
    r.seek(ranges_end)?;
    let len = r.u8()? as usize;
    let bytes = r.bytes(len)?;
    // Unicode 'luni' takes precedence. For legacy names, accept UTF-8 from
    // krkr2 and Shift-JIS used by the original Japanese Windows plugin.
    let text = match std::str::from_utf8(&bytes) {
        Ok(text) => text.into(),
        Err(_) => encoding_rs::SHIFT_JIS.decode(&bytes).0,
    };
    layer.name = text.encode_utf16().take_while(|&u| u != 0).collect();
    r.skip(((4 - ((len + 1) % 4)) % 4) as u64)?;
    while r.remaining() >= 12 {
        let sig = r.key()?;
        if sig != *b"8BIM" && sig != *b"8B64" {
            return Err(Error::Message("invalid PSD layer additional signature"));
        }
        let key = r.key()?;
        let end = r.block_end()?;
        let length = (end - r.pos) as usize;
        let data = r.bytes(length)?;
        if sig == *b"8BIM" {
            additional(layer, key, &data)?;
        }
        if length & 1 != 0 && r.remaining() != 0 {
            r.skip(1)?;
        }
    }
    Ok(())
}
fn additional(layer: &mut Layer, key: [u8; 4], data: &[u8]) -> Result<()> {
    let mut r = slice(data);
    match &key {
        b"luni" => {
            let name = r.unicode()?;
            if !name.is_empty() {
                layer.name = name;
            }
        }
        b"lyid" => layer.id = r.i32()?,
        b"iOpa" => layer.fill_opacity = r.u8()?,
        b"lsct" | b"lsdk" => {
            layer.kind = match r.u32()? {
                1 | 2 => 2,
                3 => 1,
                _ => 0,
            };
            if r.remaining() >= 8 {
                r.key()?;
                layer.blend = blend(r.key()?);
            }
        }
        b"grdm" | b"levl" | b"curv" | b"hue " | b"hue2" | b"blnc" | b"nvrt" | b"post" | b"thrs"
        | b"selc" | b"brit" | b"mixr" | b"clrL" | b"phfl" | b"blwh" | b"vibA" | b"expA" => {
            layer.kind = 3
        }
        b"SoCo" | b"GdFl" | b"PtFl" => layer.kind = 4,
        b"shmd" => {
            for _ in 0..r.count()? {
                if r.key()? != *b"8BIM" {
                    return Err(Error::Message("invalid PSD metadata signature"));
                }
                let key = r.key()?;
                r.skip(4)?;
                let end = r.block_end()?;
                let length = (end - r.pos) as usize;
                let data = r.bytes(length)?;
                if key == *b"cmls" {
                    let mut sub = slice(&data);
                    if sub.u32()? == 16 {
                        let d = descriptor(&mut sub, 0)?;
                        let mut enabled = 1;
                        for comp in d.get("layerSettings").list() {
                            let Some(id) = comp.get("compList").list().first() else {
                                continue;
                            };
                            let id = id.integer(-1);
                            enabled = comp.get("enab").integer(enabled);
                            let offsets = comp.get("Ofst");
                            layer.comps.insert(
                                id.to_string(),
                                map([
                                    ("id", int(id)),
                                    ("enable", int(i32::from(enabled != 0))),
                                    ("offset_x", int(offsets.get("Hrzn").integer(0))),
                                    ("offset_y", int(offsets.get("Vrtc").integer(0))),
                                ]),
                            );
                        }
                    }
                }
            }
        }
        _ => {} // Unknown tagged data has an explicit, checked byte boundary.
    }
    Ok(())
}
pub(super) fn resource(doc: &mut Document, id: u16, data: &[u8]) -> Result<()> {
    let mut r = slice(data);
    match id {
        1046 => {
            if r.u16()? > 256 {
                return Err(Error::Message("invalid PSD palette count"));
            }
        }
        1047 => {
            let index = r.u16()? as usize;
            if index < 256 {
                doc.palette[index][3] = 0;
            }
        }
        1032 => {
            r.u32()?;
            let horizontal = r.i32()?;
            let vertical = r.i32()?;
            let mut h = Vec::new();
            let mut v = Vec::new();
            for _ in 0..r.count()? {
                let location = int(r.i32()?);
                if r.u8()? == 0 {
                    v.push(location);
                } else {
                    h.push(location);
                }
            }
            doc.guides = map([
                ("horz_grid", int(horizontal)),
                ("vert_grid", int(vertical)),
                ("vertical", Meta::List(v)),
                ("horizontal", Meta::List(h)),
            ]);
        }
        1050 => {
            // Like the reference, expose the v6 slice record. v7/v8 carry only
            // descriptors and do not expose a slice result in this plugin API.
            if r.u32()? != 6 {
                return Ok(());
            }
            let left = r.i32()?;
            let top = r.i32()?;
            let right = r.i32()?;
            let bottom = r.i32()?;
            let name = Meta::Text(r.unicode()?);
            let mut slices = Vec::new();
            for _ in 0..r.count()? {
                let id = r.i32()?;
                let group = r.i32()?;
                let origin = r.i32()?;
                let associated = if origin == 1 { r.i32()? } else { -1 };
                let name = Meta::Text(r.unicode()?);
                let kind = r.i32()?;
                let left = r.i32()?;
                let top = r.i32()?;
                let right = r.i32()?;
                let bottom = r.i32()?;
                let url = Meta::Text(r.unicode()?);
                let target = Meta::Text(r.unicode()?);
                let message = Meta::Text(r.unicode()?);
                let alt = Meta::Text(r.unicode()?);
                let html = r.u8()? != 0;
                let cell = Meta::Text(r.unicode()?);
                let h = r.i32()?;
                let v = r.i32()?;
                let color = r.i32()?;
                slices.push(map([
                    ("id", int(id)),
                    ("group_id", int(group)),
                    ("origin", int(origin)),
                    ("type", int(kind)),
                    ("left", int(left)),
                    ("top", int(top)),
                    ("right", int(right)),
                    ("bottom", int(bottom)),
                    ("color", int(color)),
                    ("cell_text_is_html", int(i32::from(html))),
                    ("horizontal_alignment", int(h)),
                    ("vertical_alignment", int(v)),
                    ("associated_layer_id", int(associated)),
                    ("name", name),
                    ("url", url),
                    ("target", target),
                    ("message", message),
                    ("alt_tag", alt),
                    ("cell_text", cell),
                ]));
            }
            doc.slices = map([
                ("top", int(top)),
                ("left", int(left)),
                ("bottom", int(bottom)),
                ("right", int(right)),
                ("name", name),
                ("slices", Meta::List(slices)),
            ]);
        }
        1065 => {
            if r.u32()? != 16 {
                return Err(Error::Message("unsupported PSD layer comp version"));
            }
            let d = descriptor(&mut r, 0)?;
            let mut comps = Vec::new();
            for comp in d.get("list").list() {
                if matches!(comp.get("compID"), Meta::Void) {
                    continue;
                }
                let flags = comp.get("capturedInfo").integer(0);
                comps.push(map([
                    ("id", comp.get("compID").clone()),
                    ("record_visibility", int(i32::from(flags & 1 != 0))),
                    ("record_position", int(i32::from(flags & 2 != 0))),
                    ("record_appearance", int(i32::from(flags & 4 != 0))),
                    ("name", comp.get("Nm  ").text()),
                    ("comment", comp.get("comment").text()),
                ]));
            }
            if !comps.is_empty() {
                doc.comps = map([
                    ("last_applied_id", int(d.get("lastAppliedComp").integer(-1))),
                    ("comps", Meta::List(comps)),
                ]);
            }
        }
        _ => {}
    }
    Ok(())
}
