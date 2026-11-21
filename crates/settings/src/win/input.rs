use super::*;

impl Window {
    fn point(&self, lp: LPARAM) -> (f32, f32) {
        (
            (lp.0 as i16) as f32 / self.scale,
            ((lp.0 >> 16) as i16) as f32 / self.scale,
        )
    }

    fn hit(&self, x: f32, y: f32) -> Option<Hit> {
        self.hits.iter().rev().copied().find(|h| {
            h.contains(x, y) && (!h.scrolls || (y >= view::TOP && y < self.height() - 38.0))
        })
    }

    fn drag(&mut self, x: f32, hit: Hit) {
        let Action::Opacity(part) = hit.action else {
            return;
        };
        let value = ((x - hit.rect.left) * 100.0 / (hit.rect.right - hit.rect.left))
            .round()
            .clamp(0.0, 100.0) as u8;
        self.opacity(part, value);
    }

    fn activate(&mut self, action: Action) -> Option<(&'static str, PathBuf)> {
        self.model.focused = Some(action);
        match action {
            Action::Page(page) => {
                self.model.page = page;
                self.model.scroll = 0.0;
                self.model.dropdown = None;
            }
            Action::Choose(part) => {
                self.model.dropdown = if self.model.dropdown == Some(part) {
                    None
                } else {
                    Some(part)
                };
                if self.model.dropdown.is_some() {
                    self.model.focused =
                        Some(Action::Material(part.map_or(Material::Windows, |p| {
                            self.model.doc.get(p).material
                        })));
                }
            }
            Action::Material(material) => {
                if let Some(part) = self.model.dropdown.take() {
                    self.model.focused = Some(Action::Choose(part));
                    self.commit(|doc| {
                        for p in Part::ALL.into_iter().filter(|&p| {
                            view::available(p) && part.is_none_or(|single| p == single)
                        }) {
                            let old = doc.get(p);
                            doc.set(p, Style { material, ..old });
                        }
                    });
                    return None;
                }
            }
            Action::Reset => {
                self.model.dropdown = None;
                self.commit(|doc| {
                    for p in Part::ALL {
                        doc.reset(p);
                    }
                });
                return None;
            }
            Action::Fallback | Action::Uninstall => {
                return Some((
                    if action == Action::Fallback {
                        "--fallback"
                    } else {
                        "--uninstall"
                    },
                    self.exe.with_file_name("explorust-guard.exe"),
                ));
            }
            Action::Opacity(_) => {}
        }
        self.redraw();
        None
    }

    fn scroll(&mut self, delta: f32) {
        if self.model.page != Page::Appearance {
            return;
        }
        self.model.dropdown = None;
        self.model.scroll = (self.model.scroll + delta).clamp(0.0, view::max_scroll(self.height()));
        self.redraw();
    }

    fn tab(&mut self, back: bool) {
        let popup = self.model.dropdown.is_some();
        let controls: Vec<Hit> = self
            .hits
            .iter()
            .copied()
            .filter(|h| popup == matches!(h.action, Action::Material(_)))
            .collect();
        if controls.is_empty() {
            return;
        }
        let last = controls.len() - 1;
        let pos = controls
            .iter()
            .position(|b| Some(b.action) == self.model.focused);
        let next = match (pos, back) {
            (None, true) | (Some(0), true) => last,
            (None, false) => 0,
            (Some(i), true) => i - 1,
            (Some(i), false) => (i + 1) % controls.len(),
        };
        let target = controls[next];
        self.model.focused = Some(target.action);
        if target.scrolls {
            let delta = if target.rect.top < view::TOP {
                target.rect.top - view::TOP
            } else {
                (target.rect.bottom - (self.height() - 38.0)).max(0.0)
            };
            if delta != 0.0 {
                self.scroll(delta);
                return;
            }
        }
        self.redraw();
    }

    fn key(&mut self, key: VIRTUAL_KEY) -> Option<(&'static str, PathBuf)> {
        if key == VK_ESCAPE {
            if let Some(part) = self.model.dropdown.take() {
                self.model.focused = Some(Action::Choose(part));
            }
            self.redraw();
        } else if key == VK_TAB || (self.model.dropdown.is_some() && matches!(key, VK_DOWN | VK_UP))
        {
            let back =
                key == VK_UP || (key == VK_TAB && unsafe { GetKeyState(VK_SHIFT.0 as i32) } < 0);
            self.tab(back);
        } else if matches!(key, VK_RETURN | VK_SPACE) {
            return self.model.focused.and_then(|a| self.activate(a));
        } else if matches!(key, VK_LEFT | VK_RIGHT) {
            if let Some(Action::Opacity(part)) = self.model.focused {
                let value = i16::from(self.model.doc.get(part).opacity)
                    + if key == VK_LEFT { -5 } else { 5 };
                self.opacity(part, value.clamp(0, 100) as u8);
            }
        } else if matches!(key, VK_PRIOR | VK_NEXT | VK_DOWN | VK_UP) {
            let delta = if matches!(key, VK_PRIOR | VK_NEXT) {
                self.height() - view::TOP - 64.0
            } else {
                55.0
            };
            self.scroll(if matches!(key, VK_PRIOR | VK_UP) {
                -delta
            } else {
                delta
            });
        }
        None
    }
}

fn launch(action: Option<(&'static str, PathBuf)>) {
    if let Some((arg, path)) = action {
        if let Err(e) = Command::new(path).arg(arg).spawn() {
            report_error(&e.to_string());
        }
    }
}

pub(super) unsafe extern "system" fn window_proc(
    hwnd: HWND,
    msg: u32,
    wp: WPARAM,
    lp: LPARAM,
) -> LRESULT {
    unsafe {
        match msg {
            WM_PAINT => {
                let mut paint = PAINTSTRUCT::default();
                BeginPaint(hwnd, &mut paint);
                with(Window::redraw);
                let _ = EndPaint(hwnd, &paint);
            }
            WM_GETMINMAXINFO => {
                let k = GetDpiForWindow(hwnd).max(96) as f32 / 96.0;
                let mm = &mut *(lp.0 as *mut MINMAXINFO);
                mm.ptMinTrackSize = POINT {
                    x: (MIN_WIDTH as f32 * k) as i32,
                    y: (MIN_HEIGHT as f32 * k) as i32,
                };
            }
            WM_DPICHANGED => {
                let r = &*(lp.0 as *const RECT);
                let _ = SetWindowPos(
                    hwnd,
                    None,
                    r.left,
                    r.top,
                    r.right - r.left,
                    r.bottom - r.top,
                    SWP_NOACTIVATE | SWP_NOZORDER,
                );
                with(Window::resized);
            }
            WM_SIZE => {
                with(Window::resized);
            }
            WM_MOUSEMOVE => {
                with(|ui| {
                    if !ui.tracking {
                        let mut t = TRACKMOUSEEVENT {
                            cbSize: size_of::<TRACKMOUSEEVENT>() as u32,
                            dwFlags: TME_LEAVE,
                            hwndTrack: hwnd,
                            dwHoverTime: 0,
                        };
                        ui.tracking = TrackMouseEvent(&mut t).is_ok();
                    }
                    let (x, y) = ui.point(lp);
                    if let Some(hit) = ui.dragging {
                        ui.drag(x, hit);
                        return;
                    }
                    let action = ui.hit(x, y).map(|hit| hit.action);
                    if ui.model.hovered != action {
                        ui.model.hovered = action;
                        ui.redraw();
                    }
                });
            }
            WM_MOUSELEAVE => {
                with(|ui| {
                    ui.tracking = false;
                    ui.model.hovered = None;
                    ui.redraw();
                });
            }
            WM_MOUSEWHEEL => {
                let delta = ((wp.0 >> 16) as i16) as f32;
                with(|ui| ui.scroll(-delta * (55.0 / 120.0)));
            }
            WM_LBUTTONDOWN => {
                let capture = with(|ui| {
                    let (x, y) = ui.point(lp);
                    let hit = ui.hit(x, y);
                    if ui.model.dropdown.is_some()
                        && !hit.is_some_and(|h| {
                            matches!(h.action, Action::Material(_) | Action::Choose(_))
                        })
                    {
                        ui.model.dropdown = None;
                        ui.redraw();
                        return false;
                    }
                    let Some(hit) = hit else { return false };
                    ui.pressed = Some(hit.action);
                    ui.model.focused = Some(hit.action);
                    if matches!(hit.action, Action::Opacity(_)) {
                        ui.dragging = Some(hit);
                        ui.drag(x, hit);
                    }
                    ui.redraw();
                    true
                })
                .unwrap_or(false);
                if capture {
                    SetCapture(hwnd);
                }
            }
            WM_LBUTTONUP => {
                let action = with(|ui| {
                    let pressed = ui.pressed.take();
                    if ui.dragging.take().is_some() {
                        ui.flush_opacity();
                        return None;
                    }
                    let (x, y) = ui.point(lp);
                    let hit = ui.hit(x, y).map(|h| h.action);
                    if pressed == hit {
                        hit.and_then(|a| ui.activate(a))
                    } else {
                        None
                    }
                })
                .flatten();
                if GetCapture() == hwnd {
                    let _ = ReleaseCapture();
                }
                launch(action);
            }
            WM_CAPTURECHANGED => {
                with(|ui| {
                    ui.pressed = None;
                    if ui.dragging.take().is_some() {
                        ui.flush_opacity();
                    }
                });
            }
            WM_KEYDOWN => {
                launch(with(|ui| ui.key(VIRTUAL_KEY(wp.0 as u16))).flatten());
            }
            WM_TIMER if wp.0 == TIMER_SAVE => {
                with(Window::flush_opacity);
            }
            WM_DESTROY => {
                with(Window::flush_opacity);
                PostQuitMessage(0);
            }
            _ => return DefWindowProcW(hwnd, msg, wp, lp),
        }
    }
    LRESULT(0)
}
