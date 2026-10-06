//! Stream desktop frames to FFmpeg without retaining a frame sequence on disk.
use glow::HasContext;
use std::{
    io::Write,
    path::Path,
    process::{Child, ChildStdin, Command, Stdio},
    time::Duration,
};

pub struct Export {
    process: Child,
    input: Option<ChildStdin>,
    pixels: Vec<u8>,
    frames: u64,
    fps: u32,
}
impl Export {
    pub fn new(
        path: &Path,
        ffmpeg: &Path,
        fps: u32,
        encoder: &str,
        bitrate: u32,
    ) -> Result<Self, String> {
        let mut command = Command::new(ffmpeg);
        command
            .args([
                "-hide_banner",
                "-loglevel",
                "error",
                "-n",
                "-f",
                "rawvideo",
                "-pixel_format",
                "rgba",
                "-video_size",
                "960x544",
                "-framerate",
            ])
            .arg(fps.to_string())
            .args(["-i", "pipe:0", "-an", "-vf", "vflip", "-c:v"])
            .arg(encoder);
        if encoder == "ffv1" {
            command.args(["-level", "3", "-g", "1", "-pix_fmt", "bgra"]);
        } else if encoder == "libx264" {
            command.args(["-preset", "veryfast", "-crf", "18"]);
        } else {
            command.arg("-b:v").arg(bitrate.to_string());
        }
        if encoder != "ffv1" {
            command.args([
                "-pix_fmt",
                "yuv420p",
                "-profile:v",
                "main",
                "-level:v",
                "3.1",
                "-movflags",
                "+faststart",
            ]);
        }
        let mut process = command
            .arg(path)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .spawn()
            .map_err(|e| format!("start FFmpeg: {e}"))?;
        let input = process.stdin.take();
        Ok(Self {
            process,
            input,
            pixels: vec![0; 960 * 544 * 4],
            frames: 0,
            fps,
        })
    }
    pub fn sample(&mut self, gl: &glow::Context, elapsed: Duration) -> Result<(), String> {
        let due = frame_count(elapsed, self.fps);
        if due <= self.frames {
            return Ok(());
        }
        self.read_frame(gl)?;
        // A slow source must not silently become a shortened, faster movie.
        // Duplicate the latest frame on the same wall-clock timeline.
        while self.frames < due {
            self.write_frame()?;
        }
        Ok(())
    }
    /// Record exactly one settled frame from a caller-controlled script clock.
    pub fn sample_next(&mut self, gl: &glow::Context) -> Result<(), String> {
        self.read_frame(gl)?;
        self.write_frame()
    }
    fn read_frame(&mut self, gl: &glow::Context) -> Result<(), String> {
        let _span = krkr_protocol::profile::span("capture.video");
        unsafe {
            gl.bind_framebuffer(glow::FRAMEBUFFER, None);
            gl.read_pixels(
                0,
                0,
                960,
                544,
                glow::RGBA,
                glow::UNSIGNED_BYTE,
                glow::PixelPackData::Slice(Some(&mut self.pixels)),
            );
            let error = gl.get_error();
            if error != glow::NO_ERROR {
                return Err(format!("movie capture GLES error {error:#x}"));
            }
        }
        Ok(())
    }
    fn write_frame(&mut self) -> Result<(), String> {
        self.input
            .as_mut()
            .ok_or("movie input closed")?
            .write_all(&self.pixels)
            .map_err(|e| format!("write movie frame: {e}"))?;
        self.frames += 1;
        Ok(())
    }
    pub fn finish(mut self) -> Result<(), String> {
        self.input.take();
        let status = self.process.wait().map_err(|e| e.to_string())?;
        if !status.success() {
            return Err(format!("FFmpeg exited with {status}"));
        }
        Ok(())
    }
}
impl Drop for Export {
    fn drop(&mut self) {
        self.input.take();
        if self.process.try_wait().ok().flatten().is_none() {
            let _ = self.process.kill();
            let _ = self.process.wait();
        }
    }
}
fn frame_count(elapsed: Duration, fps: u32) -> u64 {
    (elapsed.as_nanos() * u128::from(fps) / 1_000_000_000) as u64 + 1
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn export_timeline_keeps_static_frames_and_elapsed_time() {
        assert_eq!(frame_count(Duration::ZERO, 30), 1);
        assert_eq!(frame_count(Duration::from_millis(32), 30), 1);
        assert_eq!(frame_count(Duration::from_millis(34), 30), 2);
        assert_eq!(frame_count(Duration::from_secs(15), 30), 451);
    }
}
