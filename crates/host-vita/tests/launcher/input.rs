use super::*;
#[test]
fn held_launch_and_touch_are_not_replayed_on_entry() {
    let sample = Sample {
        buttons: 8,
        touch: Some((1, (100, 100))),
        ..Default::default()
    };
    let mut input = Input::new(sample);
    assert!(input.poll(sample, Instant::now()).is_empty());
    input.poll(Sample::default(), Instant::now());
    let events = input.poll(sample, Instant::now());
    assert!(
        events
            .iter()
            .any(|e| matches!(e, Event::Shortcut(Action::Refresh)))
    );
    assert!(
        events
            .iter()
            .any(|e| matches!(e, Event::Ui(InputEvent::PointerDown(_))))
    );
}
#[test]
fn directions_repeat_but_do_not_catch_up_after_a_stall() {
    let now = Instant::now();
    let mut input = Input::new(Sample::default());
    let down = Sample {
        analog: [128, 255],
        ..Default::default()
    };
    assert!(
        input
            .poll(down, now)
            .iter()
            .any(|e| matches!(e, Event::Ui(InputEvent::KeyDown(Key::Down))))
    );
    assert!(
        input
            .poll(down, now + Duration::from_millis(349))
            .is_empty()
    );
    assert_eq!(input.poll(down, now + Duration::from_secs(10)).len(), 1);
    assert!(input.poll(down, now + Duration::from_secs(10)).is_empty());
}
#[test]
fn replacement_finger_cancels_drag_until_all_contacts_are_released() {
    let now = Instant::now();
    let mut input = Input::new(Sample::default());
    input.poll(
        Sample {
            touch: Some((1, (100, 100))),
            ..Default::default()
        },
        now,
    );
    let next = Sample {
        touch: Some((2, (300, 200))),
        ..Default::default()
    };
    assert!(matches!(
        input.poll(next, now).as_slice(),
        [Event::CancelPointer]
    ));
    assert!(input.poll(next, now).is_empty());
    assert!(input.poll(Sample::default(), now).is_empty());
    assert!(matches!(
        input.poll(next, now).as_slice(),
        [Event::Ui(InputEvent::PointerDown(_))]
    ));
}
