//! DEV isolation of basic quads, effect vertices, render targets and presentation.
//! The seven patches deliberately use different paths; API success is not a pixel verdict.
use std::{fs::{File, OpenOptions}, io::Write};
use knife_render_api::Vertex;
use crate::{data_path, gxm::{EffectBlend as B, EffectShader as S, Gxm, TextureFormat}};

fn record(log: &mut File, message: &str) -> Result<(), String> {
    writeln!(log, "{message}").and_then(|()| log.flush()).map_err(|e| e.to_string())?;
    *log = OpenOptions::new().append(true).open(data_path("pipeline-probe.log")?)
        .map_err(|e| e.to_string())?;
    Ok(())
}
fn vertices(rect: [f32; 4], color: [f32; 4]) -> [Vertex; 6] {
    [[0.,0.],[1.,0.],[0.,1.],[0.,1.],[1.,0.],[1.,1.]].map(|p| Vertex {
        pos: [(rect[0]+p[0]*rect[2])*(2./960.)-1., 1.-(rect[1]+p[1]*rect[3])*(2./540.)],
        uv: p, color, haze: [[0.;4];2], soft: [[0.;4];3],
    })
}
pub fn run_pipeline_probe() -> Result<(), String> {
    crate::native::init_data_dir()?;
    std::fs::write(data_path("gxm-native.log")?, []).map_err(|e| e.to_string())?;
    let mut log = File::create(data_path("pipeline-probe.log")?).map_err(|e| e.to_string())?;
    let result = checks(&mut log);
    if let Err(ref error) = result { record(&mut log, &format!("FAIL {error}"))?; }
    record(&mut log, if result.is_ok() { "RESULT PASS" } else { "RESULT FAIL" })?;
    result
}
fn checks(log: &mut File) -> Result<(), String> {
    record(log, "knife Vita DEV pipeline isolation v6; crossover display/offscreen surfaces and backing blocks")?;
    let mut g = Gxm::new()?;
    let source = g.create_texture_ex(Some(&[128,64,200,255].repeat(4)), 2,2,8,
        TextureFormat::Rgba8, false,false,false)?;
    let basic = g.create_texture_ex(None,64,64,256,TextureFormat::Rgba8,false,false,true)?;
    let effect = g.create_texture_ex(None,64,64,256,TextureFormat::Rgba8,false,false,true)?;
    g.begin_target(&basic, Some([0.25,0.5,0.75,1.]))?;
    g.end_target()?;
    g.begin_target(&effect, Some([0.,0.,0.,1.]))?;
    g.draw_effect(&vertices([0.,0.,960.,540.], [0.8,0.2,0.1,1.]), S::Fill,B::Opaque,
        [None;4],[0.;8])?;
    g.end_target()?;
    record(log, "OFFSCREEN COMPLETE; basic clear blue, effect fill red")?;
    // Diagnostic only: CPU visibility differs from render-to-texture sampling.
    // In particular, emulator mapping modes can leave backing bytes stale.
    for (name, target) in [("basic-clear", &basic), ("effect-fill", &effect)] {
        let pixels = g.read_texture(target)?;
        record(log, &format!("CPU OBSERVATION {name}: first={:?}, center={:?}",
            &pixels[..4], &pixels[(32*64+32)*4..(32*64+32)*4+4]))?;
    }
    // Repeated frames also exclude a configured-but-not-yet-submitted first scanout.
    for _ in 0..12 {
        // v4 changes only scene ordering: resubmit both targets after every
        // display frame, including after the emulator has started scanout.
        g.begin_target(&basic, Some([0.25,0.5,0.75,1.]))?;
        g.end_target()?;
        g.begin_target(&effect, Some([0.,0.,0.,1.]))?;
        g.draw_effect(&vertices([0.,0.,960.,540.], [0.8,0.2,0.1,1.]), S::Fill,B::Opaque,
            [None;4],[0.;8])?;
        g.end_target()?;
        g.begin([0.1,0.1,0.1,1.])?;
        g.draw(Some(&source),[24.,20.,100.,100.],[0.,0.,1.,1.],[1.;4],false)?;
        g.draw_effect(&vertices([140.,20.,100.,100.],[0.8,0.2,0.1,1.]), S::Fill,B::Opaque,
            [None;4],[0.;8])?;
        g.draw_effect(&vertices([256.,20.,100.,100.],[1.;4]), S::Sprite,B::Opaque,
            [Some(&source),None,None,None],[0.;8])?;
        g.draw(Some(&basic),[372.,20.,100.,100.],[0.,0.,1.,1.],[1.;4],false)?;
        g.draw(Some(&effect),[488.,20.,100.,100.],[0.,0.,1.,1.],[1.;4],false)?;
        g.draw_effect(&vertices([604.,20.,100.,100.],[1.;4]), S::Present,B::Opaque,
            [Some(&basic),None,None,None],[0.;8])?;
        g.draw_effect(&vertices([720.,20.,100.,100.],[1.;4]), S::Present,B::Opaque,
            [Some(&effect),None,None,None],[0.;8])?;
        g.present()?;
    }
    record(log, &format!("FRAME READY; patches basic/source, fill, sprite, basic/RT-clear, basic/RT-effect, present/RT-clear, present/RT-effect; {:?}",g.stats()?))?;
    std::thread::sleep(std::time::Duration::from_secs(4));
    let scanout = g.create_texture_ex(None,960,544,960*4,TextureFormat::Rgba8,false,false,true)?;
    g.dev_swap_display_surface(&scanout)?;
    g.begin([0.25,0.5,0.75,1.])?;
    g.draw_effect(&vertices([480.,0.,480.,540.],[0.8,0.2,0.1,1.]), S::Fill,B::Opaque,
        [None;4],[0.;8])?;
    g.present()?;
    record(log, "SWAPPED DISPLAY READY; normal begin/present now uses the original RT surface and backing block")?;
    std::thread::sleep(std::time::Duration::from_secs(4));
    g.begin_target(&scanout, Some([0.25,0.5,0.75,1.]))?;
    g.draw_effect(&vertices([480.,0.,480.,540.],[0.8,0.2,0.1,1.]), S::Fill,B::Opaque,
        [None;4],[0.;8])?;
    g.end_target()?;
    g.dev_scanout_target(&scanout)?;
    record(log, "RT SCANOUT READY; left basic blue [64,128,191], right effect red [204,51,25]; no texture sampling")?;
    std::thread::sleep(std::time::Duration::from_secs(4));
    // Backend close detaches scanout before retiring this target.
    for id in [&effect,&basic,&source] { g.destroy_texture(id)?; }
    g.close()?;
    record(log,"PASS single GXM lifetime closed; pixel inspection remains independent")?;
    Ok(())
}
