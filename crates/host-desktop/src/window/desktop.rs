//! Cross-platform desktop services. Unsupported queries never invent OS data.
use super::*;
use enigo::Mouse;
use protocol::Rectangle;
use protocol::desktop::{Clip, Command as DesktopCommand, Monitor, MonitorTarget, Response};

fn cursor_backend() -> Result<enigo::Enigo, String> {
    enigo::Enigo::new(&enigo::Settings {
        open_prompt_to_get_permissions: false,
        release_keys_when_dropped: false,
        ..Default::default()
    })
    .map_err(|e| e.to_string())
}

pub(super) fn monitors(event_loop: &ActiveEventLoop) -> Vec<Monitor> {
    let primary = event_loop.primary_monitor();
    event_loop
        .available_monitors()
        .map(|monitor| {
            let position = monitor.position();
            let size = monitor.size();
            Monitor {
                name: monitor.name().unwrap_or_default(),
                primary: primary.as_ref() == Some(&monitor),
                monitor: Rectangle {
                    x: position.x,
                    y: position.y,
                    width: size.width,
                    height: size.height,
                },
                work: None,
            }
        })
        .collect()
}
fn rect(x: i32, y: i32, width: i32, height: i32) -> Option<Rectangle> {
    Some(Rectangle {
        x,
        y,
        width: width.try_into().ok()?,
        height: height.try_into().ok()?,
    })
}
fn intersect(a: Rectangle, b: Rectangle) -> Option<Rectangle> {
    let x = i64::from(a.x).max(b.x.into());
    let y = i64::from(a.y).max(b.y.into());
    let right = (i64::from(a.x) + i64::from(a.width)).min(i64::from(b.x) + i64::from(b.width));
    let bottom = (i64::from(a.y) + i64::from(a.height)).min(i64::from(b.y) + i64::from(b.height));
    (right > x && bottom > y).then_some(Rectangle {
        x: x as i32,
        y: y as i32,
        width: (right - x) as u32,
        height: (bottom - y) as u32,
    })
}
fn distance(a: Rectangle, b: Rectangle) -> i128 {
    let dx = (i64::from(a.x) - i64::from(b.x) - i64::from(b.width))
        .max(i64::from(b.x) - i64::from(a.x) - i64::from(a.width))
        .max(0);
    let dy = (i64::from(a.y) - i64::from(b.y) - i64::from(b.height))
        .max(i64::from(b.y) - i64::from(a.y) - i64::from(a.height))
        .max(0);
    i128::from(dx) * i128::from(dx) + i128::from(dy) * i128::from(dy)
}
impl<F, T> App<F, T> {
    pub(super) fn desktop(
        &mut self,
        event_loop: &ActiveEventLoop,
        command: &DesktopCommand,
    ) -> Result<Response, String> {
        let native = |id: &WindowId| self.ids.get(id).and_then(|id| self.windows.get(id));
        Ok(match *command {
            DesktopCommand::Monitors(filter) => {
                let filter = match filter {
                    Some((x, y, w, h)) => match rect(x, y, w, h) {
                        Some(r) => Some(r),
                        None => return Ok(Response::Monitors(Some(vec![]))),
                    },
                    None => None,
                };
                let monitors = monitors(event_loop)
                    .into_iter()
                    .filter_map(|m| {
                        let intersection = match filter {
                            Some(r) => intersect(m.monitor, r)?,
                            None => m.monitor,
                        };
                        Some((m, intersection))
                    })
                    .collect();
                Response::Monitors(Some(monitors))
            }
            DesktopCommand::Monitor { nearest, target } => {
                let monitors = monitors(event_loop);
                let target = match target {
                    MonitorTarget::Primary => {
                        return Ok(Response::Monitor(monitors.into_iter().find(|m| m.primary)));
                    }
                    MonitorTarget::Point(x, y) => Some(Rectangle {
                        x,
                        y,
                        width: 1,
                        height: 1,
                    }),
                    MonitorTarget::Rect(x, y, w, h) => rect(x, y, w, h),
                    MonitorTarget::Window(id) => native(&id).and_then(|n| {
                        let position = n.window.outer_position().ok()?;
                        let size = n.window.outer_size();
                        Some(Rectangle {
                            x: position.x,
                            y: position.y,
                            width: size.width,
                            height: size.height,
                        })
                    }),
                };
                let Some(target) = target else {
                    return Ok(Response::Monitor(None));
                };
                let best = monitors
                    .iter()
                    .enumerate()
                    .filter_map(|(i, m)| {
                        intersect(m.monitor, target)
                            .map(|r| (i, u64::from(r.width) * u64::from(r.height)))
                    })
                    .max_by_key(|&(_, area)| area)
                    .map(|(i, _)| i)
                    .or_else(|| {
                        nearest
                            .then(|| {
                                monitors
                                    .iter()
                                    .enumerate()
                                    .min_by_key(|(_, m)| distance(m.monitor, target))
                                    .map(|(i, _)| i)
                            })
                            .flatten()
                    });
                Response::Monitor(best.map(|i| monitors[i].clone()))
            }
            DesktopCommand::Cursor => Response::Point(
                cursor_backend()
                    .ok()
                    .and_then(|backend| backend.location().ok()),
            ),
            DesktopCommand::SetCursor(x, y) => {
                Response::Bool(cursor_backend().is_ok_and(|mut backend| {
                    backend.move_mouse(x, y, enigo::Coordinate::Abs).is_ok()
                }))
            }
            DesktopCommand::Clip(Clip::Release) => {
                for window in self.windows.values() {
                    window
                        .window
                        .set_cursor_grab(winit::window::CursorGrabMode::None)
                        .map_err(|e| e.to_string())?;
                }
                Response::Void
            }
            DesktopCommand::Clip(Clip::Window(id)) => {
                native(&id)
                    .ok_or("window is closed")?
                    .window
                    .set_cursor_grab(winit::window::CursorGrabMode::Confined)
                    .map_err(|e| e.to_string())?;
                Response::Void
            }
            DesktopCommand::Clip(Clip::Rect(x, y, w, h)) => {
                let Some(target) = rect(x, y, w, h) else {
                    return Err("invalid cursor clip rectangle".into());
                };
                let window = self
                    .windows
                    .values()
                    .find(|n| {
                        n.window
                            .inner_position()
                            .is_ok_and(|p| p.x == target.x && p.y == target.y)
                            && n.window.inner_size()
                                == PhysicalSize::new(target.width, target.height)
                    })
                    .ok_or(
                        "this backend supports cursor clipping only to a window client rectangle",
                    )?;
                window
                    .window
                    .set_cursor_grab(winit::window::CursorGrabMode::Confined)
                    .map_err(|e| e.to_string())?;
                Response::Void
            }
            DesktopCommand::DoubleClickTime => {
                Response::Integer(super::input::DOUBLE_CLICK_MILLIS as i64)
            }
            DesktopCommand::Metric(index) => {
                let displays = monitors(event_loop);
                let primary = displays.iter().find(|m| m.primary);
                let value = match index {
                    0 => primary.map(|m| i64::from(m.monitor.width)),
                    1 => primary.map(|m| i64::from(m.monitor.height)),
                    76 => displays.iter().map(|m| i64::from(m.monitor.x)).min(),
                    77 => displays.iter().map(|m| i64::from(m.monitor.y)).min(),
                    78 => displays
                        .iter()
                        .map(|m| i64::from(m.monitor.x) + i64::from(m.monitor.width))
                        .max()
                        .zip(displays.iter().map(|m| i64::from(m.monitor.x)).min())
                        .map(|(max, min)| max - min),
                    79 => displays
                        .iter()
                        .map(|m| i64::from(m.monitor.y) + i64::from(m.monitor.height))
                        .max()
                        .zip(displays.iter().map(|m| i64::from(m.monitor.y)).min())
                        .map(|(max, min)| max - min),
                    80 => Some(displays.len() as i64),
                    36 | 37 => Some(8), // The engine's actual double-click proximity policy.
                    _ => None,
                };
                Response::Integer(
                    value.ok_or("this system metric is unavailable on the cross-platform host")?,
                )
            }
            DesktopCommand::MapKey { .. } => {
                return Err(
                    "native keyboard scan-code mapping is unavailable on this backend".into(),
                );
            }
            DesktopCommand::IconicPreview(_) | DesktopCommand::Corner { .. } => {
                Response::Bool(false)
            }
            DesktopCommand::Ime { window, enabled } => {
                native(&window)
                    .ok_or("window is closed")?
                    .window
                    .set_ime_allowed(enabled);
                Response::Bool(true)
            }
        })
    }
}
