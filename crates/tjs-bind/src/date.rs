//! Jiff calendar arithmetic with system or host-native timezone conversion.
//! Adapts TJS's zero-based months, seconds and mktime-style component overflow.
mod parse;
pub mod timezone;
use crate::{NativeCx, NativeError, NativeResult, RestArgs, Value};
use jiff::{
    Span, Timestamp,
    civil::DateTime,
    tz::{Dst, Offset, TimeZone},
};
use tjs_core::value;

fn invalid() -> NativeError {
    NativeError::Message("invalid value for timestamp")
}
fn error(e: jiff::Error) -> NativeError {
    NativeError::Detail(e.to_string())
}

#[crate::class(name = "Date")]
mod implementation {
    use super::*;
    #[derive(Default, crate::Trace)]
    pub struct State {
        seconds: i64,
    }
    impl State {
        #[tjs::method]
        fn finalize(&self) {}

        #[tjs::constructor]
        fn new(cx: &mut NativeCx<'_>, args: RestArgs<'_>) -> NativeResult<Self> {
            let Some(&first) = args.first() else {
                return Ok(Self {
                    seconds: Timestamp::now().as_second(),
                });
            };
            if matches!(first, Value::Str(_)) {
                return Ok(Self {
                    seconds: parse::parse(cx.heap(), first)?,
                });
            }
            let mut fields = [0, 0, 1, 0, 0, 0];
            for (i, field) in fields.iter_mut().enumerate() {
                if let Some(&arg) = args
                    .get(i)
                    .filter(|&&arg| i == 0 || !matches!(arg, Value::Void))
                {
                    *field = value::to_integer(cx.heap(), arg)? as i32;
                }
            }
            Ok(Self {
                seconds: local_timestamp(normalize(fields)?, Dst::No)?,
            })
        }
        fn local(&self, result_needed: bool) -> NativeResult<Option<DateTime>> {
            let local = Timestamp::from_second(self.seconds)
                .map_err(error)
                .and_then(|timestamp| timezone::system().to_datetime(timestamp));
            // localtime runs even with a null result pointer, but its null
            // return is only dereferenced when the caller requests a value.
            if result_needed {
                local.map(Some)
            } else {
                Ok(None)
            }
        }
        fn set(&mut self, cx: &mut NativeCx<'_>, field: usize, value: Value) -> NativeResult<()> {
            let timestamp = Timestamp::from_second(self.seconds).map_err(error)?;
            let zone = timezone::system();
            let dt = zone.to_datetime(timestamp)?;
            let mut fields = [
                i32::from(dt.year()),
                i32::from(dt.month()) - 1,
                i32::from(dt.day()),
                i32::from(dt.hour()),
                i32::from(dt.minute()),
                i32::from(dt.second()),
            ];
            fields[field] = value::to_integer(cx.heap(), value)? as i32;
            let replacement = normalize(fields)
                .and_then(|date| local_timestamp_in(&zone, date, zone.dst(timestamp)));
            // mktime's -1 is stored before the reference checks for failure.
            // Argument-conversion errors above still preserve the old time.
            self.seconds = replacement.as_ref().copied().unwrap_or(-1);
            replacement.map(|_| ())
        }
        #[tjs::method(name = "setYear")]
        fn set_year(&mut self, cx: &mut NativeCx<'_>, value: Value) -> NativeResult<()> {
            self.set(cx, 0, value)
        }
        #[tjs::method(name = "setMonth")]
        fn set_month(&mut self, cx: &mut NativeCx<'_>, value: Value) -> NativeResult<()> {
            self.set(cx, 1, value)
        }
        #[tjs::method(name = "setDate")]
        fn set_date(&mut self, cx: &mut NativeCx<'_>, value: Value) -> NativeResult<()> {
            self.set(cx, 2, value)
        }
        #[tjs::method(name = "setHours")]
        fn set_hours(&mut self, cx: &mut NativeCx<'_>, value: Value) -> NativeResult<()> {
            self.set(cx, 3, value)
        }
        #[tjs::method(name = "setMinutes")]
        fn set_minutes(&mut self, cx: &mut NativeCx<'_>, value: Value) -> NativeResult<()> {
            self.set(cx, 4, value)
        }
        #[tjs::method(name = "setSeconds")]
        fn set_seconds(&mut self, cx: &mut NativeCx<'_>, value: Value) -> NativeResult<()> {
            self.set(cx, 5, value)
        }
        #[tjs::method(name = "getYear")]
        fn get_year(&self, cx: &mut NativeCx<'_>) -> NativeResult<i64> {
            Ok(self
                .local(cx.result_needed())?
                .map_or(0, |dt| i64::from(dt.year())))
        }
        #[tjs::method(name = "getMonth")]
        fn get_month(&self, cx: &mut NativeCx<'_>) -> NativeResult<i64> {
            Ok(self
                .local(cx.result_needed())?
                .map_or(0, |dt| i64::from(dt.month()) - 1))
        }
        #[tjs::method(name = "getDate")]
        fn get_date(&self, cx: &mut NativeCx<'_>) -> NativeResult<i64> {
            Ok(self
                .local(cx.result_needed())?
                .map_or(0, |dt| i64::from(dt.day())))
        }
        #[tjs::method(name = "getDay")]
        fn get_day(&self, cx: &mut NativeCx<'_>) -> NativeResult<i64> {
            Ok(self
                .local(cx.result_needed())?
                .map_or(0, |dt| i64::from(dt.weekday().to_sunday_zero_offset())))
        }
        #[tjs::method(name = "getHours")]
        fn get_hours(&self, cx: &mut NativeCx<'_>) -> NativeResult<i64> {
            Ok(self
                .local(cx.result_needed())?
                .map_or(0, |dt| i64::from(dt.hour())))
        }
        #[tjs::method(name = "getMinutes")]
        fn get_minutes(&self, cx: &mut NativeCx<'_>) -> NativeResult<i64> {
            Ok(self
                .local(cx.result_needed())?
                .map_or(0, |dt| i64::from(dt.minute())))
        }
        #[tjs::method(name = "getSeconds")]
        fn get_seconds(&self, cx: &mut NativeCx<'_>) -> NativeResult<i64> {
            Ok(self
                .local(cx.result_needed())?
                .map_or(0, |dt| i64::from(dt.second())))
        }
        #[tjs::method(name = "getTime")]
        fn get_time(&self) -> i64 {
            self.seconds * 1000
        }
        #[tjs::method(name = "setTime")]
        fn set_time(&mut self, cx: &mut NativeCx<'_>, value: Value) -> NativeResult<()> {
            self.seconds = value::to_integer(cx.heap(), value)? / 1000;
            Ok(())
        }
        #[tjs::method(name = "getTimezoneOffset")]
        fn get_timezone_offset(cx: &mut NativeCx<'_>) -> NativeResult<i64> {
            if !cx.result_needed() {
                return Ok(0);
            }
            Ok(-i64::from(
                timezone::system()
                    .standard_offset(Timestamp::now())?
                    .seconds(),
            ) / 60)
        }
        #[tjs::method]
        fn parse(&mut self, cx: &mut NativeCx<'_>, text: Value) -> NativeResult<()> {
            self.seconds = parse::parse(cx.heap(), text)?;
            Ok(())
        }
    }
}
pub use implementation::{CLASS, install};

fn normalize([year, month, day, hour, minute, second]: [i32; 6]) -> NativeResult<DateTime> {
    let year = i64::from(year) + i64::from(month).div_euclid(12);
    let month = month.rem_euclid(12) + 1;
    let year = i16::try_from(year).map_err(|_| invalid())?;
    let base = DateTime::new(year, month as i8, 1, 0, 0, 0, 0).map_err(error)?;
    // Combine overflowing components before applying Jiff's calendar bounds:
    // large day/hour offsets can cancel without an out-of-range intermediate.
    let offset = (i64::from(day) - 1) * 86400
        + i64::from(hour) * 3600
        + i64::from(minute) * 60
        + i64::from(second);
    base.checked_add(Span::new().try_seconds(offset).map_err(error)?)
        .map_err(error)
}

fn offset_for_dst(zone: &TimeZone, timestamp: Timestamp, dst: Dst) -> Offset {
    let current = zone.to_offset_info(timestamp);
    if current.dst() == dst {
        return current.offset();
    }
    let previous = zone.preceding(timestamp).find(|t| t.dst() == dst);
    let following = zone.following(timestamp).find(|t| t.dst() == dst);
    match (previous, following) {
        (Some(a), Some(b)) => {
            if timestamp.as_second() - a.timestamp().as_second()
                < b.timestamp().as_second() - timestamp.as_second()
            {
                a.offset()
            } else {
                b.offset()
            }
        }
        (Some(t), None) | (None, Some(t)) => t.offset(),
        (None, None) => current.offset(),
    }
}
fn local_timestamp(date: DateTime, dst: Dst) -> NativeResult<i64> {
    local_timestamp_in(&timezone::system(), date, dst)
}
fn local_timestamp_in(zone: &timezone::Zone, date: DateTime, dst: Dst) -> NativeResult<i64> {
    let seconds = zone.timestamp_with_dst(date, dst)?.as_second();
    if seconds == -1 {
        return Err(invalid());
    }
    Ok(seconds)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn component_overflow_is_normalized_together_and_dst_is_explicit() {
        assert_eq!(
            normalize([2024, 0, 1000001, -24000000, 0, 0]).unwrap(),
            DateTime::new(2024, 1, 1, 0, 0, 0, 0).unwrap()
        );
        let zone = TimeZone::get("America/New_York").unwrap().into();
        for (fields, dst, expected) in [
            ([2024, 2, 10, 2, 30, 0], Dst::No, "2024-03-10T07:30:00Z"),
            ([2024, 2, 10, 2, 30, 0], Dst::Yes, "2024-03-10T06:30:00Z"),
            ([2024, 10, 3, 1, 30, 0], Dst::No, "2024-11-03T06:30:00Z"),
            ([2024, 10, 3, 1, 30, 0], Dst::Yes, "2024-11-03T05:30:00Z"),
            ([2024, 6, 1, 12, 0, 0], Dst::No, "2024-07-01T17:00:00Z"),
        ] {
            assert_eq!(
                local_timestamp_in(&zone, normalize(fields).unwrap(), dst).unwrap(),
                expected.parse::<Timestamp>().unwrap().as_second()
            );
        }
    }
}
