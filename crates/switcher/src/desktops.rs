//! Ctrl+Win+D, Ctrl+Win+Left/Right and Ctrl+Win+F4 on Windows' virtual desktops. The connection to
//! the service is made on first use and made again once when a call fails (the shell restarted).

use crate::vdesk::{Desk, Vd};
use crate::{log, model};
use std::cell::RefCell;
use std::time::Instant;
use windows_core::Result;

thread_local! {
    static VD: RefCell<Option<Vd>> = const { RefCell::new(None) };
}

/// Runs `f` on the service, connecting (or reconnecting once after an error) as needed.
pub fn with_vd<R>(f: impl Fn(&Vd) -> Result<R>) -> Result<R> {
    let cached = VD.with(|c| c.borrow_mut().take());
    let vd = match cached {
        Some(v) => v,
        None => {
            let t = Instant::now();
            log::line("desktops: connecting to the immersive shell");
            let vd = Vd::connect().inspect_err(|e| log::line(&format!("desktops: service not reached: {e}")))?;
            log::line(&format!("desktops: service reached in {:.1} ms", ms(t)));
            vd
        }
    };
    match f(&vd) {
        Ok(r) => {
            VD.with(|c| *c.borrow_mut() = Some(vd));
            Ok(r)
        }
        Err(e) => {
            drop(vd);
            let vd = Vd::connect()?;
            let r = f(&vd);
            if r.is_ok() {
                VD.with(|c| *c.borrow_mut() = Some(vd));
            } else {
                log::line(&format!("desktops: call failed twice: {e}"));
            }
            r
        }
    }
}

fn state(vd: &Vd) -> Result<(Vec<Desk>, usize)> {
    let desks = vd.desktops()?;
    let cur = vd.current_index(&desks).unwrap_or(0);
    Ok((desks, cur))
}

fn names(desks: &[Desk], cur: usize) -> String {
    desks
        .iter()
        .enumerate()
        .map(|(i, d)| if i == cur { format!("[{}]", d.name) } else { d.name.clone() })
        .collect::<Vec<_>>()
        .join(", ")
}

/// Ctrl+Win+D: a new desktop at the end, switched to, as Windows does.
pub fn new_desktop() {
    let t = Instant::now();
    let r = with_vd(|vd| {
        let d = vd.create()?;
        let how = vd.switch(&d)?;
        let (desks, cur) = state(vd)?;
        Ok((how, names(&desks, cur)))
    });
    match r {
        Ok((how, all)) => log::line(&format!("desktop new: via {how} in {:.1} ms, now {all}", ms(t))),
        Err(e) => log::line(&format!("desktop new: {e}")),
    }
}

/// Ctrl+Win+Left (`dir` -1) or Right (1).
pub fn switch(dir: isize) {
    let t = Instant::now();
    let r = with_vd(|vd| {
        let (desks, cur) = state(vd)?;
        let Some(to) = model::adjacent(cur, desks.len(), dir) else { return Ok(None) };
        let how = vd.switch(&desks[to])?;
        let (desks, cur) = state(vd)?;
        Ok(Some((how, names(&desks, cur))))
    });
    match r {
        Ok(Some((how, all))) => log::line(&format!("desktop switch {dir}: via {how} in {:.1} ms, now {all}", ms(t))),
        Ok(None) => log::line(&format!("desktop switch {dir}: at the end of the strip, nothing to do")),
        Err(e) => log::line(&format!("desktop switch {dir}: {e}")),
    }
}

/// Switches to desktop `i` of the strip.
pub fn switch_to(i: usize) -> Result<&'static str> {
    with_vd(|vd| {
        let desks = vd.desktops()?;
        let d = desks.get(i).ok_or(windows_core::Error::from_hresult(windows_core::HRESULT(0x80070057u32 as i32)))?;
        vd.switch(d)
    })
}

/// Ctrl+Win+F4: closes the current desktop, its windows go to the neighbour Windows picks.
pub fn close_current() {
    let t = Instant::now();
    let r = with_vd(|vd| {
        let (desks, cur) = state(vd)?;
        let Some(to) = model::close_fallback(cur, desks.len()) else { return Ok(None) };
        vd.remove(&desks[cur], &desks[to])?;
        let (desks, cur) = state(vd)?;
        Ok(Some(names(&desks, cur)))
    });
    match r {
        Ok(Some(all)) => log::line(&format!("desktop close: in {:.1} ms, now {all}", ms(t))),
        Ok(None) => log::line("desktop close: the last desktop stays"),
        Err(e) => log::line(&format!("desktop close: {e}")),
    }
}

/// Closes desktop `i` (the Task View close button).
pub fn close(i: usize) -> Result<()> {
    with_vd(|vd| {
        let desks = vd.desktops()?;
        let cur = vd.current_index(&desks).unwrap_or(0);
        let Some(to) = model::close_fallback(i, desks.len()) else { return Ok(()) };
        let to = if i == cur { to } else { cur };
        vd.remove(&desks[i], &desks[to])
    })
}

fn ms(t: Instant) -> f32 {
    t.elapsed().as_secs_f32() * 1000.0
}
