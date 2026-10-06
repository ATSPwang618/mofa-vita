//! Terminal progress is kept on stderr; redirected JSON remains machine-readable.
use indicatif::{MultiProgress, ProgressBar, ProgressDrawTarget, ProgressStyle};
use krkr_assets::xp3::offline;
use std::sync::OnceLock;
use std::time::Duration;

static DISPLAY: OnceLock<MultiProgress> = OnceLock::new();

fn display() -> &'static MultiProgress {
    DISPLAY.get_or_init(MultiProgress::new)
}

pub fn set_enabled(enabled: bool) {
    display().set_draw_target(if enabled {
        ProgressDrawTarget::stderr()
    } else {
        ProgressDrawTarget::hidden()
    });
}

pub fn println(message: &str) {
    display().suspend(|| eprintln!("{message}"));
}

pub(crate) struct Progress(ProgressBar);

impl Progress {
    pub(crate) fn new(label: impl Into<String>, total: Option<usize>) -> Self {
        let bar = display().add(ProgressBar::new_spinner());
        bar.set_prefix(label.into());
        bar.set_style(
            ProgressStyle::with_template("{prefix} {spinner} [{elapsed_precise}] {wide_msg}")
                .expect("valid progress template"),
        );
        if !bar.is_hidden() {
            bar.enable_steady_tick(Duration::from_millis(100));
        }
        let progress = Self(bar);
        if let Some(total) = total {
            progress.total(total);
        }
        progress
    }

    fn total(&self, files: usize) {
        self.0.set_length(files as u64);
        self.0.set_style(ProgressStyle::with_template(
            "{prefix:.24} {spinner} {pos}/{len} [{elapsed_precise}, ETA {eta_precise}]\n[{bar:24.cyan/blue}] {wide_msg}",
        ).expect("valid progress template").progress_chars("=>-"));
    }

    pub(crate) fn done(&self, name: &str) {
        self.0.set_message(name.to_owned());
        self.0.inc(1);
    }

    pub(crate) fn archive(&self, event: offline::Progress<'_>) {
        match event {
            offline::Progress::Started { files } => self.total(files),
            offline::Progress::File { name, .. } => self.done(&String::from_utf16_lossy(name)),
        }
    }
}

impl Drop for Progress {
    fn drop(&mut self) {
        self.0.finish_and_clear();
        display().remove(&self.0);
    }
}
