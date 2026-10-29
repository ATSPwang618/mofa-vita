use std::{path::PathBuf, sync::Mutex};
use ab_glyph::{Font, FontArc, PxScale, point};
use knife_core::format::{bffnt, bfttf, bntx_decode, cff};
use crate::{FontInputs, Language, Renderer, prepare_page, text::{self, FontAtlas, Layout}};
use crate::{ProbeEvent, ProbeShader, ProbeBlend, ProbeFormat, Shade};

// The original engine's language/metric globals are single-threaded state.
static TEXT_STATE: Mutex<()> = Mutex::new(());
fn font_root() -> PathBuf {
    std::env::var_os("KNIFE_TEXT_ASSETS").map(PathBuf::from).unwrap_or_else(||
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../dist/Witch-on-the-Holy-Night-Chengkong-540p-Compact-x64/assets"))
}

#[test]
fn actual_game_text_module_builds_bilingual_vita_pages() {
    let _state = TEXT_STATE.lock().unwrap_or_else(|p| p.into_inner());
    let fonts = FontInputs::load(&font_root()).expect("real compact font inputs are required; never skip");
    let (mut renderer, control) = Renderer::probe_setup();
    let mut report = Vec::new();
    for language in [Language::English, Language::Chengkong] {
        let page = prepare_page(&renderer, &fonts, language).expect("original FontAtlas/layout must construct page");
        assert!(page.placed.len() > 20);
        assert!(page.quads.len() > 20);
        assert_eq!(page.atlas.tex.physical_size(), [2048, 2048]);
        assert_eq!(page.system_atlas.tex.physical_size(), [2048, 2048]);
        assert!(page.placed.iter().any(|c| c.size < page.layout.size));
        assert!(page.placed.iter().any(|c| c.italic > 0.0));
        assert!(page.placed.iter().any(|c| c.fill != 0));
        assert!(page.placed.iter().any(|c| c.run.is_some()));
        assert!(page.quads.iter().any(|q| matches!(q.shade, Shade::Text)));
        assert!(page.quads.iter().any(|q| matches!(q.shade, Shade::RimText)));
        let atlas_id = Renderer::probe_texture_id(&page.atlas.tex);
        let updates = control.borrow().events.iter().filter_map(|e| match e {
            ProbeEvent::Update { id, origin, size, stride } if *id == atlas_id =>
                Some(serde_json::json!({"origin":origin,"size":size,"stride":stride})),
            _ => None,
        }).collect::<Vec<_>>();
        assert!(!updates.is_empty());
        let target = renderer.create_scene_target(1920, 1080, "text contract scene");
        assert_eq!(target.tex.physical_size(), [960, 540]);
        let start = control.borrow().events.len();
        let mut encoder = renderer.encoder();
        renderer.try_draw(&mut encoder, &target, Some([0.015, 0.025, 0.05, 1.0]), &page.quads).unwrap();
        assert_eq!(control.borrow().events.len(), start, "draws must remain deferred");
        renderer.try_present(encoder, &target).unwrap();
        let trace = control.borrow();
        let draws = trace.events[start..].iter().filter_map(|e| match e {
            ProbeEvent::Draw { shader, blend, textures, vertices, .. } => Some((shader,blend,textures,vertices)), _ => None,
        }).collect::<Vec<_>>();
        assert_eq!(draws.len(), page.quads.len());
        for ((shader, blend, textures, vertices), quad) in draws.iter().zip(&page.quads) {
            assert_eq!(**blend, ProbeBlend::Alpha);
            assert_eq!(**shader, if quad.shade == Shade::Text { ProbeShader::Text } else { ProbeShader::RimText });
            assert_eq!(textures[0], Some(Renderer::probe_texture_id(&quad.tex)));
            assert_eq!(vertices.len(), 6, "unclipped glyph is two triangles");
            assert!((vertices[0].pos[0] - (quad.pos[0][0] / 1920.0 * 2.0 - 1.0)).abs() < 1e-6);
            assert!((vertices[0].pos[1] - (1.0 - quad.pos[0][1] / 1080.0 * 2.0)).abs() < 1e-6);
            assert_eq!(vertices[0].uv, quad.uv[0]);
            assert_eq!(vertices[0].color, quad.color);
            assert_eq!(vertices[0].haze, quad.haze);
        }
        assert!(trace.events[start..].iter().any(|e| matches!(e, ProbeEvent::Present(_))));
        let metrics = |m: knife_core::format::sfnt::SetMetrics| serde_json::json!({
            "height":m.height,"ascent":m.ascent,"line_feed":m.line_feed,"baseline":m.baseline});
        report.push(serde_json::json!({
            "language":language.label(),"placed":page.placed.len(),"quads":page.quads.len(),
            "text_draws":draws.iter().filter(|d| *d.0 == ProbeShader::Text).count(),
            "rim_text_draws":draws.iter().filter(|d| *d.0 == ProbeShader::RimText).count(),
            "message_metrics":metrics(page.metrics),"system_metrics":metrics(page.system_metrics),
            "atlas_updates":updates,"atlas_bytes":2048*2048,"format":"R8, linear sampler, non-sRGB",
            "characters":page.placed.iter().map(|c| serde_json::json!({"character":c.c.to_string(),
                "x":c.x,"baseline":c.baseline,"size":c.size,"order":c.order,"italic":c.italic,
                "fill":c.fill,"run":c.run})).collect::<Vec<_>>(),
        }));
        println!("LANGUAGE {} placed={} quads={} native_draws={} atlas_partial_updates={}",
            language.label(), page.placed.len(), page.quads.len(), draws.len(), updates.len());
    }
    if let Some(path) = std::env::var_os("KNIFE_TEXT_LAYOUT_REPORT") {
        std::fs::write(path, serde_json::to_vec_pretty(&serde_json::json!({
            "schema":"knife-vita-text-host-layout-v1", "authority":"strict CPU recorder; no GPU shader execution",
            "canvas_logical":[1920,1080],"canvas_physical":[960,540],"pages":report,
        })).unwrap()).unwrap();
    }
}

fn face(data: &[u8]) -> FontArc {
    let mut bytes = data.to_vec();
    cff::normalize(&mut bytes);
    FontArc::try_from_vec(bytes).unwrap()
}

// Independent ab_glyph output supplies expected coverage. The test then checks
// the facade's actual stored texture, all rows, and unchanged neighboring texels.
fn raster(font: &FontArc, c: char) -> ([u32; 2], Vec<u8>) {
    assert_ne!(font.glyph_id(c).0, 0, "reference glyph is required");
    let scale = PxScale::from(40.0 * font.height_unscaled() / font.units_per_em().unwrap());
    let outline = font.outline_glyph(font.glyph_id(c).with_scale_and_position(scale, point(0.0, 0.0))).unwrap();
    let bounds = outline.px_bounds();
    let size = [bounds.width().ceil() as u32 + 1, bounds.height().ceil() as u32 + 1];
    let mut data = vec![0; (size[0]*size[1]) as usize];
    outline.draw(|x,y,a| data[(y*size[0]+x) as usize] = (a.clamp(0.0,1.0)*255.0) as u8);
    assert!(data.iter().any(|v| *v > 0));
    (size,data)
}

fn compare_region(actual: &[u8], width: u32, origin: [u32;2], size: [u32;2], expected: &[u8]) -> Result<(), String> {
    if expected.len() != (size[0]*size[1]) as usize { return Err("expected size".into()); }
    for y in 0..size[1] {
        let from = ((origin[1]+y)*width+origin[0]) as usize;
        let row = &expected[(y*size[0]) as usize..((y+1)*size[0]) as usize];
        if actual.get(from..from+size[0] as usize) != Some(row) { return Err(format!("coverage differs at row {y}")); }
    }
    Ok(())
}

fn updates(control: &crate::ProbeControl, texture: usize) -> Vec<([u32;2],[u32;2],u32)> {
    control.borrow().events.iter().filter_map(|event| match event {
        ProbeEvent::Update{id,origin,size,stride} if *id==texture => Some((*origin,*size,*stride)), _=>None,
    }).collect()
}

#[test]
fn r8_glyph_partial_upload_matches_real_coverage_and_reuses_cache() {
    let _state = TEXT_STATE.lock().unwrap_or_else(|p| p.into_inner());
    let fonts = FontInputs::load(&font_root()).unwrap();
    let reference = face(&fonts.english);
    let (renderer,control) = Renderer::probe_setup();
    let mut atlas = FontAtlas::new(&renderer, fonts.english).unwrap();
    let id = Renderer::probe_texture_id(&atlas.tex);
    assert!(control.borrow().events.iter().any(|event| matches!(event,
        ProbeEvent::Create{id:created,size:[2048,2048],format:ProbeFormat::R8,srgb:false,linear:true,target:false} if *created==id)));
    assert!(renderer.probe_bytes(&atlas.tex).unwrap().iter().all(|v| *v==0));
    let first = atlas.quad(&renderer,'A',40.0,100.0,200.0,[1.0;4]).unwrap();
    let (size,data) = raster(&reference,'A');
    assert_eq!(updates(&control,id),vec![([0,0],size,size[0])]);
    let pixels = renderer.probe_bytes(&atlas.tex).unwrap();
    compare_region(&pixels,2048,[0,0],size,&data).unwrap();
    for (index,pixel) in pixels.iter().enumerate() {
        if index%2048>=size[0] as usize || index/2048>=size[1] as usize { assert_eq!(*pixel,0); }
    }
    assert_eq!(first.uv[0],[0.0,0.0]);
    assert_eq!(first.uv[2],[size[0] as f32/2048.0,size[1] as f32/2048.0]);
    let second = atlas.quad(&renderer,'A',80.0,300.0,450.0,[0.8,0.7,0.6,1.0]).unwrap();
    assert_eq!(updates(&control,id).len(),1,"display size/position must not rerasterize cache");
    assert_eq!(first.uv,second.uv);
    assert!(((second.pos[2][0]-second.pos[0][0])/(first.pos[2][0]-first.pos[0][0])-2.0).abs()<1e-6);
    atlas.quad(&renderer,'g',40.0,150.0,200.0,[1.0;4]).unwrap();
    let all = updates(&control,id);
    assert_eq!(all.len(),2);
    assert_eq!(all[1].0,[size[0]+2,0]);
    let pixels2 = renderer.probe_bytes(&atlas.tex).unwrap();
    compare_region(&pixels2,2048,[0,0],size,&data).unwrap();
    let (gsize,gdata) = raster(&reference,'g');
    compare_region(&pixels2,2048,all[1].0,gsize,&gdata).unwrap();
    assert!(atlas.quad(&renderer,' ',40.0,0.0,0.0,[1.0;4]).is_none());
    assert_eq!(updates(&control,id).len(),2);
    let mut corrupt = pixels2;
    let changed = gdata.iter().position(|v| *v>0).unwrap();
    let offset = (all[1].0[1] as usize+changed/gsize[0] as usize)*2048+all[1].0[0] as usize+changed%gsize[0] as usize;
    corrupt[offset] ^= 1;
    assert!(compare_region(&corrupt,2048,all[1].0,gsize,&gdata).is_err(),"negative control: one changed atlas texel must fail");
}

#[test]
fn actual_cff_font_repairs_required_accents_and_punctuation_without_placeholders() {
    let _state = TEXT_STATE.lock().unwrap_or_else(|p| p.into_inner());
    let fonts = FontInputs::load(&font_root()).unwrap();
    let mut bytes = fonts.english;
    assert_eq!(cff::normalize(&mut bytes),59);
    assert_eq!(cff::normalize(&mut bytes),0);
    let font = FontArc::try_from_vec(bytes).unwrap();
    for c in "Sometimes,it'snotourheroöà’”—".chars() {
        assert_ne!(font.glyph_id(c).0,0,"required glyph {c}");
        assert!(font.outline(font.glyph_id(c)).is_some(),"actual CFF outline {c}");
    }
}

#[test]
fn real_shared_font_priority_and_fallback_supply_actual_chinese_glyphs() {
    let _state = TEXT_STATE.lock().unwrap_or_else(|p| p.into_inner());
    let fonts = FontInputs::load(&font_root()).unwrap();
    assert_eq!(fonts.shared(Language::English).unwrap().fonts.len(),1);
    let shared = fonts.shared(Language::Chengkong).unwrap();
    assert_eq!(shared.fonts.len(),3);
    for bytes in &shared.fonts {
        assert!(knife_core::format::sfnt::metrics(bytes).is_some());
        FontArc::try_from_vec(bytes.clone()).unwrap();
    }
    let primary = face(&fonts.english);
    let fallback = face(&shared.fonts[0]);
    let c = "魔汉法夜龙龘".chars().find(|c| primary.glyph_id(*c).0==0 && fallback.glyph_id(*c).0!=0).unwrap();
    let (renderer,control) = Renderer::probe_setup();
    let mut atlas = FontAtlas::new(&renderer,fonts.english).unwrap();
    atlas.add_fallback(shared.fonts[0].clone()).unwrap();
    let id = Renderer::probe_texture_id(&atlas.tex);
    atlas.quad(&renderer,c,40.0,0.0,100.0,[1.0;4]).unwrap();
    let update = updates(&control,id);
    assert_eq!(update.len(),1);
    let (size,data) = raster(&fallback,c);
    assert_eq!(update[0].1,size);
    compare_region(&renderer.probe_bytes(&atlas.tex).unwrap(),2048,update[0].0,size,&data).unwrap();
}

#[test]
fn original_witch_shared_slot_metrics_use_the_first_priority_font() {
    let _state = TEXT_STATE.lock().unwrap_or_else(|p| p.into_inner());
    let fonts = FontInputs::load(&font_root()).unwrap();
    let ex = bffnt::Font::parse(&fonts.exfont).unwrap();
    let (renderer,_) = Renderer::probe_setup();
    for language in [Language::English, Language::Chengkong] {
        let shared = fonts.shared(language).unwrap();
        // Assets::font_metrics uses shared_fonts().first() for Witch's
        // variant-3 slot, while shared_font_atlas keeps all fallback faces.
        let first = knife_core::format::sfnt::metrics(&shared.fonts[0]).unwrap();
        let expected = knife_core::format::sfnt::pair_metrics(
            knife_core::format::sfnt::set_metrics(&[first],40.0).unwrap(),
            Some(knife_core::format::sfnt::SetMetrics {
                height:ex.height.into(), ascent:ex.ascent.into(),
                line_feed:ex.line_feed.into(), baseline:ex.baseline.into(),
            }));
        let actual = prepare_page(&renderer,&fonts,language).unwrap().system_metrics;
        assert_eq!((actual.height,actual.ascent,actual.line_feed,actual.baseline),
            (expected.height,expected.ascent,expected.line_feed,expected.baseline),
            "Witch's shared slot metrics must follow its first priority face");
    }
}

#[test]
fn actual_bffnt_sheet_is_upright_r8_and_dash_runs_share_the_bitmap() {
    let _state = TEXT_STATE.lock().unwrap_or_else(|p| p.into_inner());
    let fonts = FontInputs::load(&font_root()).unwrap();
    let font = bffnt::Font::parse(&fonts.exfont).unwrap();
    let rgba = bntx_decode::to_rgba8(&font.sheet).unwrap();
    let expected = rgba.chunks_exact(font.sheet.width as usize*4).rev()
        .flat_map(|row| row.iter().step_by(4).copied()).collect::<Vec<_>>();
    assert!(expected.iter().any(|v| *v>0));
    let (renderer,control) = Renderer::probe_setup();
    let mut atlas = FontAtlas::new(&renderer,fonts.chengkong).unwrap();
    atlas.load_exfont(&renderer,&fonts.exfont).unwrap();
    assert!(atlas.has_ex('œ') && atlas.has_ex('½'));
    text::set_language(2);
    let lay = layout(40.0,1000.0);
    let mut placed = Vec::new();
    text::layout(&atlas,&lay,&text::parse_markup("——"),0.0,100.0,0,&mut placed);
    assert_eq!(placed.len(),1);
    assert_eq!(placed[0].c,'œ');
    assert_eq!(placed[0].run,Some((2,2.0)));
    let q = atlas.placed_quad(&renderer,&placed[0],0.0,0.0,[1.0;4]).unwrap();
    assert_eq!(q.shade,Shade::Text);
    let id = Renderer::probe_texture_id(&q.tex);
    assert_eq!(q.tex.physical_size(),[font.sheet.width,font.sheet.height]);
    assert!(control.borrow().events.iter().any(|e| matches!(e,ProbeEvent::Create{id:created,format:ProbeFormat::R8,srgb:false,..} if *created==id)));
    assert_eq!(renderer.probe_bytes(&q.tex).unwrap(),expected);
    assert_eq!(updates(&control,id),vec![([0,0],[font.sheet.width,font.sheet.height],font.sheet.width)]);
}

fn layout(size: f32, right: f32) -> Layout {
    Layout { left:0.0,right,size,spacing:2.0,sx:1.0,line_height:80.0,ascent:40.0,ruby_y:-2.0 }
}

#[test]
fn original_english_break_tables_preserve_words_in_message_and_slot_layout() {
    let _state = TEXT_STATE.lock().unwrap_or_else(|p| p.into_inner());
    let fonts = FontInputs::load(&font_root()).unwrap();
    crate::probe::original_line_break().unwrap();
    text::set_language(1);
    let (renderer,_) = Renderer::probe_setup();
    let atlas = FontAtlas::new(&renderer,fonts.english).unwrap();
    let sample = "one two three four";
    let width = atlas.measure_slot("three ",[40.0,40.0],2.0)+2.0;
    let lay = layout(40.0,width);
    let mut placed = Vec::new();
    text::layout(&atlas,&lay,&text::parse_markup(sample),0.0,100.0,0,&mut placed);
    assert_eq!(placed.iter().map(|p| p.c).collect::<String>(),sample);
    assert!(placed.last().unwrap().baseline>100.0);
    let mut index=0;
    for word in sample.split(' ') {
        let count=word.chars().count();
        let baseline=placed[index].baseline;
        assert!(placed[index..index+count].iter().all(|p| p.baseline==baseline),"word cannot split: {word}");
        index+=count+1;
    }
    let lines=atlas.wrap_slot(sample,[40.0,40.0],2.0,width);
    assert_eq!(lines.concat(),sample);
    assert!(lines.len()>1);
    for word in sample.split(' ') { assert!(lines.iter().any(|l| l.contains(word))); }
}

#[test]
fn original_chinese_kinsoku_ruby_orders_and_italic_geometry_are_preserved() {
    let _state = TEXT_STATE.lock().unwrap_or_else(|p| p.into_inner());
    let fonts = FontInputs::load(&font_root()).unwrap();
    let (renderer,_) = Renderer::probe_setup();
    let mut page = prepare_page(&renderer,&fonts,Language::Chengkong).unwrap();
    let lay = layout(40.0,64.0);
    let mut placed=Vec::new();
    text::layout(&page.atlas,&lay,&text::parse_markup("魔法，之夜！"),0.0,100.0,0,&mut placed);
    assert_eq!(placed.iter().map(|p| p.c).collect::<String>(),"魔法，之夜！");
    assert_eq!(placed[2].baseline,placed[1].baseline,"comma overhangs; cannot begin its own line");
    assert!(placed[2].x>lay.right);
    assert!(placed[3].baseline>placed[2].baseline);
    assert_eq!(placed[5].baseline,placed[4].baseline,"closing punctuation overhangs");
    let mut ruby=Vec::new();
    let roomy=layout(40.0,1000.0);
    text::layout(&page.atlas,&roomy,&text::parse_markup("<魔法使|魔法师>"),0.0,100.0,5,&mut ruby);
    assert_eq!(ruby.len(),6);
    let base=ruby.iter().filter(|p| p.size==40.0).collect::<Vec<_>>();
    let small=ruby.iter().filter(|p| p.size<40.0).collect::<Vec<_>>();
    assert_eq!(base.len(),3); assert_eq!(small.len(),3);
    for (pc,base) in small.iter().zip(&base) {
        assert!((pc.size-40.0*0.51).abs()<1e-6);
        let expected=100.0-(page.metrics.baseline as f32+(1.0-0.51))-roomy.ruby_y;
        assert!((pc.baseline-expected).abs()<1e-5);
        assert_eq!(pc.order,base.order,"ruby reveal must follow its base character");
    }
    let tilted = *page.placed.iter().find(|p| p.italic>0.0 && p.size==page.layout.size && p.run.is_none()).unwrap();
    let mut upright=tilted; upright.italic=0.0;
    let a=page.atlas.placed_quad(&renderer,&upright,0.0,0.0,[1.0;4]).unwrap();
    let b=page.atlas.placed_quad(&renderer,&tilted,0.0,0.0,[1.0;4]).unwrap();
    assert_eq!(a.uv,b.uv);
    let k=tilted.size/40.0;
    let bottom=tilted.baseline+10.0*k;
    for (([x,y],[tx,ty]),index) in a.pos.into_iter().zip(b.pos).zip(0..) {
        let expected=x+tilted.italic*41.0*k*tilted.sx*(bottom-y)/(55.0*k);
        assert!((tx-expected).abs()<1e-5,"italic vertex {index}");
        assert_eq!(ty,y);
    }
}

#[test]
fn malformed_fonts_and_bad_atlas_regions_fail_before_native_updates() {
    let _state = TEXT_STATE.lock().unwrap_or_else(|p| p.into_inner());
    let fonts=FontInputs::load(&font_root()).unwrap();
    let (renderer,control)=Renderer::probe_setup();
    assert!(FontAtlas::new(&renderer,vec![0;32]).is_err());
    assert!(bfttf::decode(b"bad header").is_none());
    let truncated=bfttf::decode(&fonts.standard[..8]).unwrap();
    assert!(FontAtlas::new(&renderer,truncated).is_err());
    assert!(bffnt::Font::parse(&fonts.exfont[..8]).is_err());
    let mut atlas=FontAtlas::new(&renderer,fonts.english).unwrap();
    assert!(atlas.add_fallback(vec![0;32]).is_err());
    let before=control.borrow().events.len();
    let cases=[([2047,2047,2,1],1,vec![0;2]),([0,0,2,2],1,vec![0;3]),
        ([0,0,1,1],4,vec![0;4]),([u32::MAX,0,2,1],1,vec![0;2]),([0,0,0,1],1,vec![])];
    for (rect,bpp,data) in cases { assert!(renderer.try_write_region(&atlas.tex,rect,bpp,&data).is_err()); }
    assert_eq!(control.borrow().events.len(),before,"rejected regions cannot enter native backend");
}

#[test]
fn deferred_text_encoder_keeps_real_atlas_resources_until_retirement() {
    let _state = TEXT_STATE.lock().unwrap_or_else(|p| p.into_inner());
    let fonts=FontInputs::load(&font_root()).unwrap();
    let (mut renderer,control)=Renderer::probe_setup();
    let page=prepare_page(&renderer,&fonts,Language::Chengkong).unwrap();
    let ids=page.quads.iter().map(|q| Renderer::probe_texture_id(&q.tex)).collect::<std::collections::HashSet<_>>();
    assert!(ids.len()>=3,"message atlas, shared atlas, exfont sheet must all be used");
    let target=renderer.create_scene_target(1920,1080,"resource lifetime scene");
    let mut encoder=renderer.encoder();
    renderer.try_draw(&mut encoder,&target,Some([0.0,0.0,0.0,1.0]),&page.quads).unwrap();
    drop(page);
    for id in &ids { assert!(!control.borrow().events.iter().any(|e| matches!(e,ProbeEvent::Destroy(i) if i==id))); }
    drop(encoder);
    for id in &ids { assert_eq!(control.borrow().events.iter().filter(|e| matches!(e,ProbeEvent::Destroy(i) if i==id)).count(),1); }
    drop(target); drop(renderer);
    assert_eq!(control.borrow().events.iter().filter(|e| matches!(e,ProbeEvent::Close)).count(),1);
}
