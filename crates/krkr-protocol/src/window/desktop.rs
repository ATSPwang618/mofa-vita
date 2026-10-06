//! Owned desktop requests. No native handles cross the host boundary.
use super::{Rectangle, WindowId};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Monitor {
    pub name: String,
    pub primary: bool,
    pub monitor: Rectangle,
    /// None means the backend cannot query usable desktop bounds.
    pub work: Option<Rectangle>,
}

#[derive(Clone, Copy, Debug)]
pub enum MonitorTarget {
    Primary,
    Window(WindowId),
    Point(i32, i32),
    Rect(i32, i32, i32, i32),
}
#[derive(Clone, Copy, Debug)]
pub enum Clip {
    Release,
    Window(WindowId),
    Rect(i32, i32, i32, i32),
}
#[derive(Debug)]
pub enum Command {
    Monitors(Option<(i32, i32, i32, i32)>),
    Monitor {
        nearest: bool,
        target: MonitorTarget,
    },
    Cursor,
    SetCursor(i32, i32),
    Clip(Clip),
    Metric(u32),
    DoubleClickTime,
    MapKey {
        code: u32,
        mapping: u32,
    },
    IconicPreview(bool),
    Corner {
        window: WindowId,
        preference: u32,
    },
    Ime {
        window: WindowId,
        enabled: bool,
    },
}
#[derive(Debug)]
pub enum Response {
    Void,
    Bool(bool),
    Integer(i64),
    Point(Option<(i32, i32)>),
    Monitors(Option<Vec<(Monitor, Rectangle)>>),
    Monitor(Option<Monitor>),
}
