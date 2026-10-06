#![cfg(not(target_os = "vita"))]

#[test]
fn injected_clock_advances_timed_waits_after_script_and_storage_work_settle() {
    use std::{cell::Cell, rc::Rc, time::Duration};
    #[derive(Clone)]
    struct Manual(Rc<Cell<Duration>>);
    impl tjs_runtime::clock::Clock for Manual {
        fn now(&self) -> Duration {
            self.0.get()
        }
    }
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("startup.tjs"),
        "Scripts.execStorage('loaded.tjs'); System.wait(100); if(System.getTickCount()!=100) throw 'clock did not wake timer'; System.wait(50); if(System.getTickCount()!=150) throw 'second deadline'; System.exit(23);"
    ).unwrap();
    std::fs::write(
        root.path().join("loaded.tjs"),
        "if(System.getTickCount()!=0) throw 'clock advanced during storage read';",
    )
    .unwrap();
    let clock = Manual(Rc::new(Cell::new(Duration::ZERO)));
    let advancing = clock.clone();
    let mut idle_ticks = Vec::new();
    let code = krkr_host_vita::bootstrap::run_with_clock(
        root.path(),
        krkr_host_vita::bootstrap::Options {
            startup: "startup.tjs",
            debug: false,
            data: None,
            after_startup: None,
            effect_interval_ms: None,
        },
        None,
        &std::sync::atomic::AtomicBool::new(false),
        clock,
        |_| Ok(()),
        |engine| {
            assert_eq!(engine.pending_host_operations(), 0);
            assert!(engine.pending_operations() > 0, "timer should remain armed");
            let now = advancing.0.get();
            idle_ticks.push(now.as_millis());
            advancing.0.set(now + Duration::from_millis(50));
            Ok(true)
        },
    )
    .unwrap();
    assert_eq!(code, 23);
    assert_eq!(idle_ticks, [0, 50, 100]);
}

#[test]
fn effect_cadence_only_changes_action_manager_and_keeps_slower_settings() {
    let root = tempfile::tempdir().unwrap();
    for (initial, limit, expected) in [(16, Some(33), 33), (50, Some(33), 50), (16, None, 16)] {
        std::fs::write(root.path().join("actionmanager.tjs"), format!(
            "class ActionManager {{ var interval={initial}; var calls=0; function startFlip() {{ calls++; }} }}"
        )).unwrap();
        std::fs::write(
            root.path().join("startup.tjs"),
            "Scripts.execStorage('actionmanager.tjs');",
        )
        .unwrap();
        let after = format!(
            "global.kag=%['actmgr'=>new ActionManager()]; System.wait(200); var manager=kag.actmgr; manager.startFlip(); if(manager.interval != {expected} || manager.calls != 1) throw 'cadence changed incorrectly'; if(typeof global.__krkrEffectTuner != 'undefined') throw 'tuner not released'; System.exit(21);"
        );
        assert_eq!(
            krkr_host_vita::bootstrap::run_with_options(
                root.path(),
                krkr_host_vita::bootstrap::Options {
                    startup: "startup.tjs",
                    debug: false,
                    data: None,
                    after_startup: Some(&after),
                    effect_interval_ms: limit,
                },
                None,
                &std::sync::atomic::AtomicBool::new(false),
                |_| Ok(())
            )
            .unwrap(),
            21
        );
    }
    for script in [
        "",
        "global.kag=%[];",
        "class ActionManager {} global.kag=%[];",
    ] {
        std::fs::write(root.path().join("startup.tjs"), script).unwrap();
        assert_eq!(
            krkr_host_vita::bootstrap::run_with_options(
                root.path(),
                krkr_host_vita::bootstrap::Options {
                    startup: "startup.tjs",
                    debug: false,
                    data: None,
                    after_startup: Some("System.exit(22);"),
                    effect_interval_ms: Some(33),
                },
                None,
                &std::sync::atomic::AtomicBool::new(false),
                |_| Ok(())
            )
            .unwrap(),
            22
        );
    }
}

#[test]
fn scene_publication_failure_stops_the_vm_instead_of_leaving_a_frozen_window() {
    use krkr_protocol::window::{self, Command, Geometry, Response};
    use std::{
        sync::{Arc, atomic::AtomicBool},
        time::{Duration, Instant},
    };

    let root = tempfile::tempdir().unwrap();
    std::fs::write(
        root.path().join("startup.tjs"),
        "System.exitOnWindowClose = false;\n\
         var w = new Window(); w.visible = true;\n\
         var l = new Layer(w, null); l.visible = true; w.update();\n\
         invalidate w;\n\
         var next = new Window(); next.visible = true;\n\
         var nextLayer = new Layer(next, null); nextLayer.visible = true; next.update();\n\
         System.wait(60000);",
    )
    .unwrap();
    let (client, host) = window::channel(
        window::Limits {
            windows: 1,
            ..Default::default()
        },
        Arc::new(|| {}),
    );
    let stopped = Arc::new(AtomicBool::new(false));
    let cancel = stopped.clone();
    let path = root.path().to_owned();
    let vm = std::thread::spawn(move || {
        krkr_host_vita::bootstrap::run_with_windows(&path, Some(client), &cancel)
    });
    let deadline = Instant::now() + Duration::from_secs(5);
    // A host can acknowledge commands before consuming its queued scene. The
    // stale first-window scene forces the second publication over its limit.
    while !vm.is_finished() && Instant::now() < deadline {
        while let Some(request) = host.next_request() {
            let response = if matches!(request.command, Command::Graphics(_)) {
                Response::Done
            } else {
                Response::Geometry(Geometry {
                    width: 64,
                    height: 64,
                    inner_width: 64,
                    inner_height: 64,
                    ..Default::default()
                })
            };
            request.respond(Ok(response));
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    let finished = vm.is_finished();
    if !finished {
        stopped.store(true, std::sync::atomic::Ordering::Release);
        host.disconnect("test timed out".into());
    }
    let result = vm.join().unwrap();
    assert!(
        finished,
        "VM ignored a failed scene publication: {result:?}"
    );
    assert_eq!(result.unwrap_err(), "scene window capacity reached");
}

#[test]
fn startup_app_lock_is_installed_and_released_between_games() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(
        root.path().join("startup.tjs"),
        "if (!System.createAppLock('vita-boot-test')) throw 'missing lock';\n\
         if (System.createAppLock('vita-boot-test')) throw 'duplicate lock';\n\
         System.exit(17);",
    )
    .unwrap();
    assert_eq!(krkr_host_vita::bootstrap::run(root.path()).unwrap(), 17);
    assert_eq!(krkr_host_vita::bootstrap::run(root.path()).unwrap(), 17);
}
#[test]
fn startup_loads_another_storage_and_propagates_system_exit() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(
        root.path().join("startup.tjs"),
        "Scripts.execStorage('next.tjs');",
    )
    .unwrap();
    std::fs::write(
        root.path().join("next.tjs"),
        "Debug.message('silent'); System.exit(23);",
    )
    .unwrap();
    assert_eq!(krkr_host_vita::bootstrap::run(root.path()).unwrap(), 23);
    assert!(root.path().join("savedata").is_dir());
    assert!(!root.path().join("savedata/krkr-debug.log").exists());
}

#[test]
fn selected_loose_and_archive_startups_run_and_debug_is_opt_in() {
    use krkr_engine::assets::xp3::{Compression, offline::pack_directory};
    let root = tempfile::tempdir().unwrap();
    let source = tempfile::tempdir().unwrap();
    std::fs::create_dir(root.path().join("scripts")).unwrap();
    std::fs::create_dir(source.path().join("scripts")).unwrap();
    let script = "Debug.message('chosen entry'); System.exit(42);";
    std::fs::write(root.path().join("startup.tjs"), "System.exit(1);").unwrap();
    std::fs::write(root.path().join("scripts/boot.tjs"), script).unwrap();
    std::fs::write(source.path().join("scripts/boot.tjs"), script).unwrap();
    pack_directory(
        source.path(),
        &root.path().join("data.xp3"),
        Compression::Zlib,
        Default::default(),
    )
    .unwrap();
    let stopped = std::sync::atomic::AtomicBool::new(false);
    for startup in ["scripts/boot.tjs", "data.xp3>scripts/boot.tjs"] {
        assert_eq!(
            krkr_host_vita::bootstrap::run_configured(root.path(), startup, true, None, &stopped)
                .unwrap(),
            42
        );
        let log_path = root.path().join("savedata/krkr-debug.log");
        assert!(!log_path.exists());
        assert_eq!(
            krkr_host_vita::bootstrap::run_configured(root.path(), startup, false, None, &stopped)
                .unwrap(),
            42
        );
        assert!(!log_path.exists());
    }
}

#[test]
fn root_patch_precedes_default_loose_and_archive_startups() {
    use krkr_engine::assets::xp3::{Compression, offline::pack_directory};
    let root = tempfile::Builder::new()
        .prefix("krkr-补丁-")
        .tempdir()
        .unwrap();
    let source = tempfile::tempdir().unwrap();
    std::fs::create_dir(root.path().join("scripts")).unwrap();
    std::fs::create_dir(source.path().join("scripts")).unwrap();
    std::fs::write(
        root.path().join("patch.tjs"),
        r#"Plugins.mock("VitaBootCompat.dll", %[
            "onLink" => function(p) { p.export("patchValue", 41); }
        ]);
        var bootTrace = "patch";
        Scripts.afterLoad("entry-body.tjs", function() { bootTrace += ":after"; });"#,
    )
    .unwrap();
    std::fs::write(
        root.path().join("entry-body.tjs"),
        r#"if (bootTrace != "patch") throw "patch order";
        Plugins.link("VitaBootCompat.dll");
        bootTrace += ":body";"#,
    )
    .unwrap();
    let entry = r#"Scripts.execStorage(System.exePath + "entry-body.tjs");
        if (bootTrace != "patch:body:after") throw "afterLoad order";
        System.exit(patchValue + 1);"#;
    std::fs::write(root.path().join("startup.tjs"), entry).unwrap();
    std::fs::write(root.path().join("scripts/boot.tjs"), entry).unwrap();
    std::fs::write(source.path().join("scripts/boot.tjs"), entry).unwrap();
    std::fs::write(source.path().join("patch.tjs"), "throw 'archive patch';").unwrap();
    pack_directory(
        source.path(),
        &root.path().join("data.xp3"),
        Compression::Zlib,
        Default::default(),
    )
    .unwrap();
    let stopped = std::sync::atomic::AtomicBool::new(false);
    for startup in [
        "startup.tjs",
        "scripts/boot.tjs",
        "data.xp3>scripts/boot.tjs",
    ] {
        assert_eq!(
            krkr_host_vita::bootstrap::run_configured(root.path(), startup, true, None, &stopped)
                .unwrap(),
            42
        );
        assert!(!root.path().join("savedata/krkr-debug.log").exists());
    }
}

#[test]
fn patch_errors_prevent_entry_execution() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("startup.tjs"), "System.exit(7);").unwrap();
    let patch = root.path().join("patch.tjs");
    std::fs::write(&patch, "throw 'patch failed';").unwrap();
    let error = krkr_host_vita::bootstrap::run(root.path()).unwrap_err();
    assert!(error.contains("patch failed"), "{error}");
    std::fs::remove_file(&patch).unwrap();
    std::fs::create_dir(&patch).unwrap();
    let error = krkr_host_vita::bootstrap::run(root.path()).unwrap_err();
    assert!(error.contains("script is not a file"), "{error}");
}

#[test]
fn startup_errors_show_original_exception_text_and_source_stack() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(
        root.path().join("startup.tjs"),
        "Scripts.execStorage('init.tjs');",
    )
    .unwrap();
    std::fs::write(
        root.path().join("init.tjs"),
        "System.exceptionHandler=function(e){throw new Exception('failed to save');};\n\
         function boot(){throw new Exception('missing asset', 'original trace');}\nboot();",
    )
    .unwrap();
    let error = krkr_host_vita::bootstrap::run(root.path()).unwrap_err();
    for expected in [
        "missing asset",
        "original trace",
        "failed to save",
        "init.tjs:2:",
        "in boot",
        "startup.tjs",
    ] {
        assert!(error.contains(expected), "missing {expected}: {error}");
    }
}
