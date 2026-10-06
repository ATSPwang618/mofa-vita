use libloading::Library;
use std::ffi::{CString, c_char, c_void};
type Handle = *mut c_void;
type Int = i32;

/// A real ES2 pbuffer: surfaceless Mesa on Linux, ANGLE on Windows.
/// The backend itself has no EGL dependency.
pub struct Context {
    #[cfg(windows)]
    _serial: std::sync::MutexGuard<'static, ()>,
    egl: Library,
    gles: Library,
    display: Handle,
    surface: Handle,
    context: Handle,
}
impl Context {
    pub fn new() -> Self {
        Self::sized(64, 64)
    }
    pub fn sized(width: i32, height: i32) -> Self {
        // ANGLE's default display is shared; terminating one test's display
        // must not invalidate another thread's active context.
        #[cfg(windows)]
        let serial = {
            static CONTEXT: std::sync::Mutex<()> = std::sync::Mutex::new(());
            CONTEXT.lock().unwrap_or_else(|error| error.into_inner())
        };
        unsafe {
            let (egl_name, gles_name) = if cfg!(windows) {
                ("libEGL.dll", "libGLESv2.dll")
            } else {
                ("libEGL.so.1", "libGLESv2.so.2")
            };
            let egl = Library::new(egl_name).expect("install EGL or add ANGLE DLLs to PATH");
            let gles = Library::new(gles_name).expect("install GLES2 or add ANGLE DLLs to PATH");
            let display = egl
                .get::<unsafe extern "C" fn(Handle) -> Handle>(b"eglGetDisplay\0")
                .unwrap()(std::ptr::null_mut());
            assert!(!display.is_null());
            let mut major = 0;
            let mut minor = 0;
            assert_ne!(
                egl.get::<unsafe extern "C" fn(Handle, *mut Int, *mut Int) -> u32>(
                    b"eglInitialize\0"
                )
                .unwrap()(display, &mut major, &mut minor),
                0
            );
            assert_ne!(
                egl.get::<unsafe extern "C" fn(u32) -> u32>(b"eglBindAPI\0")
                    .unwrap()(0x30A0),
                0
            );
            let attributes = [
                0x3033, 1, 0x3040, 4, 0x3024, 8, 0x3023, 8, 0x3022, 8, 0x3021, 8, 0x3038,
            ];
            let mut config = std::ptr::null_mut();
            let mut count = 0;
            assert_ne!(egl.get::<unsafe extern "C" fn(Handle, *const Int, *mut Handle, Int, *mut Int) -> u32>(b"eglChooseConfig\0").unwrap()(display, attributes.as_ptr(), &mut config, 1, &mut count), 0);
            assert_eq!(count, 1);
            let surface = egl
                .get::<unsafe extern "C" fn(Handle, Handle, *const Int) -> Handle>(
                    b"eglCreatePbufferSurface\0",
                )
                .unwrap()(
                display,
                config,
                [0x3057, width, 0x3056, height, 0x3038].as_ptr(),
            );
            assert!(!surface.is_null());
            let context_attributes = if cfg!(windows) {
                // ANGLE otherwise promotes the request to a compatible ES3
                // context. Keep the same ES2 restriction as the Mesa tests.
                vec![0x3098, 2, 0x3483, 0, 0x3038]
            } else {
                vec![0x3098, 2, 0x3038]
            };
            let context = egl
                .get::<unsafe extern "C" fn(Handle, Handle, Handle, *const Int) -> Handle>(
                    b"eglCreateContext\0",
                )
                .unwrap()(
                display,
                config,
                std::ptr::null_mut(),
                context_attributes.as_ptr(),
            );
            assert!(!context.is_null());
            assert_ne!(
                egl.get::<unsafe extern "C" fn(Handle, Handle, Handle, Handle) -> u32>(
                    b"eglMakeCurrent\0"
                )
                .unwrap()(display, surface, surface, context),
                0
            );
            Self {
                #[cfg(windows)]
                _serial: serial,
                egl,
                gles,
                display,
                surface,
                context,
            }
        }
    }
    #[allow(dead_code)] // Shared by integration test binaries using different loaders.
    pub fn gl(&self) -> glow::Context {
        self.gl_with(|_, address| address)
    }
    #[allow(dead_code)]
    pub fn swap(&self) {
        unsafe {
            assert_ne!(
                self.egl
                    .get::<unsafe extern "C" fn(Handle, Handle) -> u32>(b"eglSwapBuffers\0")
                    .unwrap()(self.display, self.surface),
                0
            );
        }
    }
    pub fn gl_with(
        &self,
        mut intercept: impl FnMut(&str, *const c_void) -> *const c_void,
    ) -> glow::Context {
        unsafe {
            let get = self
                .egl
                .get::<unsafe extern "C" fn(*const c_char) -> *const c_void>(b"eglGetProcAddress\0")
                .unwrap();
            let gl = glow::Context::from_loader_function(|name| {
                let symbol = CString::new(name).unwrap();
                let address = get(symbol.as_ptr());
                let address = if !address.is_null() {
                    address
                } else {
                    self.gles
                        .get::<*const c_void>(symbol.as_bytes_with_nul())
                        .map(|value| *value)
                        .unwrap_or(std::ptr::null())
                };
                intercept(name, address)
            });
            use glow::HasContext;
            assert!(
                gl.get_parameter_string(glow::VERSION)
                    .starts_with("OpenGL ES 2.0"),
                "expected ES2; on Mesa run with MESA_GLES_VERSION_OVERRIDE=2.0; got {}",
                gl.get_parameter_string(glow::VERSION)
            );
            gl
        }
    }
}
impl Drop for Context {
    fn drop(&mut self) {
        unsafe {
            self.egl
                .get::<unsafe extern "C" fn(Handle, Handle, Handle, Handle) -> u32>(
                    b"eglMakeCurrent\0",
                )
                .unwrap()(
                self.display,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            );
            self.egl
                .get::<unsafe extern "C" fn(Handle, Handle) -> u32>(b"eglDestroyContext\0")
                .unwrap()(self.display, self.context);
            self.egl
                .get::<unsafe extern "C" fn(Handle, Handle) -> u32>(b"eglDestroySurface\0")
                .unwrap()(self.display, self.surface);
            self.egl
                .get::<unsafe extern "C" fn(Handle) -> u32>(b"eglTerminate\0")
                .unwrap()(self.display);
        }
    }
}
