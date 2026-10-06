//! System calendar conversion. Hosts without zoneinfo supply native UTC/local
//! conversions; desktop Jiff retains its timezone and DST rules.
use super::{NativeResult, offset_for_dst};
use jiff::{
    Timestamp,
    civil::DateTime,
    tz::{Dst, Offset, TimeZone},
};
use std::sync::OnceLock;

#[derive(Clone, Copy, Debug)]
pub struct Host {
    pub to_local: fn(Timestamp) -> NativeResult<DateTime>,
    pub to_utc: fn(DateTime) -> NativeResult<Timestamp>,
}
static HOST: OnceLock<Host> = OnceLock::new();

/// Install once for the process, before running scripts or archive workers.
pub fn install(host: Host) -> Result<(), Host> {
    HOST.set(host)
}

enum Backend {
    Jiff(TimeZone),
    Host(&'static Host),
}
pub struct Zone(Backend);
impl From<TimeZone> for Zone {
    fn from(zone: TimeZone) -> Self {
        Self(Backend::Jiff(zone))
    }
}
pub fn system() -> Zone {
    match HOST.get() {
        Some(host) => Zone(Backend::Host(host)),
        None => TimeZone::system().into(),
    }
}
impl Zone {
    pub fn to_datetime(&self, timestamp: Timestamp) -> NativeResult<DateTime> {
        match &self.0 {
            Backend::Jiff(zone) => Ok(zone.to_datetime(timestamp)),
            Backend::Host(host) => (host.to_local)(timestamp),
        }
    }
    pub fn to_timestamp(&self, date: DateTime) -> NativeResult<Timestamp> {
        match &self.0 {
            Backend::Jiff(zone) => zone.to_timestamp(date).map_err(super::error),
            Backend::Host(host) => (host.to_utc)(date),
        }
    }
    pub(super) fn timestamp_with_dst(&self, date: DateTime, dst: Dst) -> NativeResult<Timestamp> {
        match &self.0 {
            Backend::Jiff(zone) => {
                let approximate = zone.to_timestamp(date).map_err(super::error)?;
                offset_for_dst(zone, approximate, dst)
                    .to_timestamp(date)
                    .map_err(super::error)
            }
            Backend::Host(host) => (host.to_utc)(date),
        }
    }
    pub(super) fn dst(&self, timestamp: Timestamp) -> Dst {
        match &self.0 {
            Backend::Jiff(zone) => zone.to_offset_info(timestamp).dst(),
            // Native RTC conversion owns the device's daylight-saving policy.
            Backend::Host(_) => Dst::No,
        }
    }
    pub(super) fn standard_offset(&self, timestamp: Timestamp) -> NativeResult<Offset> {
        match &self.0 {
            Backend::Jiff(zone) => Ok(offset_for_dst(zone, timestamp, Dst::No)),
            Backend::Host(host) => {
                let local = (host.to_local)(timestamp)?;
                let seconds = Offset::UTC
                    .to_timestamp(local)
                    .map_err(super::error)?
                    .as_second()
                    - timestamp.as_second();
                let seconds = i32::try_from(seconds).map_err(|_| super::invalid())?;
                Offset::from_seconds(seconds).map_err(super::error)
            }
        }
    }
}
