#![cfg(feature = "profiling")]
use krkr_protocol::{
    budget::Budget,
    diagnostics::{Level, Timer},
    profile::{self, Event},
};

#[test]
fn recording_is_independent_of_console_and_tracks_reservations_and_thread_origins() {
    krkr_protocol::diagnostics::set_level(Level::Warn);
    let (session, receiver) = profile::Session::start(128).unwrap();
    assert!(profile::Session::start(128).is_err());
    let timer = Timer::start();
    let budget = Budget::new(1024);
    budget.set_profile_name("test.bytes");
    {
        let _span = profile::span_detail("allocation", || "fixture".into());
        let _permit = budget.reserve(512).unwrap();
        assert!(budget.reserve(1024).is_err());
    }
    std::thread::Builder::new()
        .name("receiver".into())
        .spawn(move || timer.report(|| "stage=io-queue kind=test".into()))
        .unwrap()
        .join()
        .unwrap();
    assert_eq!(session.finish(), 0);
    let events: Vec<_> = receiver.into_iter().collect();
    let bytes: Vec<_> = events
        .iter()
        .filter_map(|e| {
            if let Event::Counter { name, value, .. } = e {
                (name == "test.bytes").then_some(*value)
            } else {
                None
            }
        })
        .collect();
    assert_eq!(bytes, vec![0, 512, 0]);
    let allocation = events
        .iter()
        .find_map(|e| {
            if let Event::Span { thread, name, .. } = e {
                (name == "allocation").then_some(*thread)
            } else {
                None
            }
        })
        .unwrap();
    assert!(events.iter().any(
        |e| matches!(e,Event::Span{thread,name,..} if *thread==allocation && name=="io-queue")
    ));
    profile::span_detail("disabled", || panic!("disabled recorder evaluated detail"));
    let (session, receiver) = profile::Session::start(1).unwrap();
    for _ in 0..8 {
        profile::counter("bounded", 1);
    }
    assert!(session.finish() > 0);
    assert_eq!(receiver.into_iter().count(), 1);
}
