//! The native dialog future is started/polled from the OS event thread. Its
//! waker shares the bounded event-loop notification; the VM waits independently.
use super::{App, Request, Wake};
use krkr_engine::assets::local;
use krkr_protocol::window::{Command, DirectoryDialog, Response, WindowId};
use std::{
    collections::VecDeque,
    future::Future,
    path::PathBuf,
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    task::{Context, Poll, Waker},
};
use winit::{event_loop::EventLoopProxy, window::Window};

type Selection = Result<Option<Vec<u16>>, String>;
struct Pending {
    request: Request,
    // Keep the real owner alive until the platform has finished with its handle.
    _owner: Option<Arc<Window>>,
    future: Pin<Box<dyn Future<Output = Result<Response, String>>>>,
}
#[derive(Default)]
pub(super) struct Dialogs {
    queue: VecDeque<Request>,
    active: Option<Pending>,
}
struct Notify {
    proxy: EventLoopProxy<Wake>,
    notified: Arc<AtomicBool>,
}
impl std::task::Wake for Notify {
    fn wake(self: Arc<Self>) {
        self.wake_by_ref();
    }
    fn wake_by_ref(self: &Arc<Self>) {
        if !self.notified.swap(true, Ordering::AcqRel) {
            let _ = self.proxy.send_event(Wake::Commands);
        }
    }
}
impl<F, T> App<F, T> {
    pub(super) fn queue_directory_dialog(&mut self, request: Request) {
        self.directory_dialogs.queue.push_back(request);
        self.poll_directory_dialogs();
    }
    pub(super) fn poll_directory_dialogs(&mut self) {
        let waker = Waker::from(Arc::new(Notify {
            proxy: self.proxy.clone(),
            notified: self.notified.clone(),
        }));
        let mut context = Context::from_waker(&waker);
        loop {
            if let Some(pending) = self.directory_dialogs.active.as_mut() {
                // rfd has no portable force-close API. Retain its future and
                // owner after VM cancellation, discard its eventual result, and
                // avoid opening a second modal dialog until the user closes it.
                if let Poll::Ready(result) = pending.future.as_mut().poll(&mut context) {
                    let pending = self.directory_dialogs.active.take().expect("active dialog");
                    if !pending.request.cancelled() {
                        pending.request.respond(result);
                    }
                } else {
                    return;
                }
            }
            let Some(request) = self.directory_dialogs.queue.pop_front() else {
                return;
            };
            if request.cancelled() {
                continue;
            }
            let application_owner = match &request.command {
                Command::SelectDirectory(options) => options.application_owner,
                Command::Inform { .. } => true,
                _ => unreachable!(),
            };
            let owner = if request.window == WindowId::default() {
                application_owner
                    .then(|| {
                        self.main_window
                            .and_then(|id| self.windows.get(&id))
                            .filter(|native| native.alive())
                            .map(|native| native.window.clone())
                    })
                    .flatten()
            } else {
                let Some(window) = self
                    .ids
                    .get(&request.window)
                    .and_then(|id| self.windows.get(id))
                    .filter(|native| native.alive())
                else {
                    request.respond(Err("directory dialog owner is closed".into()));
                    continue;
                };
                Some(window.window.clone())
            };
            let future: Pin<Box<dyn Future<Output = Result<Response, String>>>> =
                match &request.command {
                    Command::SelectDirectory(options) => {
                        let options = options.clone();
                        let owner = owner.clone();
                        Box::pin(async move {
                            select(options, owner)
                                .await
                                .map(Response::DirectorySelected)
                        })
                    }
                    Command::Inform {
                        text,
                        caption,
                        buttons,
                    } => Box::pin(inform(
                        text.clone(),
                        caption.clone(),
                        buttons.clone(),
                        owner.clone(),
                    )),
                    _ => unreachable!(),
                };
            self.directory_dialogs.active = Some(Pending {
                request,
                _owner: owner,
                future,
            });
        }
    }
}
async fn inform(
    text: String,
    caption: String,
    labels: Vec<String>,
    owner: Option<Arc<Window>>,
) -> Result<Response, String> {
    use rfd::{MessageButtons as B, MessageDialogResult as R};
    let buttons = match labels.as_slice() {
        [] => B::Ok,
        [ok] if ok == "OK" => B::Ok,
        [ok, cancel] if ok == "OK" && cancel == "Cancel" => B::OkCancel,
        [a] => B::OkCustom(a.clone()),
        [a, b] => B::OkCancelCustom(a.clone(), b.clone()),
        [a, b, c] => B::YesNoCancelCustom(a.clone(), b.clone(), c.clone()),
        _ => return Err("System.inform supports at most three custom buttons on this host".into()),
    };
    let mut dialog = rfd::AsyncMessageDialog::new()
        .set_title(caption)
        .set_description(text)
        .set_buttons(buttons);
    if let Some(owner) = &owner {
        dialog = dialog.set_parent(owner.as_ref());
    }
    let index = match dialog.show().await {
        R::Ok | R::Yes => 0,
        R::No => 1,
        R::Cancel => {
            if labels.len() >= 2 {
                (labels.len() - 1) as i32
            } else {
                -1
            }
        }
        R::Custom(label) => labels
            .iter()
            .position(|v| v == &label)
            .map_or(-1, |v| v as i32),
    };
    Ok(Response::Informed(index))
}
async fn select(options: DirectoryDialog, owner: Option<Arc<Window>>) -> Selection {
    let mut dialog = rfd::AsyncFileDialog::new().set_can_create_directories(true);
    if let Some(owner) = &owner {
        dialog = dialog.set_parent(owner.as_ref());
    }
    if !options.title.is_empty() {
        dialog = dialog.set_title(String::from_utf16(&options.title).map_err(|e| e.to_string())?);
    }
    let root = if options.root.is_empty() {
        None
    } else {
        let root = local::path(&options.root).map_err(|e| e.to_string())?;
        // The original ignores a root which cannot be resolved to a folder.
        std::fs::canonicalize(root)
            .ok()
            .filter(|path| path.is_dir())
    };
    let mut initial = if options.initial.is_empty() {
        None
    } else {
        Some(local::path(&options.initial).map_err(|e| e.to_string())?)
    };
    if let Some(root) = &root {
        let inside = initial
            .as_ref()
            .and_then(|path| std::fs::canonicalize(path).ok())
            .is_some_and(|path| path.starts_with(root));
        if !inside {
            initial = Some(root.clone());
        }
    }
    if let Some(initial) = initial {
        dialog = dialog.set_directory(initial);
    }
    loop {
        let Some(selected) = dialog.clone().pick_folder().await else {
            return Ok(None);
        };
        let selected = selected.path();
        if !selected.is_dir() {
            return Err("the selected directory is no longer available".into());
        }
        if let Some(root) = &root {
            let actual = std::fs::canonicalize(selected).map_err(|e| e.to_string())?;
            if !actual.starts_with(root) {
                // Modern pickers do not expose a common navigation-root API.
                // Enforce the allowed selection on every platform, including
                // symlink escapes, and let the user choose again or cancel.
                let mut message = rfd::AsyncMessageDialog::new()
                    .set_title("Select a directory")
                    .set_description("Choose a directory inside the permitted root folder.")
                    .set_level(rfd::MessageLevel::Warning);
                if let Some(owner) = &owner {
                    message = message.set_parent(owner.as_ref());
                }
                message.show().await;
                dialog = dialog.set_directory(root);
                continue;
            }
        }
        // No extra trailing slash: the original normalizes SHGetPathFromIDList.
        return local::units(&PathBuf::from(selected))
            .map(Some)
            .map_err(|e| e.to_string());
    }
}
