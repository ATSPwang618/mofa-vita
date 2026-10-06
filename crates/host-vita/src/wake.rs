//! A retained wake token, independent of pthread's absolute timeout clock.
use std::{sync::Arc, time::Duration};

#[cfg(not(target_os = "vita"))]
pub struct Wake(std::thread::Thread);
#[cfg(not(target_os = "vita"))]
impl Wake {
    pub fn new() -> Result<Arc<Self>, String> {
        Ok(Arc::new(Self(std::thread::current())))
    }
    pub fn signal(&self) {
        self.0.unpark();
    }
    pub fn wait(&self, timeout: Duration) -> Result<(), String> {
        std::thread::park_timeout(timeout);
        Ok(())
    }
    pub fn wait_forever(&self) -> Result<(), String> {
        std::thread::park();
        Ok(())
    }
}

#[cfg(target_os = "vita")]
pub use native::Wake;
#[cfg(target_os = "vita")]
#[allow(unsafe_code)]
mod native {
    use super::*;
    use vitasdk_sys::*;

    pub struct Wake(SceUID);
    impl Wake {
        pub fn new() -> Result<Arc<Self>, String> {
            // One waiter, any number of notifiers. Bit 1 retains signals sent
            // before wait; clearing it is atomic with a successful wait.
            let id = unsafe {
                sceKernelCreateEventFlag(c"krkr-wake".as_ptr(), 0, 0, std::ptr::null_mut())
            };
            if id < 0 {
                return Err(format!("create wake event: {id:#010x}"));
            }
            Ok(Arc::new(Self(id)))
        }
        pub fn signal(&self) {
            // Arc keeps the kernel object alive across wait and notification.
            let result = unsafe { sceKernelSetEventFlag(self.0, 1) };
            if result < 0 {
                krkr_protocol::log!(Warn, "[VITA][WAKE] signal failed: {result:#010x}");
            }
        }
        pub fn wait(&self, timeout: Duration) -> Result<(), String> {
            self.wait_inner(Some(timeout))
        }
        pub fn wait_forever(&self) -> Result<(), String> {
            self.wait_inner(None)
        }
        fn wait_inner(&self, timeout: Option<Duration>) -> Result<(), String> {
            // Vita's pthread condvar accepts a monotonic attribute but the
            // SDK's pte_relmillisecs subtracts ftime (wall time). Use the
            // kernel's relative microsecond timeout directly instead.
            let mut micros = timeout.map_or(0, |timeout| {
                timeout.as_nanos().div_ceil(1000).min(u128::from(u32::MAX)) as u32
            });
            let limit = if timeout.is_some() {
                &mut micros
            } else {
                std::ptr::null_mut()
            };
            let result = unsafe {
                sceKernelWaitEventFlag(
                    self.0,
                    1,
                    SCE_EVENT_WAITCLEAR_PAT,
                    std::ptr::null_mut(),
                    limit,
                )
            };
            if result < 0 && result as u32 != SCE_KERNEL_ERROR_WAIT_TIMEOUT {
                return Err(format!("wait wake event: {result:#010x}"));
            }
            Ok(())
        }
    }
    impl Drop for Wake {
        fn drop(&mut self) {
            unsafe { sceKernelDeleteEventFlag(self.0) };
        }
    }
}
