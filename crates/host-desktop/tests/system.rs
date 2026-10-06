#[path = "../../krkr-engine/tests/support/events.rs"]
mod support;
use krkr_engine::{
    Engine,
    system::{SystemConfig, SystemHost},
};
use krkr_host_desktop::system::{DesktopSystem, configure};
use support::{Manual, run};
use tjs_runtime::Runtime;

fn engine(lock_directory: &std::path::Path) -> Engine<Manual> {
    let mut config = SystemConfig::for_process().unwrap();
    configure(&mut config).unwrap();
    config.host = Some(Box::new(DesktopSystem::new(lock_directory.to_path_buf())));
    Engine::with_system(
        Runtime::new(),
        Manual::default(),
        Default::default(),
        Default::default(),
        config,
    )
    .unwrap()
}

#[test]
fn script_locks_are_exclusive_survive_reset_and_release_with_host() {
    let directory = tempfile::tempdir().unwrap();
    let mut a = engine(directory.path());
    let mut b = engine(directory.path());
    assert_eq!(
        run(&mut a, r#"System.createAppLock('project\xD800');"#),
        "1"
    );
    assert_eq!(
        run(&mut a, r#"System.createAppLock('project\xD800');"#),
        "0"
    );
    assert_eq!(
        run(&mut b, r#"System.createAppLock('project\xD800\0ignored');"#),
        "0"
    );
    assert_eq!(run(&mut b, r#"System.createAppLock('different');"#), "1");
    a.reset();
    assert_eq!(
        run(&mut b, r#"System.createAppLock('project\xD800');"#),
        "0"
    );
    drop(a);
    assert_eq!(
        run(&mut b, r#"System.createAppLock('project\xD800');"#),
        "1"
    );
    let personal = String::from_utf16(&b.system_config().personal_path).unwrap();
    let app_data = String::from_utf16(&b.system_config().app_data_path).unwrap();
    let saved = String::from_utf16(&b.system_config().saved_games_path).unwrap();
    assert_eq!(run(&mut b, "System.personalPath;"), personal);
    assert_eq!(run(&mut b, "System.appDataPath;"), app_data);
    assert_eq!(run(&mut b, "System.savedGamesPath;"), saved);
    assert!(personal.starts_with("file://") && personal.ends_with('/'));
    assert!(app_data.starts_with("file://") && app_data.ends_with('/'));
    let (mut unconfigured, _) = support::setup(Runtime::new(), Default::default());
    assert_eq!(
        run(
            &mut unconfigured,
            "var caught=false; try{System.createAppLock('missing host');}catch(e){caught=true;} caught && System.personalPath==System.exePath && System.appDataPath==System.exePath;"
        ),
        "1"
    );
}

#[test]
fn app_lock_child() {
    let Some(directory) = std::env::var_os("KRKR_LOCK_TEST_DIRECTORY") else {
        return;
    };
    let mut host = DesktopSystem::new(directory.into());
    let acquired = host
        .create_app_lock(&"process lock".encode_utf16().collect::<Vec<_>>())
        .unwrap();
    assert_eq!(
        acquired,
        std::env::var("KRKR_LOCK_TEST_ACQUIRE").unwrap() == "yes"
    );
}

#[test]
fn lock_exclusion_and_release_are_visible_to_another_process() {
    let directory = tempfile::tempdir().unwrap();
    let mut host = DesktopSystem::new(directory.path().to_owned());
    assert!(
        host.create_app_lock(&"process lock".encode_utf16().collect::<Vec<_>>())
            .unwrap()
    );
    let child = |acquire: &str| {
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "app_lock_child"])
            .env("KRKR_LOCK_TEST_DIRECTORY", directory.path())
            .env("KRKR_LOCK_TEST_ACQUIRE", acquire)
            .status()
            .unwrap();
        assert!(status.success());
    };
    child("no");
    drop(host);
    child("yes");
}
