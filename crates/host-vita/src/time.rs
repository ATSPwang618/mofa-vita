//! Device-local calendar conversion, following ons-rs's SceRtc path. Unix
//! timestamps remain UTC; only calendar conversion consults device settings.
#![allow(unsafe_code)]
use jiff::{Timestamp, civil::DateTime, tz::TimeZone};
use tjs_bind::{NativeError, NativeResult, date::timezone};
use vitasdk_sys::{
    SceDateTime, SceRtcTick, sceRtcConvertLocalTimeToUtc, sceRtcConvertUtcToLocalTime,
    sceRtcGetTick, sceRtcSetTick,
};

pub fn install() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        assert!(
            timezone::install(timezone::Host { to_local, to_utc }).is_ok(),
            "Vita timezone host already installed"
        );
    });
}
fn checked(operation: &str, status: i32) -> NativeResult<()> {
    if status < 0 {
        Err(NativeError::Detail(format!(
            "{operation}: RTC error {status:#010x}"
        )))
    } else {
        Ok(())
    }
}
fn tick(date: DateTime) -> NativeResult<SceRtcTick> {
    if date.year() < 1 {
        return Err(NativeError::Message("Vita RTC requires a year in 1..=9999"));
    }
    let date = SceDateTime {
        year: date.year() as u16,
        month: date.month() as u16,
        day: date.day() as u16,
        hour: date.hour() as u16,
        minute: date.minute() as u16,
        second: date.second() as u16,
        microsecond: date.nanosecond() as u32 / 1000,
    };
    let mut tick = SceRtcTick { tick: 0 };
    // SAFETY: initialized calendar input and exclusive output with SDK layout.
    checked("sceRtcGetTick", unsafe { sceRtcGetTick(&date, &mut tick) })?;
    Ok(tick)
}
fn calendar(tick: &SceRtcTick) -> NativeResult<DateTime> {
    let mut date = std::mem::MaybeUninit::<SceDateTime>::uninit();
    // SAFETY: SDK initializes the output on success, checked before reading it.
    checked("sceRtcSetTick", unsafe {
        sceRtcSetTick(date.as_mut_ptr(), tick)
    })?;
    let date = unsafe { date.assume_init() };
    DateTime::new(
        date.year as i16,
        date.month as i8,
        date.day as i8,
        date.hour as i8,
        date.minute as i8,
        date.second as i8,
        (date.microsecond * 1000) as i32,
    )
    .map_err(|e| NativeError::Detail(e.to_string()))
}
fn to_local(timestamp: Timestamp) -> NativeResult<DateTime> {
    let utc = tick(TimeZone::UTC.to_datetime(timestamp))?;
    let mut local = SceRtcTick { tick: 0 };
    // SAFETY: separate initialized SDK tick structures, no retained pointers.
    checked("sceRtcConvertUtcToLocalTime", unsafe {
        sceRtcConvertUtcToLocalTime(&utc, &mut local)
    })?;
    calendar(&local)
}
fn to_utc(date: DateTime) -> NativeResult<Timestamp> {
    let local = tick(date)?;
    let mut utc = SceRtcTick { tick: 0 };
    // SAFETY: separate initialized SDK tick structures, no retained pointers.
    checked("sceRtcConvertLocalTimeToUtc", unsafe {
        sceRtcConvertLocalTimeToUtc(&local, &mut utc)
    })?;
    TimeZone::UTC
        .to_timestamp(calendar(&utc)?)
        .map_err(|e| NativeError::Detail(e.to_string()))
}
