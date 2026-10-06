//! Original driver-format SGX543 shaders, embedded by the host build.
use crate::{Error, Result};
use glow::HasContext;
use std::ffi::c_void;

#[cfg(feature = "sgx-binaries")]
include!(concat!(env!("OUT_DIR"), "/sgx_catalog.rs"));
#[cfg(not(feature = "sgx-binaries"))]
static BINARIES: &[(u32, &str, &[u8])] = &[];

type Upload = unsafe extern "C" fn(i32, *const u32, u32, *const c_void, i32);
const SGX_BINARY: u32 = 0x8C0A;

pub(crate) struct Loader {
    upload: Option<Upload>,
}
impl Loader {
    pub fn new(_gl: &glow::Context) -> Self {
        #[allow(unused_mut)]
        let mut upload = None;
        #[cfg(all(target_os = "vita", feature = "sgx-binaries"))]
        unsafe {
            if _gl.supported_extensions().contains("GL_IMG_shader_binary")
                && _gl.get_parameter_string(glow::RENDERER).contains("SGX 543")
            {
                let count = _gl.get_parameter_i32(glow::NUM_SHADER_BINARY_FORMATS);
                if (1..=32).contains(&count) {
                    let mut formats = vec![0; count as usize];
                    _gl.get_parameter_i32_slice(glow::SHADER_BINARY_FORMATS, &mut formats);
                    if formats.contains(&(SGX_BINARY as i32)) {
                        let address =
                            pvr_psp2_sys::gles::eglGetProcAddress(c"glShaderBinary".as_ptr());
                        if !address.is_null() {
                            upload = Some(std::mem::transmute::<*const c_void, Upload>(address));
                        }
                    }
                }
            }
        }
        Self { upload }
    }

    pub fn load(
        &self,
        gl: &glow::Context,
        shader: glow::NativeShader,
        kind: u32,
        source: &str,
    ) -> Result<bool> {
        let Some(upload) = self.upload else {
            return Ok(false);
        };
        // Compare the complete source, including precision and specialization.
        // Shader changes can never accidentally reuse a stale executable.
        let Some((_, _, binary)) = BINARIES
            .iter()
            .find(|(stage, text, _)| *stage == kind && *text == source)
        else {
            return Ok(false);
        };
        self.load_binary(gl, shader, upload, binary)?;
        Ok(true)
    }

    fn load_binary(
        &self,
        gl: &glow::Context,
        shader: glow::NativeShader,
        upload: Upload,
        binary: &[u8],
    ) -> Result<()> {
        unsafe {
            let previous = gl.get_error();
            if previous != glow::NO_ERROR {
                return Err(Error::Backend(format!(
                    "GLES error before shader binary: {previous:#x}"
                )));
            }
            upload(
                1,
                &shader.0.get(),
                SGX_BINARY,
                binary.as_ptr().cast(),
                binary.len() as i32,
            );
            let error = gl.get_error();
            if error != glow::NO_ERROR {
                return Err(Error::Backend(format!(
                    "GLES shader binary upload: {error:#x}"
                )));
            }
            if !gl.get_shader_compile_status(shader) {
                return Err(Error::Backend(format!(
                    "GLES shader binary rejected: {}",
                    gl.get_shader_info_log(shader)
                )));
            }
            Ok(())
        }
    }
}

#[cfg(all(
    test,
    any(target_os = "linux", all(windows, feature = "windows-gles-tests"))
))]
#[path = "../tests/internal/shader_binary.rs"]
mod tests;
