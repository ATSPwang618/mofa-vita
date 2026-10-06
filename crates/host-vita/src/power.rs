//! Scoped clocks following the existing ons-rs Vita host's operating point.
#![allow(unsafe_code)]
use vitasdk_sys::*;
type Setter = unsafe extern "C" fn(i32) -> i32;
pub struct Clocks([(Setter, i32); 4]);
impl Clocks {
    pub fn acquire() -> Result<Self, String> {
        // SAFETY: getters have no pointer arguments or ownership transfers.
        let previous = unsafe {
            [
                scePowerGetArmClockFrequency(),
                scePowerGetBusClockFrequency(),
                scePowerGetGpuClockFrequency(),
                scePowerGetGpuXbarClockFrequency(),
            ]
        };
        if previous.iter().any(|&value| value < 0) {
            return Err("cannot read Vita clock frequencies".into());
        }
        let clocks = Self([
            (scePowerSetArmClockFrequency, previous[0]),
            (scePowerSetBusClockFrequency, previous[1]),
            (scePowerSetGpuClockFrequency, previous[2]),
            (scePowerSetGpuXbarClockFrequency, previous[3]),
        ]);
        for ((set, _), frequency) in clocks.0.iter().zip([444, 222, 222, 166]) {
            // SAFETY: SDK setter and documented scalar frequency. A failure
            // drops the guard, restoring even partially applied settings.
            let result = unsafe { set(frequency) };
            if result < 0 {
                return Err(format!("setting Vita clock: 0x{result:08x}"));
            }
        }
        Ok(clocks)
    }
}
impl Drop for Clocks {
    fn drop(&mut self) {
        for (set, previous) in self.0 {
            // SAFETY: these frequencies were read from this device on acquire.
            unsafe {
                set(previous);
            }
        }
    }
}
