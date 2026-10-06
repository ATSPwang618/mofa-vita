use krkr_protocol::diagnostics::{self, Level, Timer};
#[test]
fn debug_does_not_print_per_frame_timings_and_errors_have_their_own_level() {
    diagnostics::set_enabled(true);
    assert!(diagnostics::allows(Level::Debug));
    assert!(!diagnostics::allows(Level::Trace));
    Timer::start().report(|| panic!("debug should not format timing details"));
    diagnostics::set_enabled(false);
    assert!(diagnostics::allows(Level::Error));
    assert!(diagnostics::allows(Level::Warn));
    assert!(!diagnostics::allows(Level::Info));
    diagnostics::set_level("trace".parse().unwrap());
    assert!(diagnostics::allows(Level::Trace));
    diagnostics::set_level(Level::Off);
    assert!(!diagnostics::allows(Level::Error));
    assert!("unknown".parse::<Level>().is_err());
}
