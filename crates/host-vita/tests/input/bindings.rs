use super::*;

#[test]
fn settings_round_trip_and_invalid_chords_do_not_replace_defaults() {
    let root = tempfile::tempdir().unwrap();
    assert_eq!(Bindings::load(root.path()).unwrap(), Bindings::default());
    let mut settings = Bindings {
        top: true,
        ..Default::default()
    };
    settings.entries.push(Binding {
        buttons: L | CIRCLE,
        action: Action::Key {
            key: 83,
            modifiers: 5,
        },
    });
    settings.save(root.path()).unwrap();
    assert_eq!(Bindings::load(root.path()).unwrap(), settings);
    settings.entries.pop();
    settings.save(root.path()).unwrap();
    assert_eq!(Bindings::load(root.path()).unwrap(), settings);
    for text in [
        "KRKR-INPUT\t1\nbind\t9\tkey\t13\t0\n",
        "KRKR-INPUT\t1\nbind\t2000\tkey\t83\t8\n",
        "KRKR-INPUT\t1\nbind\t2000\tmouse\t3\n",
        "KRKR-INPUT\t1\nbind\t2000\tkey\t13\t0\nbind\t2000\tkey\t32\t0\n",
        "KRKR-INPUT\t1\nbind\t10000\tkey\t13\t0\n",
    ] {
        assert!(Bindings::parse(text).is_err(), "{text}");
    }
}
#[test]
fn specific_chord_consumes_its_buttons_and_merged_keys_keep_modifiers() {
    let mut map = Bindings::default();
    map.entries.push(Binding {
        buttons: L | CIRCLE,
        action: Action::Key {
            key: 83,
            modifiers: 4,
        },
    });
    let (out, claimed) = map.resolve(L | CIRCLE | TRIANGLE, VALID);
    assert_eq!(claimed, L | CIRCLE);
    assert!(out.keys.contains(17));
    assert!(out.keys.contains(83));
    assert!(!out.keys.contains(33));
    assert_eq!(out.mouse, 0);
    let (out, _) = map.resolve(TRIANGLE, VALID);
    assert!(out.keys.contains(17));
    assert!(!out.keys.contains(83));
    let (out, _) = map.resolve(CIRCLE, 0);
    assert_eq!(out, Output::default());
}
