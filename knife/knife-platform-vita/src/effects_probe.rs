//! Finite native-effect fixture. A separate host oracle checks the screenshot.
use std::{fs::{File, OpenOptions}, io::Write};
use knife_render_api::Vertex;
use crate::{data_path, gxm::{EffectBlend as B, EffectShader as S, Gxm, TextureFormat, TextureId}};

const COLOR: [f32;4] = [0.8,0.7,0.6,0.75];
const BASE: [f32;4] = [0.2/0.7,0.4/0.7,0.6/0.7,0.7];

fn vertices(rect: [f32;4], color: [f32;4]) -> [Vertex;6] {
    let corners = [[0.,0.],[1.,0.],[0.,1.],[0.,1.],[1.,0.],[1.,1.]];
    corners.map(|p| Vertex {
        pos: [(rect[0]+p[0]*rect[2])*(2./960.)-1., 1.-(rect[1]+p[1]*rect[3])*(2./540.)],
        uv: p, color, haze: [[0.;4];2], soft: [[0.;4];3],
    })
}
fn draw(g: &mut Gxm, rect: [f32;4], color: [f32;4], shader: S, blend: B,
    textures: [Option<&TextureId>;4], params: [f32;8]) -> Result<(),String> {
    g.draw_effect(&vertices(rect,color),shader,blend,textures,params)
}
fn record(log: &mut File, text: &str) -> Result<(),String> {
    writeln!(log,"{text}").and_then(|()| log.flush()).map_err(|e|e.to_string())?;
    // Publish to Vita3K's host FILE layer even when a later stage stalls.
    *log = OpenOptions::new().append(true).open(data_path("effects-probe.log")?)
        .map_err(|e|e.to_string())?;
    Ok(())
}
const GROUP_BLENDS: [B;9] = [B::Alpha,B::Add,B::Sub,B::Multiply,B::Screen,
    B::Opaque,B::Over,B::Footprint,B::MaskAlpha];

// These DEV pages deliberately use display captures. Vita3K can leave the CPU
// backing of a completed GPU target stale; equal CPU reads cannot prove parity.
fn group_page(g: &mut Gxm, log: &mut File, name: &str, opacity: f32,
    exhaustive: bool) -> Result<(),String> {
    let size = if exhaustive { 256usize } else { 64 };
    let mut pixels = Vec::with_capacity(size*size*4);
    let mut masks = Vec::with_capacity(size*size*4);
    let level = |n: usize| -> u8 {
        if exhaustive || n < 16 { n as u8 }
        else if n >= 48 { (n+192) as u8 }
        else { ((n-16)*7+16) as u8 }
    };
    for y in 0..size { for x in 0..size {
        let rgb = if exhaustive { [255;3] } else {
            [((x*73+y*19)%256) as u8,((x*17+y*61)%256) as u8,((x*43+y*11)%256) as u8]
        };
        pixels.extend_from_slice(&[rgb[0],rgb[1],rgb[2],level(x)]);
        masks.extend_from_slice(&[0,0,0,level(y)]);
    }}
    let texture = |g: &mut Gxm, data: Option<&[u8]>, side: usize| {
        g.create_texture_ex(data,side as u32,side as u32,(side*4) as u32,
            TextureFormat::Rgba8,true,true,true)
    };
    let pixel = texture(g,Some(&pixels),size)?;
    let mask = texture(g,Some(&masks),size)?;
    let scratch = texture(g,None,size)?;
    let canvas = g.create_texture_ex(None,960,540,3840,TextureFormat::Rgba8,true,true,true)?;
    let backdrop = g.create_texture_ex(None,960,540,3840,TextureFormat::Rgba8,true,true,true)?;
    g.copy_texture(&pixel,&scratch)?;
    g.begin_target(&scratch,None)?;
    draw(g,[0.,0.,960.,540.],[1.;4],S::Sprite,B::MaskAlpha,
        [Some(&mask),None,None,None],[0.;8])?;
    g.end_target()?;
    g.begin_target(&canvas,Some([0.23,0.41,0.67,0.7]))?;
    let count = if exhaustive { 1 } else { 33 };
    for case in 0..count {
        let (x,y) = if exhaustive { (32.,32.) } else {
            (16.+132.*(case%7) as f32,16.+68.*(case/7) as f32)
        };
        let left = [x,y,size as f32,size as f32];
        let right = [x+size as f32,y,size as f32,size as f32];
        let color = [1.,1.,1.,opacity];
        if case < GROUP_BLENDS.len() {
            let blend = if exhaustive { B::Opaque } else { GROUP_BLENDS[case] };
            draw(g,left,color,S::Sprite,blend,[Some(&scratch),None,None,None],[0.;8])?;
            draw(g,right,color,S::GroupComposite,blend,[Some(&pixel),Some(&mask),None,None],[0.;8])?;
        } else {
            g.end_target()?;
            g.copy_texture(&canvas,&backdrop)?;
            g.begin_target(&canvas,None)?;
            let mut params = [(case-8) as f32,0.,960.,540.,-1.,0.,0.,0.];
            draw(g,left,color,S::Ps,B::Opaque,[Some(&scratch),Some(&backdrop),None,None],params)?;
            params[7] = 1.;
            draw(g,right,color,S::Ps,B::Opaque,[Some(&pixel),Some(&backdrop),Some(&mask),None],params)?;
        }
    }
    // Make target alpha observable through premultiplication before scanout.
    // The white exhaustive page covers all 65,536 source/mask alpha pairs.
    draw(g,[924.,16.,24.,24.],[1.,0.,0.,1.],S::Fill,B::Opaque,[None;4],[0.;8])?;
    draw(g,[924.,48.,24.,24.],[0.,1.,0.,1.],S::Fill,B::Opaque,[None;4],[0.;8])?;
    g.end_target()?;
    g.begin_target(&backdrop,None)?;
    draw(g,[0.,0.,960.,540.],[1.;4],S::Sprite,B::Opaque,
        [Some(&canvas),None,None,None],[0.;8])?;
    g.end_target()?;
    g.present_canvas(&backdrop)?;
    record(log,&format!("GROUP_FRAME READY page={name} cases={count} alpha_levels={size} opacity={opacity}; actual displayed RGB comparison required"))?;
    std::thread::sleep(std::time::Duration::from_secs(5));
    record(log,&format!("GROUP_FRAME END page={name}"))?;
    for id in [&backdrop,&canvas,&scratch,&mask,&pixel] { g.destroy_texture(id)?; }
    Ok(())
}
pub fn run_effects_probe() -> Result<(),String> {
    crate::native::init_data_dir()?;
    std::fs::write(data_path("gxm-native.log")?,[]).map_err(|e|e.to_string())?;
    let mut log = File::create(data_path("effects-probe.log")?).map_err(|e|e.to_string())?;
    let result = checks(&mut log);
    if let Err(ref e) = result { record(&mut log,&format!("FAIL {e}"))?; }
    record(&mut log,if result.is_ok() { "RESULT PASS" } else { "RESULT FAIL" })?;
    result
}
fn checks(log: &mut File) -> Result<(),String> {
    record(log,"knife Vita DEV effects fixture v2; original WGSL oracle and old/fused group display comparisons are independent")?;
    let mut g = Gxm::new()?;
    let baseline = g.stats()?;
    record(log,&format!("GXM initialized; {baseline:?}"))?;
    let pixels = [128,64,200,128].repeat(4);
    let source = g.create_texture_ex(Some(&pixels),2,2,8,TextureFormat::Rgba8,false,false,false)?;
    let canvas = g.create_texture_ex(None,960,540,3840,TextureFormat::Rgba8,true,false,true)?;
    let backdrop = g.create_texture_ex(None,960,540,3840,TextureFormat::Rgba8,true,false,false)?;
    let atlas = g.create_texture_ex(None,2048,2048,2048,TextureFormat::R8,false,false,false)?;
    g.update_texture(&atlas,[16,32],[2,2],&[128;4],2)?;
    if g.update_texture(&atlas,[2048,2048],[1,1],&[1],1).is_ok() {
        return Err("out-of-bounds R8 atlas update accepted".into());
    }
    record(log,"PASS R8 2048x2048 atlas allocation, partial update and rejected out-of-bounds update")?;
    record(log,"OFFSCREEN BEGIN; checking/registering all sixteen offline GXP entries")?;
    g.begin_target(&canvas,Some([0.,0.,0.,1.]))?;
    let shades = [S::Sprite,S::Fill,S::Text,S::RimText,S::Mono,S::Nega,S::NoAlpha,S::ForceAlpha];
    for (i,shader) in shades.into_iter().enumerate() {
        let rect = [24.+112.*i as f32,20.,88.,48.];
        draw(&mut g,rect,BASE,S::Fill,B::Opaque,[None;4],[0.;8])?;
        if matches!(shader,S::Text|S::RimText) {
            let mut v = vertices(rect,COLOR);
            for vertex in &mut v {
                vertex.uv = [(16.+vertex.uv[0]*2.)/2048.,(32.+vertex.uv[1]*2.)/2048.];
                if matches!(shader,S::RimText) {
                    vertex.haze = [[16./2048.,32./2048.,18./2048.,34./2048.],
                        [1./2048.,1./2048.,0.5,0x00ff00 as f32]];
                }
            }
            g.draw_effect(&v,shader,B::Alpha,[Some(&atlas),None,None,None],[0.;8])?;
        } else {
            draw(&mut g,rect,COLOR,shader,B::Alpha,[Some(&source),None,None,None],[0.;8])?;
        }
        record(log,&format!("SHADE {shader:?} submitted"))?;
    }
    let blends = [B::Alpha,B::Add,B::Sub,B::Multiply,B::Screen,B::Opaque,B::Over,B::Footprint,B::MaskAlpha];
    for (i,blend) in blends.into_iter().enumerate() {
        let rect = [24.+100.*i as f32,96.,80.,48.];
        draw(&mut g,rect,BASE,S::Fill,B::Opaque,[None;4],[0.;8])?;
        draw(&mut g,rect,COLOR,S::Sprite,blend,[Some(&source),None,None,None],[0.;8])?;
    }
    // Every PS operation sees a new GPU copy made immediately before its draw.
    for i in 0..24 {
        let rect = [24.+76.*(i%12) as f32,172.+72.*(i/12) as f32,60.,48.];
        draw(&mut g,rect,BASE,S::Fill,B::Opaque,[None;4],[0.;8])?;
        g.end_target()?;
        g.copy_texture(&canvas,&backdrop)?;
        g.begin_target(&canvas,None)?;
        draw(&mut g,rect,COLOR,S::Ps,B::Opaque,[Some(&source),Some(&backdrop),None,None],
            [(i+1) as f32,0.,960.,540.,-1.,0.,0.,0.])?;
        record(log,&format!("PS {} copied/submitted",i+1))?;
    }
    // Exercise the remaining entry points too; these tiles need separate oracles.
    for (i,(shader,params)) in [
        (S::Transition,[0.4,8.,1.,0.,0.,0.,0.,0.]),
        (S::Rule,[0.4,8.,960.,540.,0.,0.,0.,0.]),
        (S::Blur,[1.,0.,1.,2.,0.,0.,0.,0.]),
        (S::Alter,[0.;8]),(S::PresentSrgb,[0.;8]),
    ].into_iter().enumerate() {
        let rect = [24.+150.*i as f32,340.,120.,48.];
        draw(&mut g,rect,BASE,S::Fill,B::Opaque,[None;4],[0.;8])?;
        draw(&mut g,rect,COLOR,shader,B::Opaque,[Some(&source),Some(&source),Some(&source),None],params)?;
        record(log,&format!("ADDITIONAL {shader:?} submitted; pixel equivalence not asserted"))?;
    }
    g.end_target()?;
    record(log,"PRESENT BEGIN")?;
    g.present_canvas(&canvas)?;
    let stats = g.stats()?;
    if stats.program_checks != 19 || stats.frames != 1 {
        return Err(format!("unexpected program/frame counts: {stats:?}"));
    }
    record(log,&format!("FRAME READY; all sixteen entries submitted, nine blend states and 24 GPU backdrop copies; {stats:?}"))?;
    std::thread::sleep(std::time::Duration::from_secs(6));
    for id in [&atlas,&backdrop,&canvas,&source] { g.destroy_texture(id)?; }
    group_page(&mut g,log,"cartesian-037",0.37,false)?;
    group_page(&mut g,log,"cartesian-100",1.0,false)?;
    group_page(&mut g,log,"alpha-exhaustive",1.0,true)?;
    if g.stats()?.cdram_mapped_bytes != baseline.cdram_mapped_bytes {
        return Err("effect texture CDRAM did not return to baseline".into());
    }
    record(log,"PASS texture retirement and CDRAM accounting; GXM CLOSE BEGIN")?;
    g.close()?;
    record(log,"PASS single GXM lifetime closed; pixel oracle and hardware status are separate")?;
    Ok(())
}
