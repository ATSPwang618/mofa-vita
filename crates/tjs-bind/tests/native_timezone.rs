//! Isolated process: installing a platform calendar must also reach fresh VMs
//! and archive workers, without changing the OS timezone or other test suites.
use jiff::{Timestamp, civil::DateTime, tz::Offset};
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering::Relaxed};
use tjs_bind::{NativeError, NativeResult, date::timezone};
use tjs_core::{RunBudget, SourceMap, Vm, VmExit};

static OFFSET: AtomicI32 = AtomicI32::new(8 * 3600);
static FAIL: AtomicBool = AtomicBool::new(false);
fn offset() -> NativeResult<Offset> {
    if FAIL.load(Relaxed) {
        return Err(NativeError::Message("RTC unavailable"));
    }
    Offset::from_seconds(OFFSET.load(Relaxed)).map_err(|e| NativeError::Detail(e.to_string()))
}
fn local(timestamp: Timestamp) -> NativeResult<DateTime> {
    Ok(offset()?.to_datetime(timestamp))
}
fn utc(date: DateTime) -> NativeResult<Timestamp> {
    offset()?
        .to_timestamp(date)
        .map_err(|e| NativeError::Detail(e.to_string()))
}
fn check(script: &str, expected: &str) {
    let mut sources = SourceMap::new();
    let source = sources.add_utf8("native timezone", script).unwrap();
    let module = tjs_front::compile(&sources, source).unwrap();
    for slice in [1, 10000] {
        let mut heap = tjs_bind::new_heap();
        let mut vm = Vm::new(&module);
        loop {
            match vm.run_slice(&mut heap, RunBudget::new(slice).unwrap()) {
                VmExit::Finished(value) => {
                    assert_eq!(heap.display(value).unwrap(), expected, "{script}");
                    break;
                }
                VmExit::Yielded => {
                    heap.collect(vm.roots());
                }
                other => panic!("{script}: {other:?}"),
            }
            assert!(vm.work_executed() < 100000);
        }
    }
}

#[test]
fn native_calendar_covers_date_construction_parsing_setters_offsets_and_workers() {
    timezone::install(timezone::Host {
        to_local: local,
        to_utc: utc,
    })
    .unwrap();
    for (script, expected) in [
        ("(new Date(1970,0,1)).getTime();", "-28800000"),
        (
            "var d=new Date('1970/1/1 00:00 GMT'); d.getTime()+':'+d.getHours()+':'+d.getTimezoneOffset();",
            "0:8:-480",
        ),
        ("var d=new Date('1970/1/1 08:00'); d.getTime();", "0"),
        (
            "var d=new Date();d.setTime(0);d.setHours(9);d.getTime();",
            "3600000",
        ),
        (
            "var d=new Date(2024,11,31,23,59,59);d.setSeconds(60);d.getYear()+':'+d.getMonth()+':'+d.getDate()+':'+d.getHours();",
            "2025:0:1:0",
        ),
        (
            "var d=new Date(1970,0,1);d.parse('1970/1/1 05:45 +0545');d.getTime();",
            "0",
        ),
    ] {
        check(script, expected);
    }
    // Later conversions must see device setting changes without reinstalling.
    for (seconds, script, expected) in [
        (
            20700,
            "var d=new Date();d.setTime(0);d.getHours()+':'+d.getMinutes()+':'+d.getTimezoneOffset();",
            "5:45:-345",
        ),
        (
            -19800,
            "var d=new Date();d.setTime(0);d.getYear()+':'+d.getMonth()+':'+d.getDate()+':'+d.getDay()+':'+d.getHours()+':'+d.getMinutes()+':'+d.getTimezoneOffset();",
            "1969:11:31:3:18:30:330",
        ),
    ] {
        OFFSET.store(seconds, Relaxed);
        check(script, expected);
        let zone = timezone::system();
        let date = zone.to_datetime(Timestamp::UNIX_EPOCH).unwrap();
        assert_eq!(zone.to_timestamp(date).unwrap(), Timestamp::UNIX_EPOCH);
        assert_eq!(
            std::thread::spawn(|| timezone::system()
                .to_datetime(Timestamp::UNIX_EPOCH)
                .unwrap())
            .join()
            .unwrap(),
            date
        );
    }
    FAIL.store(true, Relaxed);
    check(
        "var n=0;try { var d=new Date(2024,0,1); } catch(e) {n++;} var d=new Date();d.setTime(0);try { var h=d.getHours(); } catch(e) {n++;} try { var z=d.getTimezoneOffset(); } catch(e) {n++;} n;",
        "3",
    );
}
