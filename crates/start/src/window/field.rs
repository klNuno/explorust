//! Mouse and keyboard interaction with the Start search field.

use super::{redraw, search, with};
use crate::{layout::{self as L, Hit}, sources};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetKeyState, VK_BACK, VK_CONTROL, VK_DELETE, VK_END, VK_HOME, VK_LEFT, VK_MENU, VK_RIGHT, VK_SHIFT,
};

pub fn press(x: f32, y: f32, word: bool) -> bool {
    if !L::SEARCH.contains(x, y) {
        return false;
    }
    // SAFETY: reads the modifier state associated with this message.
    let shift = unsafe { GetKeyState(VK_SHIFT.0 as i32) < 0 };
    let pressed = with(|s| {
        if !s.shown {
            return false;
        }
        let at = s.search_hit(x, word);
        if word {
            s.edit.select_word(&s.query, at);
        } else {
            s.edit.place(at, shift);
        }
        s.search_drag = !word;
        s.search_focus = true;
        s.focus = Hit::Search;
        s.dirty = true;
        true
    }) == Some(true);
    if pressed {
        redraw();
    }
    pressed
}

pub fn drag(x: f32) -> bool {
    let dragging = with(|s| {
        if !s.search_drag {
            return false;
        }
        let at = s.search_hit(x, false);
        if at != s.edit.caret {
            s.edit.place(at, true);
            s.dirty = true;
        }
        true
    }) == Some(true);
    if dragging {
        redraw();
    }
    dragging
}

pub fn key(vk: u16) -> bool {
    let Some(hwnd) = with(|s| (s.shown && s.search_focus).then_some(s.hwnd)).flatten() else { return false };
    // SAFETY: reads modifier states for this key message. AltGr can include Ctrl.
    let (shift, ctrl, alt) = unsafe {
        (GetKeyState(VK_SHIFT.0 as i32) < 0, GetKeyState(VK_CONTROL.0 as i32) < 0, GetKeyState(VK_MENU.0 as i32) < 0)
    };
    if alt {
        return false;
    }
    match vk {
        v if v == VK_BACK.0 || v == VK_DELETE.0 => {
            let right = v == VK_DELETE.0;
            search::answer(if right { "delete" } else { "backspace" }, |edit, query| edit.delete(query, right, ctrl));
        }
        v if v == VK_LEFT.0 || v == VK_RIGHT.0 => {
            with(|s| {
                s.edit.step(&s.query, v == VK_RIGHT.0, shift, ctrl);
                s.dirty = true;
            });
            redraw();
        }
        v if v == VK_HOME.0 || v == VK_END.0 => {
            with(|s| {
                s.edit.place(if v == VK_END.0 { s.query.len() } else { 0 }, shift);
                s.dirty = true;
            });
            redraw();
        }
        0x41 if ctrl => {
            with(|s| {
                s.edit.select_all(&s.query);
                s.dirty = true;
            });
            redraw();
        }
        0x43 | 0x58 if ctrl => {
            let selected = with(|s| s.query[s.edit.selection()].to_owned()).unwrap_or_default();
            if !selected.is_empty() && sources::copy_text(hwnd, &selected) && vk == 0x58 {
                search::answer("cut", |edit, query| edit.replace(query, ""));
            }
        }
        0x56 if ctrl => {
            if let Some(text) = sources::clipboard_text(hwnd) {
                let mut text = text.replace("\r\n", " ").replace(['\n', '\r', '\t'], " ");
                text.retain(|c| !c.is_control());
                if !text.is_empty() {
                    search::answer("paste", |edit, query| edit.replace(query, &text));
                }
            }
        }
        _ => return false,
    }
    true
}
