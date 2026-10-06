//! One lazy, bounded blocking IO worker. Only owned asset data crosses threads;
//! TJS values, native states and the VFS stay on the engine thread.
use crate::operations::OperationId;
use krkr_assets::ReadPlan;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
    mpsc::{self, Receiver, SyncSender},
};
use tjs_core::{NativeError, NativeResult};

pub(crate) struct Read {
    pub plan: ReadPlan,
    pub offset: u64,
    pub encoding: Vec<u16>,
    pub limit: usize,
}
pub(crate) enum Work {
    Extension(Box<dyn crate::extensions::worker::Job>),
    VideoOpen {
        plan: ReadPlan,
        scale: Option<ReadPlan>,
        service: krkr_video::Service,
        silent: bool,
    },
    VideoSeek(krkr_video::Handle, f64),
    VideoAudio(krkr_video::Handle, Option<usize>),
    VideoStream(krkr_video::Handle, usize),
    AudioOpen(
        ReadPlan,
        Option<ReadPlan>,
        krkr_audio::Service,
        Vec<krkr_audio::filter::PhaseVocoder>,
    ),
    AudioSeek(krkr_audio::Handle, u64),
    AudioPlay(krkr_audio::Handle),
    Cursor(ReadPlan, krkr_protocol::budget::Budget),
    Icon(ReadPlan, krkr_protocol::budget::Budget),
    Font(crate::font::tasks::Work),
    Script(Read),
    ImageProbe(krkr_image::Request),
    ImageDecode(krkr_image::Prepared),
    ImageDecodeCompact(krkr_image::Prepared),
    ImageSave(krkr_image::save::Request),
    ImageExport(krkr_image::export::Request),
}
impl From<Read> for Work {
    fn from(read: Read) -> Self {
        Self::Script(read)
    }
}
impl Work {
    fn label(&self) -> &'static str {
        match self {
            Self::Script(_) => "script-read",
            Self::ImageProbe(_) => "image-probe",
            Self::ImageDecode(_) | Self::ImageDecodeCompact(_) => "image-decode",
            Self::ImageSave(_) => "image-save",
            Self::ImageExport(_) => "image-export",
            Self::Font(_) => "font",
            Self::AudioOpen(..) | Self::AudioSeek(..) | Self::AudioPlay(_) => "audio",
            Self::VideoOpen { .. }
            | Self::VideoSeek(..)
            | Self::VideoAudio(..)
            | Self::VideoStream(..) => "video",
            Self::Cursor(..) | Self::Icon(..) => "window-image",
            Self::Extension(_) => "extension",
        }
    }
    fn run(self, cancelled: &AtomicBool) -> NativeResult<Data> {
        match self {
            Self::VideoOpen {
                plan,
                scale,
                service,
                silent,
            } => {
                let metadata = scale
                    .map(|p| krkr_image::scale::Metadata::read(p, cancelled))
                    .transpose()
                    .map_err(|e| NativeError::Detail(e.to_string()))?;
                let mut handle = if silent {
                    service.open_silent(plan, cancelled)
                } else {
                    service.open(plan, cancelled)
                }
                .map_err(NativeError::Detail)?;
                if let Some(metadata) = metadata {
                    metadata
                        .validate(handle.stored_size())
                        .map_err(|e| NativeError::Detail(e.to_string()))?;
                    handle = handle.with_logical_size(metadata.logical);
                }
                Ok(Data::Video(handle))
            }
            Self::Extension(job) => job.run(cancelled).map(Data::Extension),
            Self::ImageExport(request) => request
                .run(cancelled, &Default::default())
                .map(Data::Exported)
                .map_err(|e| NativeError::Detail(e.to_string())),
            Self::VideoSeek(handle, time) => handle
                .seek(time, cancelled)
                .map(|()| Data::Written)
                .map_err(NativeError::Detail),
            Self::VideoAudio(handle, stream) => handle
                .audio_stream(stream, cancelled)
                .map(|()| Data::Written)
                .map_err(NativeError::Detail),
            Self::VideoStream(handle, stream) => handle
                .video_stream(stream, cancelled)
                .map(|()| Data::Written)
                .map_err(NativeError::Detail),
            Self::AudioPlay(handle) => handle
                .play(cancelled)
                .map(|()| Data::Written)
                .map_err(NativeError::Detail),
            Self::AudioOpen(plan, sli, service, filters) => service
                .open_filtered(plan, sli, filters, cancelled)
                .map(Data::Audio)
                .map_err(NativeError::Detail),
            Self::AudioSeek(handle, position) => handle
                .seek(position, cancelled)
                .map(|()| Data::Written)
                .map_err(NativeError::Detail),
            Self::Cursor(plan, budget) => krkr_image::cursor::read(plan, budget, cancelled)
                .map(Data::Cursor)
                .map_err(|e| NativeError::Detail(e.to_string())),
            Self::Icon(plan, budget) => krkr_image::icon::read(plan, budget, cancelled)
                .map(Data::Icon)
                .map_err(|e| NativeError::Detail(e.to_string())),
            Self::Font(work) => work.run(cancelled).map(Data::Font),
            Self::Script(read) => read.run(cancelled),
            Self::ImageProbe(request) => request
                .probe(cancelled)
                .map(|prepared| Data::ImagePrepared(Box::new(prepared)))
                .map_err(|e| NativeError::Detail(e.to_string())),
            Self::ImageDecode(prepared) => prepared
                .decode(cancelled)
                .map(Data::Image)
                .map_err(|e| NativeError::Detail(e.to_string())),
            Self::ImageDecodeCompact(prepared) => prepared
                .decode_compact(cancelled)
                .map(Data::Image)
                .map_err(|e| NativeError::Detail(e.to_string())),
            Self::ImageSave(request) => {
                let path = request.target.path().to_owned();
                request
                    .write(cancelled)
                    .map(|()| Data::FileWritten(path))
                    .map_err(|e| NativeError::Detail(e.to_string()))
            }
        }
    }
}
pub(crate) enum Data {
    Extension(Box<dyn std::any::Any + Send>),
    Exported(Option<krkr_protocol::pixels::Bytes>),
    Video(krkr_video::Handle),
    Audio(krkr_audio::Handle),
    Cursor(krkr_protocol::input_style::CursorImage),
    Icon(krkr_protocol::window::IconImage),
    Font(crate::font::tasks::Data),
    Written,
    FileWritten(std::path::PathBuf),
    ImagePrepared(Box<krkr_image::Prepared>),
    Image(krkr_image::Decoded),
    Text(Vec<u16>),
    Bytecode(Vec<u8>),
}
/// Main-thread mailbox. Move decoded storage data into the continuation without
/// copying an entire script through a temporary GC string/octet.
pub(crate) type Delivery = std::rc::Rc<std::cell::RefCell<Option<Data>>>;
type Completion = (OperationId, NativeResult<Data>);
struct Job {
    id: OperationId,
    cancelled: Arc<AtomicBool>,
    read: Box<Work>,
    queued: krkr_protocol::diagnostics::Timer,
}
pub(crate) struct Worker {
    sender: SyncSender<Job>,
    receiver: Receiver<Completion>,
}
impl Worker {
    pub fn new(capacity: usize, wake: Arc<dyn Fn() + Send + Sync>) -> NativeResult<Self> {
        let (sender, jobs) = mpsc::sync_channel::<Job>(capacity);
        let (completed, receiver) = mpsc::sync_channel(capacity);
        // Dropping the engine disconnects both channels. An in-progress OS read
        // finishes on this thread; it never forces engine teardown to join it.
        std::thread::Builder::new()
            .name("krkr-io".into())
            .spawn(move || {
                while let Ok(job) = jobs.recv() {
                    if job.cancelled.load(Ordering::Relaxed) {
                        continue;
                    }
                    let label = job.read.label();
                    job.queued.report(|| format!("stage=io-queue kind={label}"));
                    let timer = krkr_protocol::diagnostics::Timer::start();
                    let result = job.read.run(&job.cancelled);
                    timer.report(|| format!("stage=io-work kind={label}"));

                    if !job.cancelled.load(Ordering::Relaxed) {
                        if completed.send((job.id, result)).is_err() {
                            break;
                        }
                        wake();
                    }
                }
            })
            .map_err(|error| NativeError::Detail(error.to_string()))?;
        Ok(Self { sender, receiver })
    }
    pub fn submit(
        &self,
        id: OperationId,
        cancelled: Arc<AtomicBool>,
        read: Box<Work>,
    ) -> NativeResult<()> {
        self.sender
            .try_send(Job {
                id,
                cancelled,
                read,
                queued: krkr_protocol::diagnostics::Timer::start(),
            })
            .map_err(|error| match error {
                mpsc::TrySendError::Full(_) => NativeError::Message("engine IO queue is full"),
                mpsc::TrySendError::Disconnected(_) => {
                    NativeError::Message("engine IO worker stopped")
                }
            })
    }
    pub fn completion(&self) -> NativeResult<Option<Completion>> {
        match self.receiver.try_recv() {
            Ok(completion) => Ok(Some(completion)),
            Err(mpsc::TryRecvError::Empty) => Ok(None),
            Err(mpsc::TryRecvError::Disconnected) => {
                Err(NativeError::Message("engine IO worker stopped"))
            }
        }
    }
}
impl Read {
    fn run(self, cancelled: &AtomicBool) -> NativeResult<Data> {
        let timer = krkr_protocol::diagnostics::Timer::start();
        let bytes = self
            .plan
            .read_interruptible(self.offset, || cancelled.load(Ordering::Relaxed))
            .map_err(|error| NativeError::Detail(error.to_string()))?;
        timer.report(|| {
            format!(
                "stage=script-bytes name={} bytes={}",
                String::from_utf16_lossy(&self.plan.name),
                bytes.len()
            )
        });
        if cancelled.load(Ordering::Relaxed) {
            return Err(NativeError::Message("IO cancelled"));
        }
        if tjs_runtime::bytecode::is_bytecode(&bytes) {
            return Ok(Data::Bytecode(bytes));
        }
        krkr_assets::text::decode(&bytes, &self.encoding, self.limit)
            .map(Data::Text)
            .map_err(|error| NativeError::Detail(error.to_string()))
    }
}
