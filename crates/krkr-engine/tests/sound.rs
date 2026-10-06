use krkr_engine::{
    Engine, EngineEvent,
    assets::Vfs,
    audio::{Mixer, OutputHost},
};
use std::{
    num::NonZeroUsize,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use tjs_core::RunBudget;
use tjs_runtime::{Runtime, RuntimeExit, clock::MonotonicClock};

#[derive(Clone, Default)]
struct Capture(Arc<Mutex<Option<Mixer>>>);
impl OutputHost for Capture {
    fn start(&self, mixer: Mixer) -> Result<(), String> {
        *self.0.lock().unwrap() = Some(mixer);
        Ok(())
    }
}
struct Harness {
    engine: Engine<MonotonicClock>,
    output: Capture,
}
impl Harness {
    fn poll(&mut self) -> EngineEvent {
        let event = self
            .engine
            .poll(RunBudget::new(128).unwrap(), NonZeroUsize::new(64).unwrap());
        if let EngineEvent::Sound {
            context,
            ref result,
            ..
        } = event
        {
            assert!(
                matches!(result, RuntimeExit::Finished(_)),
                "audio callback: {result:?}"
            );
            self.engine.take_result(context);
        }
        event
    }
    fn run(&mut self, script: &str) -> String {
        let source = self
            .engine
            .runtime_mut()
            .sources
            .add_utf8("sound", script)
            .unwrap();
        let module = tjs_front::compile(&self.engine.runtime().sources, source).unwrap();
        let id = self
            .engine
            .submit(&module)
            .unwrap_or_else(|_| panic!("context capacity"));
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if let EngineEvent::Completed { context, result } = self.poll() {
                assert_eq!(context, id);
                let RuntimeExit::Finished(value) = result else {
                    panic!("{script}: {result:?}")
                };
                let value = self.engine.runtime().heap.display(value).unwrap();
                self.engine.take_result(id);
                self.engine.collect([]);
                return value;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        panic!("sound script timed out: {script}")
    }
    fn render(&mut self) -> [f32; 160] {
        let mut output = [0.0; 160];
        self.output.0.lock().unwrap().as_mut().unwrap().render(
            &mut output,
            2,
            8000,
            Duration::ZERO,
        );
        output
    }
    fn pump(&mut self, millis: u64) {
        for _ in 0..millis.div_ceil(10) {
            self.render();
            std::thread::sleep(Duration::from_millis(10));
            for _ in 0..16 {
                self.poll();
            }
        }
    }
}

#[test]
fn script_audio_reaches_pcm_with_loop_flags_labels_seek_fade_and_cleanup() {
    let dir = tempfile::tempdir().unwrap();
    // Two seconds of distinct PCM plateaus make seek/gain/pan observable.
    let mut wav = Vec::new();
    wav.extend(b"RIFF");
    wav.extend(32036u32.to_le_bytes());
    wav.extend(b"WAVEfmt ");
    wav.extend(16u32.to_le_bytes());
    wav.extend(1u16.to_le_bytes());
    wav.extend(1u16.to_le_bytes());
    wav.extend(8000u32.to_le_bytes());
    wav.extend(16000u32.to_le_bytes());
    wav.extend(2u16.to_le_bytes());
    wav.extend(16u16.to_le_bytes());
    wav.extend(b"data");
    wav.extend(32000u32.to_le_bytes());
    for i in 0..16000 {
        wav.extend((if i < 8000 { 8192i16 } else { 16384i16 }).to_le_bytes());
    }
    std::fs::write(dir.path().join("tone.wav"), &wav).unwrap();
    std::fs::write(dir.path().join("plain.wav"), &wav).unwrap();
    std::fs::write(dir.path().join("tone.wav.sli"), "#2.00\nLink {From=12000;To=8000;Smooth=True;Condition=eq;RefValue=0;CondVar=0;}\nLabel {Position=8000;Name='chorus';}\nLabel {Position=8100;Name=':[1]++';}").unwrap();
    let mut runtime = Runtime::new();
    krkr_engine::storages::install(
        &mut runtime.heap,
        Vfs::new(dir.path(), Default::default()).unwrap(),
    )
    .unwrap();
    let mut engine = Engine::new(
        runtime,
        MonotonicClock::default(),
        Default::default(),
        Default::default(),
    )
    .unwrap();
    let output = Capture::default();
    engine.set_audio_output(output.clone()).unwrap();
    let mut h = Harness { engine, output };
    assert_eq!(
        h.run(
            r#"
            var emptyBuffers=[];
            for(var i=0;i<100;i++) emptyBuffers.add(new WaveSoundBuffer(%[]));
            if(emptyBuffers[99].status!='unload') throw 'empty buffer allocated audio';
            emptyBuffers[99].open('plain.wav');
            emptyBuffers[99].play();emptyBuffers[99].stop();
            for(var i=0;i<emptyBuffers.count;i++) invalidate emptyBuffers[i];
            emptyBuffers.clear();'pool released';
        "#
        ),
        "pool released"
    );
    assert_eq!(
        h.run(
            r#"
        var silent = new WaveSoundBuffer(%[]);
        silent.open('plain.wav');
        silent.play();
        silent.stop();
        var status = silent.status;
        invalidate silent;
        status;
    "#
        ),
        "stop"
    );
    assert_eq!(
        h.run(
            r#"
        var events=[], labels=[];
        var w=new WaveSoundBuffer(%[action:function(e) {
            if(e.type=='onStatusChanged') global.events.add(e.status);
            if(e.type=='onLabel') global.labels.add(e.name);
            if(e.type=='onFadeCompleted') global.events.add('fade');
        }]);
        w.open('tone.wav');
        w.flags[0]=1; w.flags[15]=20000;
        var oldLabels=w.labels;
        w.status+':'+w.totalTime+':'+w.labels.chorus.position+':'+w.flags[15];
    "#
        ),
        "stop:2000:1000:9999"
    );
    assert_eq!(
        h.run("w.samplePosition=8000;w.pan=-100000;w.volume=50000;w.play();w.status;"),
        "play"
    );
    let pcm = h.render();
    assert!(
        (pcm[0] - 0.25).abs() < 0.0001 && pcm[1] == 0.0,
        "decoded PCM: {:?}",
        &pcm[..4]
    );
    h.pump(80);
    assert_eq!(
        h.run("w.paused=true;var stoppedAt=w.samplePosition;labels[0];"),
        "chorus"
    );
    assert!(h.render().iter().all(|&v| v == 0.0));
    h.pump(30);
    assert_eq!(h.run("w.samplePosition==stoppedAt;"), "1");
    assert_eq!(
        h.run("w.stop();w.paused=false;w.pan=0;w.volume=100000;w.play();w.samplePosition;"),
        "0"
    );
    let pcm = h.render();
    assert!((pcm[0] - 0.25).abs() < 0.0001 && pcm[0] == pcm[1]);
    h.run("w.flags[0]=0;w.samplePosition=8000;w.fade(50000,60);");
    h.pump(1250);
    assert_eq!(
        h.run("w.status+':'+w.volume+':'+(w.flags[1]>1)+':'+(events.find('fade')>=0);"),
        "play:50000:1:1"
    );
    h.run("w.flags[0]=1;");
    h.pump(1600);
    assert_eq!(h.run("w.status+':'+events[events.count-1];"), "stop:stop");
    assert_eq!(
        h.run(
            "w.open('plain.wav');(isvalid oldLabels)+':'+(w.labels.chorus===void)+':'+w.flags[1];"
        ),
        "0:1:0"
    );
    h.run("w.looping=true;w.play();");
    h.engine.reset();
    assert!(h.render().iter().all(|&v| v == 0.0));
    assert_eq!(h.run("w.status;"), "unload");
}
