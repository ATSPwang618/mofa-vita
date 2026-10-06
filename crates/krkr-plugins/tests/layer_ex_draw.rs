mod support;
use krkr_engine::{
    Engine, EngineEvent,
    assets::Vfs,
    protocol::{
        self,
        graphics::{Command as Graphics, ImageId, Size},
        pixels::{Bytes, Pixels},
        window::{Command, Geometry, Response},
    },
};
use std::{
    collections::HashMap,
    num::NonZeroUsize,
    sync::Arc,
    time::{Duration, Instant},
};
use tjs_core::{RunBudget, Value};
use tjs_runtime::{Runtime, RuntimeExit, clock::MonotonicClock};

#[test]
fn vector_plugin_end_to_end() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::copy(
        concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../krkr-render/fonts/QiushuiShotai.ttf"
        ),
        dir.path().join("font.ttf"),
    )
    .unwrap();
    // A generated 2x2 opaque blue BMP, independent of game assets.
    let mut bmp = vec![0u8; 70];
    bmp[..2].copy_from_slice(b"BM");
    for (offset, value) in [(2, 70u32), (10, 54), (14, 40), (18, 2), (22, 2), (34, 16)] {
        bmp[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
    }
    bmp[26] = 1;
    bmp[28] = 24;
    for i in [54, 57, 62, 65] {
        bmp[i] = 255;
    }
    std::fs::write(dir.path().join("blue.bmp"), bmp).unwrap();
    let mut runtime = Runtime::new();
    krkr_engine::install(&mut runtime, support::Log).unwrap();
    krkr_engine::storages::install(
        &mut runtime.heap,
        Vfs::new(dir.path(), Default::default()).unwrap(),
    )
    .unwrap();
    krkr_plugins::register(&mut runtime.heap).unwrap();
    let mut engine = Engine::new(
        runtime,
        MonotonicClock::default(),
        Default::default(),
        Default::default(),
    )
    .unwrap();
    let (client, host) = protocol::window::channel(Default::default(), Arc::new(|| {}));
    engine.attach_windows(client).unwrap();
    let script = r#"
        function check(v,s) { if(!v) throw s; }
        Plugins.link('layerExDraw.dll');
        check(GdiPlus.HatchStyleMax==52 && GdiPlus.RotateNoneFlipY==6, 'constants');
        var point=new GdiPlus.PointF(2,3), rect=new GdiPlus.RectF(2,3,4,5);
        check(point.Equals([2,3]) && rect.location.Equals(point) && rect.Clone().Equals(rect), 'geometry');
        var m=new GdiPlus.Matrix(); m.Scale(2,3); m.Translate(4,5);
        check(m.OffsetX()==4 && m.OffsetY()==5 && m.IsInvertible(), 'matrix');
        var n=new GdiPlus.Matrix(); n.SetElements(2,0,0,3,8,15);
        check(m.Equals(n) && n.Invert()==0, 'matrix product');
        m.Reset();
        var bounded=false; try { GdiPlus.addPrivateFont('font.ttf'); } catch(e) { bounded=true; }
        check(bounded, 'file source exceeds the default 24 MiB font budget');
        GdiPlus.addPrivateFont('QiushuiShotai');
        var font=new GdiPlus.Font('QiushuiShotai',24,0);
        check(font.ascent>0 && font.descent>=0 && font.familyName=='QiushuiShotai', 'font');
        font.emSize=30; font.style=GdiPlus.FontStyleBold; font.forceSelfPathDraw=true;
        font.familyName='fallback-name';
        check(GdiPlus.getFontList(true).count==1 && font.ascent>0, 'fallback list');
        GdiPlus.addPrivateFont('QiushuiShotai');
        check(GdiPlus.getFontList(true).count==2, 'private duplicates');
        System.exitOnWindowClose=false;
        var w=new Window(), root=new Layer(w,null), l=new Layer(w,root);
        l.setImageSize(64,64); l.setSize(64,64);
        var updates=0; l.update=function(){global.updates++;};
        l.clear(0); l.updateWhenDraw=0;
        var app=new GdiPlus.Appearance(); app.addBrush(0xffff0000);
        l.drawRectangle(app,4,4,12,12);
        check(l.getMainPixel(8,8)==0xff0000 && l.getMaskPixel(8,8)==255 && updates==1, 'solid and update');
        l.setTransform(new GdiPlus.Matrix(1,0,0,1,20,0));
        l.drawRectangle(app,4,4,12,12); l.resetTransform();
        check(l.getMainPixel(28,8)==0xff0000, 'transform');
        l.setClip(0,0,8,8); app.clear(); app.addBrush(0xff00ff00); l.drawRectangle(app,0,0,16,16);
        check(l.getMainPixel(4,4)==0x00ff00 && l.getMainPixel(10,10)==0xff0000, 'clip');
        l.setClip(0,0,64,64);
        class Coord { property x { getter { System.wait(1); return 20; } } property y { getter { return 20; } } }
        var points=[new Coord(),[40,20],[40,40],[20,40]];
        var path=new GdiPlus.Path(); path.drawPolygon(points);
        app.addPen(0xffffffff,%['width'=>2,'dashStyle'=>[3,2],'lineJoin'=>2,'endCap'=>%['width'=>2,'height'=>2]]);
        l.drawPath(app,path);
        path.startFigure(); path.drawBezier(1,1,3,4,5,4,7,1); path.closeFigure();
        path.drawArc(2,2,12,10,0,180); path.drawPie(2,2,12,10,90,90);
        path.drawCurve3([[2,2],[6,8],[12,2]],0,2,0.5); path.drawClosedCurve2([[2,2],[6,8],[12,2]],0.5);
        l.drawPath(app,path);
        app.clear(); app.addBrush(%['type'=>4,'point1'=>[0,0],'point2'=>[64,0],'color1'=>0xffff0000,'color2'=>0xff0000ff]);
        l.drawRectangle(app,0,48,64,8);
        check(l.getMainPixel(4,50)!=l.getMainPixel(60,50), 'gradient');
        app.clear(); app.addBrush(%['type'=>3,'points'=>[[0,0],[16,16]],'centerColor'=>0xffffffff,'surroundColors'=>[0xff000000]]);
        l.drawEllipse(app,0,0,16,16);
        app.clear(); app.addBrush(%['type'=>1,'hatchStyle'=>4,'foreColor'=>0xffffffff,'backColor'=>0xff000000]);
        l.drawRectangle(app,40,0,16,16);
        app.clear(); app.addBrush(%['type'=>2,'image'=>'blue.bmp']); l.drawRectangle(app,40,20,8,8);
        check(l.getMainPixel(42,22)==255, 'texture');
        var image=new GdiPlus.Image('blue.bmp');
        check(image.GetWidth()==2 && image.GetType()==1 && image.Clone().GetHeight()==2, 'image');
        l.drawImage(0,30,image); l.drawImageRect(4,30,image,0,0,2,2);
        l.drawImageStretch(8,30,4,4,image,0,0,2,2);
        l.drawImageAffine(image,0,0,2,2,true,1,0,0,1,16,30);
        check(l.getMainPixel(0,30)==255 && l.getMainPixel(16,30)==255, 'image draws');
        var measure=l.measureString(font,'Ag中文');
        check(measure.width>0 && measure.height>0 && measure.Equals(l.measureStringInternal(font,'Ag中文')), 'measure');
        app.clear(); app.addBrush(0xffffffff); app.addPen(0xff00ff00,1);
        l.clear(0); l.drawString(font,app,0,40,'Ag');
        check(l.saveRecord('TEXT_FILE'), 'save text');
        l.clear(0); l.drawPathString(font,app,0,40,'Ag');
        check(l.saveRecord('OUTLINE_FILE'), 'save outline');
        l.record=true; l.clear(0); app.clear(); app.addBrush(0xffff0000); l.drawRectangle(app,4,4,8,8);
        l.setViewTransform(new GdiPlus.Matrix());
        check(l.getMainPixel(6,6)==0xff0000, 'unchanged view');
        var recorded=l.getRecordImage();
        check(recorded.GetType()==2 && l.getMaskPixel(6,6)==0, 'record extraction redraw');
        l.drawImage(0,0,recorded);
        check(l.getMainPixel(6,6)==0xff0000, 'vector replay');
        check(!l.loadRecord('absent') && l.redrawRecord(), 'reference record behavior');
        l.record=false; check(!l.redrawRecord(), 'record disabled');
        var reference=new Layer(w,root); reference.setImageSize(64,64); reference.setClip(0,0,64,64);
        // The transparent pen forces full-clip rendering without changing the fill.
        for(var caseId=0;caseId<5;caseId++) {
            l.resetTransform(); reference.resetTransform();
            l.clear(0x80335577); reference.clear(0x80335577);
            var affine=new GdiPlus.Matrix(1.125,0.25,-0.125,0.875,17.25,9.5);
            l.setTransform(affine); reference.setTransform(affine);
            app.clear();
            if(caseId==0) app.addBrush(0xc0e0a020);
            if(caseId==1) app.addBrush(%['type'=>4,'point1'=>[0,0],'point2'=>[64,0],'color1'=>0x80ff0000,'color2'=>0xc00000ff]);
            if(caseId==2) app.addBrush(%['type'=>1,'hatchStyle'=>4,'foreColor'=>0xffffffff,'backColor'=>0x40000000]);
            if(caseId==3) app.addBrush(%['type'=>2,'image'=>'blue.bmp']);
            if(caseId==4) app.addBrush(%['type'=>3,'points'=>[[0,0],[32,32]],'centerColor'=>0xffffffff,'surroundColors'=>[0x40000000]]);
            l.drawEllipse(app,3.25,5.5,17.5,13.25);
            app.addPen(0,1);
            reference.drawEllipse(app,3.25,5.5,17.5,13.25);
            check(l.saveRecord('REGION_FILE'+caseId+'.png'), 'save cropped rendering');
            check(reference.saveRecord('REFERENCE_FILE'+caseId+'.png'), 'save full rendering');
        }
        l.resetTransform(); l.setClip(10,11,0,12);
        var beforeUpdates=updates; l.updateWhenDraw=1; l.drawRectangle(app,0,0,64,64);
        check(updates==beforeUpdates+1, 'empty clip still invokes update');
        check(Plugins.unlink('layerExDraw.dll'), 'unlink');
        Plugins.link('layerExDraw.dll'); check(GdiPlus.getFontList(true).count==0,'font cleanup');
        check(Plugins.unlink('layerExDraw.dll'), 'second unlink');
        invalidate w; 42;
    "#.replace("TEXT_FILE", &dir.path().join("text.png").to_string_lossy().replace('\\', "/"))
      .replace("OUTLINE_FILE", &dir.path().join("outline.png").to_string_lossy().replace('\\', "/"));
    let script = script
        .replace(
            "REGION_FILE",
            &dir.path()
                .join("region-")
                .to_string_lossy()
                .replace('\\', "/"),
        )
        .replace(
            "REFERENCE_FILE",
            &dir.path()
                .join("reference-")
                .to_string_lossy()
                .replace('\\', "/"),
        );
    let source = engine
        .runtime_mut()
        .sources
        .add_utf8("LayerExDraw", &script)
        .unwrap();
    let module = tjs_front::compile(&engine.runtime().sources, source).unwrap();
    let id = engine.submit(&module).unwrap_or_else(|_| panic!("submit"));
    let mut images: HashMap<ImageId, (Size, Vec<u8>)> = HashMap::new();
    let mut small_reads = 0;
    let mut small_writes = 0;
    let mut reads = 0;
    let mut clear_without_read = false;
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        assert!(Instant::now() < deadline, "drawing timeout");
        let event = engine.poll(RunBudget::new(100).unwrap(), NonZeroUsize::new(64).unwrap());
        engine.collect([]);
        while let Some(request) = host.next_request() {
            let response = match &request.command {
                Command::Graphics(command) => match command {
                    Graphics::Create { image, size, .. } => {
                        images.insert(*image, (*size, vec![0; size.rgba_bytes().unwrap()]));
                        Response::Done
                    }
                    Graphics::Resize { image, size, .. }
                    | Graphics::EnableImage { image, size, .. } => {
                        images.insert(image.id, (*size, vec![0; size.rgba_bytes().unwrap()]));
                        Response::Done
                    }
                    Graphics::PatchPixels { image, pixels }
                    | Graphics::Upload { image, pixels } => {
                        let bytes = pixels.main.as_ref().unwrap().as_slice().to_vec();
                        images.insert(image.id, (pixels.size, bytes));
                        Response::Done
                    }
                    Graphics::PatchRegion {
                        image,
                        rectangle,
                        pixels,
                    } => {
                        let (size, data) = images.get_mut(&image.id).unwrap();
                        clear_without_read |= reads == 0 && *rectangle == size.rect();
                        assert_eq!(rectangle.width, pixels.size.width);
                        assert_eq!(rectangle.height, pixels.size.height);
                        if rectangle.width < size.width && rectangle.height < size.height {
                            small_writes += 1;
                        }
                        let source = pixels.main.as_ref().unwrap().as_slice();
                        for row in 0..rectangle.height as usize {
                            let dst = ((rectangle.top as usize + row) * size.width as usize
                                + rectangle.left as usize)
                                * 4;
                            let src = row * rectangle.width as usize * 4;
                            data[dst..dst + rectangle.width as usize * 4]
                                .copy_from_slice(&source[src..src + rectangle.width as usize * 4]);
                        }
                        Response::Done
                    }
                    Graphics::ReadImage { image } | Graphics::ReadRegion { image, .. } => {
                        reads += 1;
                        let (size, data) = &images[&image.id];
                        let rectangle = match command {
                            Graphics::ReadRegion { rectangle, .. } => *rectangle,
                            _ => size.rect(),
                        };
                        if reads == 1 {
                            assert_eq!(
                                (rectangle.width, rectangle.height),
                                (16, 16),
                                "12x12 fill should transfer only its bounds plus raster margin"
                            );
                        }
                        if rectangle.width < size.width && rectangle.height < size.height {
                            small_reads += 1;
                        }
                        let size = Size {
                            width: rectangle.width,
                            height: rectangle.height,
                        };
                        let mut main =
                            Bytes::zeroed(size.rgba_bytes().unwrap(), &host.staging_budget())
                                .unwrap();
                        let stride = images[&image.id].0.width as usize * 4;
                        for (row, dst) in main
                            .as_mut_slice()
                            .chunks_exact_mut(size.width as usize * 4)
                            .enumerate()
                        {
                            let src = (rectangle.top as usize + row) * stride
                                + rectangle.left as usize * 4;
                            dst.copy_from_slice(&data[src..src + dst.len()]);
                        }
                        Response::Image(Pixels {
                            size,
                            main: Some(main),
                            province: None,
                        })
                    }
                    Graphics::Pixel {
                        image,
                        x,
                        y,
                        province: false,
                    } => {
                        let (size, data) = &images[&image.id];
                        let i = (*y as usize * size.width as usize + *x as usize) * 4;
                        Response::Pixel(u32::from_be_bytes([
                            data[i + 3],
                            data[i],
                            data[i + 1],
                            data[i + 2],
                        ]))
                    }
                    Graphics::Independ { .. } => Response::Done,
                    other => panic!("unhandled graphics: {other:?}"),
                },
                _ => Response::Geometry(Geometry {
                    width: 64,
                    height: 64,
                    inner_width: 64,
                    inner_height: 64,
                    ..Default::default()
                }),
            };
            request.respond(Ok(response));
        }
        host.take_scenes(u64::MAX);
        match event {
            EngineEvent::Completed {
                context,
                result: RuntimeExit::Finished(Value::Int(42)),
            } if context == id => {
                engine.take_result(id);
                break;
            }
            EngineEvent::System {
                result: RuntimeExit::Finished(_),
                ..
            }
            | EngineEvent::Yielded
            | EngineEvent::Waiting { .. }
            | EngineEvent::Idle => {}
            other => panic!("{other:?}"),
        }
        std::thread::yield_now();
    }
    for name in ["text.png", "outline.png"] {
        let data = std::fs::read(dir.path().join(name)).unwrap();
        let pixels = image::load_from_memory(&data).unwrap().to_rgba8();
        assert!(pixels.pixels().any(|p| p.0[3] != 0), "{name} has no glyphs");
    }
    assert!(
        small_reads >= 8,
        "small vector draws still read whole images"
    );
    assert!(
        clear_without_read,
        "clear unnecessarily reads destination pixels"
    );
    assert_eq!(
        small_reads, small_writes,
        "readback and patch regions must match"
    );
    for case in 0..5 {
        let read = |name: &str| {
            image::open(dir.path().join(format!("{name}-{case}.png")))
                .unwrap()
                .to_rgba8()
        };
        let cropped = read("region");
        let reference = read("reference");
        assert_eq!(cropped.dimensions(), reference.dimensions());
        let differences = cropped
            .pixels()
            .zip(reference.pixels())
            .filter(|(a, b)| a != b)
            .count();
        let examples: Vec<_> = cropped
            .pixels()
            .zip(reference.pixels())
            .enumerate()
            .filter(|(_, (a, b))| a != b)
            .take(8)
            .collect();
        assert_eq!(
            differences, 0,
            "cropped rasterization changed pixels, case {case}: {examples:?}"
        );
    }
}
