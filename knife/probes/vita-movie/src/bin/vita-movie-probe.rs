#[cfg(target_os = "vita")]
mod native_probe {
    use std::{fs::{File, OpenOptions}, io::{self, Write}, path::PathBuf, time::{Duration, Instant}};
    use knife_platform_vita::{gxm::{Gxm, TextureId}, movie::{MovieError, MoviePlayer, MovieState, VideoFrame}};
    use knife_platform_vita::{movie_geometry::{self, VideoGeometry}, movie_view::{self, VideoPresentation}};
    use knife_vita_movie_probe::{ASSETS, Asset, Observations};

    // Fixed DEV path. Closing before reopening publishes guest FILE writes to
    // the emulator host as well as keeping each stage independently readable.
    struct PublishedLog { path: PathBuf, file: Option<File> }
    impl PublishedLog {
        fn create(path: String) -> io::Result<Self> {
            let path = PathBuf::from(path);
            Ok(Self { file: Some(File::create(&path)?), path })
        }
    }
    impl Write for PublishedLog {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.file.as_mut().ok_or_else(|| io::Error::new(io::ErrorKind::BrokenPipe,"DEV log closed"))?.write(bytes)
        }
        fn flush(&mut self) -> io::Result<()> {
            self.file.as_mut().ok_or_else(|| io::Error::new(io::ErrorKind::BrokenPipe,"DEV log closed"))?.flush()?;
            drop(self.file.take());
            self.file = Some(OpenOptions::new().append(true).open(&self.path)?);
            Ok(())
        }
    }
    fn record(log: &mut PublishedLog, line: impl std::fmt::Display) -> Result<(), String> {
        writeln!(log,"{line}").map_err(|e|e.to_string())?;
        log.flush().map_err(|e|e.to_string())
    }
    fn movie<T>(result: Result<T,MovieError>) -> Result<T,String> { result.map_err(|e|e.to_string()) }
    fn elapsed(start: Instant) -> u64 { start.elapsed().as_millis().min(u64::MAX as u128) as u64 }

    fn draw(gxm: &mut Gxm, texture: &mut Option<TextureId>, frame: &VideoFrame<'_>, view: VideoPresentation) -> Result<(),String> {
        if let Some(texture) = texture.as_ref() {
            // SAFETY: a live MoviePlayer frame borrow spans present + GPU fence.
            // Every prior submission completed before the next native pull.
            unsafe { gxm.rebind_movie_texture(texture,frame.native_data(),frame.width,frame.height)?; }
        } else {
            // SAFETY: SDK-owned GPU-mapped YVU420P2 surface, never a guessed CPU
            // plane/stride. The frame owner survives the complete GPU fence.
            *texture = Some(unsafe { gxm.create_movie_texture(frame.native_data(),frame.width,frame.height)? });
        }
        gxm.begin([0.0,0.0,0.0,1.0])?;
        gxm.draw(texture.as_ref(),[0.0,0.0,960.0,540.0],view.uv,[1.0;4],false)?;
        gxm.present()?;
        gxm.finish_movie_sampling()
    }

    fn check_seek(player: &mut MoviePlayer, gxm: &mut Gxm, texture: &mut Option<TextureId>,
        asset: Asset, geometry: VideoGeometry, log: &mut PublishedLog) -> Result<(),String> {
        // This call is made only after a completed GPU fence. Rejected native
        // seeks are reported as rejected, never counted as a successful seek.
        match player.seek_ms(1000) {
            Err(error @ (MovieError::Unsupported | MovieError::Native(_))) => {
                record(log,format!("ASSET {} SEEK REJECTED error={error:?} supported_seek=UNVERIFIED",asset.name))
            }
            Err(error) => Err(format!("seek boundary failed: {error}")),
            Ok(()) => {
                let start = Instant::now();
                while elapsed(start) < 3000 {
                    let time = movie(player.time_ms())?;
                    if let Some(frame) = movie(player.next_video())? {
                        let view = movie_view::resolve(geometry, [frame.width, frame.height]).map_err(|e|e.to_string())?;
                        if view.visible_width != 960 || view.visible_height != 540 { return Err("seek returned wrong visible video dimensions".into()); }
                        let timestamp = frame.timestamp_ms;
                        draw(gxm,texture,&frame,view)?;
                        if (750..=1500).contains(&timestamp) && (750..=2000).contains(&time) {
                            return record(log,format!("ASSET {} SEEK OBSERVED target_ms=1000 frame_ms={timestamp} clock_ms={time}",asset.name));
                        }
                    }
                    std::thread::sleep(Duration::from_millis(33));
                }
                Err("native seek accepted but target time/frame was not observed".into())
            }
        }
    }

    fn playback(player: &mut MoviePlayer, gxm: &mut Gxm, texture: &mut Option<TextureId>,
        observed: &mut Observations, geometry: VideoGeometry, log: &mut PublishedLog) -> Result<(),String> {
        let asset = observed.asset;
        let start = Instant::now();
        let mut last_video_call = None;
        let mut last_progress_second = u64::MAX;
        let mut paused_and_resumed = false;
        let mut frame_ready = false;
        let mut pcm = [[0.0f32;2];960];
        loop {
            let wall_ms = elapsed(start);
            if wall_ms > if asset.require_natural_eof { 30000 } else { 10000 } {
                return Err("bounded native playback deadline exceeded".into());
            }
            // Do not poll video faster than the source's 30 fps cadence. Some
            // emulators return the same descriptor between decoded frames.
            if last_video_call.is_none_or(|last|wall_ms.saturating_sub(last)>=33) {
                last_video_call = Some(wall_ms);
                if let Some(frame) = movie(player.next_video())? {
                    let timestamp = frame.timestamp_ms;
                    let first = observed.video_frames == 0;
                    if first {
                        record(log,format!("ASSET {} NATIVE_VIDEO storage_width={} storage_height={} raw_aspect={} crop_left={} crop_right={} crop_top={} crop_bottom={} crop_units=UNVERIFIED timestamp_ms={timestamp}",
                            asset.name,frame.width,frame.height,frame.aspect_ratio,frame.crop.left,frame.crop.right,frame.crop.top,frame.crop.bottom))?;
                    }
                    let view = movie_view::resolve(geometry, [frame.width, frame.height]).map_err(|e|e.to_string())?;
                    observed.video(view.visible_width,view.visible_height,view.display_aspect,timestamp)?;
                    draw(gxm,texture,&frame,view)?;
                    observed.presented()?;
                    if first {
                        record(log,format!("ASSET {} FIRST_VIDEO width={} height={} timestamp_ms={timestamp} native_format={:?} gpu_fence=COMPLETE texture_width={} texture_height={} uv_left={} uv_top={} uv_right={} uv_bottom={} display_aspect={} native_layout={:?} sampling_authority=MP4_AVCC_SPS",
                            asset.name,view.visible_width,view.visible_height,frame.format,frame.width,frame.height,
                            view.uv[0],view.uv[1],view.uv[2],view.uv[3],view.display_aspect,view.native_layout))?;
                    }
                    if !frame_ready && observed.video_timestamp_advances > 0 {
                        frame_ready = true;
                        record(log,format!("ASSET {} FRAME READY timestamp_ms={timestamp} gpu_fence=COMPLETE",asset.name))?;
                    }
                }
            }
            // At most four bounded pulls per iteration. The aggregate pull
            // cadence follows 48k frames/s wall time; a fast host decoder must
            // not consume an entire audio track ahead of the video clock.
            for _ in 0..4 {
                if asset.has_audio && observed.audio_frames >= wall_ms * 48 + 960 { break; }
                let read = movie(player.read_audio(&mut pcm))?;
                if read.frames == 0 { break; }
                let first = observed.audio_frames == 0;
                observed.audio(&pcm[..read.frames],read.timestamp_ms)?;
                if first {
                    record(log,format!("ASSET {} FIRST_AUDIO frames={} timestamp_ms={} output_rate=48000 output_channels=2 samples=FINITE_BOUNDED native_channels=1_or_2_validated audio_output=NOT_EXERCISED",asset.name,read.frames,read.timestamp_ms))?;
                }
            }
            let state = movie(player.poll_state())?;
            if state == MovieState::Eof {
                observed.conclude(true,elapsed(start));
                record(log,format!("ASSET {} NATIVE EOF wall_ms={} video_frames={} video_last_ms={:?} audio_frames={} audio_last_ms={:?}",asset.name,elapsed(start),observed.video_frames,observed.last_video_ms,observed.audio_frames,observed.last_audio_ms))?;
                return observed.complete();
            }
            if state != MovieState::Playing { return Err(format!("unexpected playback state {state:?}")); }
            if !asset.require_natural_eof && !paused_and_resumed && wall_ms >= 1000 && observed.video_timestamp_advances > 0 {
                let before = movie(player.time_ms())?;
                movie(player.pause())?;
                let paused_state = movie(player.poll_state())?;
                let audio = movie(player.read_audio(&mut pcm))?;
                let video_available = movie(player.next_video())?.is_some();
                record(log,format!("ASSET {} PAUSED clock_ms={before} fresh_audio_frames={} fresh_video={video_available}",asset.name,audio.frames))?;
                std::thread::sleep(Duration::from_millis(150));
                let after = movie(player.time_ms())?;
                observed.pause_hold(before,after,paused_state==MovieState::Paused,video_available,audio.frames)?;
                movie(player.resume())?;
                observed.resumed(movie(player.poll_state())?==MovieState::Playing)?;
                paused_and_resumed = true;
                record(log,format!("ASSET {} RESUMED paused_clock_ms={before}..{after}",asset.name))?;
            }
            if !asset.require_natural_eof && paused_and_resumed && wall_ms >= 3000 {
                check_seek(player,gxm,texture,asset,geometry,log)?;
                movie(player.stop())?;
                observed.stopped(movie(player.poll_state())?==MovieState::Stopped)?;
                observed.conclude(false,elapsed(start));
                record(log,format!("ASSET {} STOPPED wall_ms={} video_frames={} video_last_ms={:?} audio_frames={}",asset.name,elapsed(start),observed.video_frames,observed.last_video_ms,observed.audio_frames))?;
                return observed.complete();
            }
            if wall_ms / 1000 != last_progress_second {
                last_progress_second = wall_ms / 1000;
                record(log,format!("ASSET {} PROGRESS wall_ms={wall_ms} native_clock_ms={} video_frames={} video_last_ms={:?} audio_frames={} audio_last_ms={:?}",asset.name,movie(player.time_ms())?,observed.video_frames,observed.last_video_ms,observed.audio_frames,observed.last_audio_ms))?;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    fn asset(gxm: &mut Gxm, asset: Asset, log: &mut PublishedLog) -> Result<(),String> {
        let path = format!("ux0:data/knife-woh/assets/{}",asset.relative_path);
        let length = std::fs::metadata(&path).map_err(|e|format!("input {path}: {e}"))?.len();
        if length != asset.bytes { return Err(format!("input byte length mismatch {length} != {}",asset.bytes)); }
        record(log,format!("ASSET {} BEGIN path={path} runtime_bytes={length} expected_sha256={} sha_authority=HOST_RECEIPT expected_audio={}",asset.name,asset.sha256,asset.has_audio))?;
        let geometry = movie_geometry::read_file(&path).map_err(|e|format!("read source video geometry: {e}"))?;
        let sar = geometry.sar.ok_or("source SPS does not declare sample aspect")?;
        record(log,format!("ASSET {} SOURCE_VIDEO coded_width={} coded_height={} crop_left={} crop_top={} crop_right={} crop_bottom={} visible_width={} visible_height={} sar_num={} sar_den={} authority=MP4_AVCC_SPS",
            asset.name,geometry.coded_width,geometry.coded_height,geometry.crop_left,geometry.crop_top,
            geometry.crop_right,geometry.crop_bottom,geometry.visible_width,geometry.visible_height,sar.numerator,sar.denominator))?;
        if !asset.has_audio { record(log,format!("ASSET {} EXPECTED_AUDIO_ABSENT source_inventory=VIDEO_ONLY",asset.name))?; }
        let mut player = movie(MoviePlayer::open_file(&path))?;
        let mut texture = None;
        let mut observed = Observations::new(asset);
        let outcome = movie(player.start()).and_then(|()|playback(&mut player,gxm,&mut texture,&mut observed,geometry,log));
        // Cleanup is explicit and checked even on a decode/assertion failure.
        // A failed GPU fence cannot authorize freeing an SDK frame allocation.
        if let Err(error) = gxm.finish_movie_sampling() {
            std::mem::forget(player);
            record(log,format!("ASSET {} CLEANUP FAIL gpu_fence={error}; bounded native owner retained to avoid frame UAF",asset.name))?;
            return Err(format!("unsafe to release native movie after GPU fence failure: {error}"));
        }
        if let Some(texture) = texture.take() {
            if let Err(error) = gxm.destroy_texture(&texture) {
                std::mem::forget(player);
                record(log,format!("ASSET {} CLEANUP FAIL texture_destroy={error}; bounded native owner retained",asset.name))?;
                return Err(error);
            }
        }
        record(log,format!("ASSET {} TEXTURE RETIRED gpu_fence=COMPLETE",asset.name))?;
        let close = movie(player.close());
        record(log,format!("ASSET {} CLOSE {}",asset.name,match &close { Ok(())=>"PASS".to_owned(),Err(error)=>format!("FAIL {error}") }))?;
        match (outcome,close) {
            (Ok(()),Ok(())) => record(log,format!("ASSET {} PASS video_frames={} presented_frames={} audio_frames={} nonzero_samples={} audio_output=NOT_EXERCISED supported_seek=SEPARATE_EVIDENCE",asset.name,observed.video_frames,observed.presented_frames,observed.audio_frames,observed.audio_nonzero_samples)),
            (Err(error),Ok(())) | (Ok(()),Err(error)) => Err(error),
            (Err(error),Err(close)) => Err(format!("{error}; close also failed: {close}")),
        }
    }

    fn all(log: &mut PublishedLog) -> Result<(),String> {
        let mut gxm = Gxm::new()?;
        record(log,"GXM READY native_YVU420P2_CSC1 CPU_stride=UNKNOWN_AND_UNUSED")?;
        let mut failures = Vec::new();
        for input in ASSETS {
            if let Err(error) = asset(&mut gxm,input,log) {
                record(log,format!("ASSET {} FAIL {error}",input.name))?;
                failures.push(format!("{}: {error}",input.name));
            }
        }
        let close = gxm.close();
        record(log,format!("GXM CLOSE {}",match &close { Ok(())=>"PASS".to_owned(),Err(error)=>format!("FAIL {error}") }))?;
        if let Err(error) = close { failures.push(error); }
        if failures.is_empty() { Ok(()) } else { Err(failures.join("; ")) }
    }

    pub fn run() -> Result<(),String> {
        unsafe extern "C" { fn kv_init_data_dir() -> i32; }
        // SAFETY: fixed native DEV directory initializer has no pointers.
        let status = unsafe { kv_init_data_dir() };
        if status < 0 { return Err(format!("initialize DEV directory {status:#010x}")); }
        let mut log = PublishedLog::create(knife_platform_vita::data_path("movie-probe.log")?).map_err(|e|e.to_string())?;
        record(&mut log,"Knife Vita DEV actual native AvPlayer probe v1")?;
        record(&mut log,"TITLE_ID KNIF00011")?;
        record(&mut log,"CLAIM finite 3-file native decode/metadata/controls/EOF/GXM submissions; not audible output, visual correctness, full openings, supported seek or hardware equivalence")?;
        let result = all(&mut log);
        if let Err(error) = &result { record(&mut log,format!("ERROR {error}"))?; }
        record(&mut log,format!("RESULT {}",if result.is_ok(){"PASS"}else{"FAIL"}))?;
        result
    }
}

#[cfg(target_os = "vita")]
fn main() { if let Err(error)=native_probe::run() { eprintln!("movie probe failed: {error}");std::process::exit(1); } }
#[cfg(not(target_os = "vita"))]
fn main() { eprintln!("Vita-only native runtime probe; use cargo test for observation rejection rules.");std::process::exit(2); }
