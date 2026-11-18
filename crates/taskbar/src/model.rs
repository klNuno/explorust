//! The task buttons as data: pins in the Taskband order, then running apps in first-seen order,
//! windows grouped by app identity (lowercase AUMID or exe path). What a click does.

pub type Hwnd = isize;

pub struct TaskWin {
    pub hwnd: Hwnd,
    pub key: String,
    /// Activation stamp, 0 when never active since it was seen.
    pub last_active: u64,
    pub flashing: bool,
}

#[derive(Default)]
pub struct Model {
    /// Pin keys in the bar's order.
    pub pins: Vec<String>,
    /// Windows in first-seen order.
    pub wins: Vec<TaskWin>,
    pub active: Option<Hwnd>,
    tick: u64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Group {
    pub key: String,
    /// Index in `Model::pins`.
    pub pin: Option<usize>,
    /// Its windows, first seen first.
    pub wins: Vec<Hwnd>,
    pub flashing: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    Launch,
    Activate(Hwnd),
    Minimize(Hwnd),
}

impl Model {
    /// Adds a window or updates its key; true when the buttons change.
    pub fn upsert(&mut self, hwnd: Hwnd, key: &str) -> bool {
        if let Some(w) = self.wins.iter_mut().find(|w| w.hwnd == hwnd) {
            if w.key == key {
                return false;
            }
            w.key = key.to_string();
            return true;
        }
        let last_active = if self.active == Some(hwnd) { self.bump() } else { 0 };
        self.wins.push(TaskWin { hwnd, key: key.to_string(), last_active, flashing: false });
        true
    }

    pub fn remove(&mut self, hwnd: Hwnd) -> bool {
        let n = self.wins.len();
        self.wins.retain(|w| w.hwnd != hwnd);
        n != self.wins.len()
    }

    pub fn contains(&self, hwnd: Hwnd) -> bool {
        self.wins.iter().any(|w| w.hwnd == hwnd)
    }

    fn bump(&mut self) -> u64 {
        self.tick += 1;
        self.tick
    }

    /// The foreground window changed; true when the buttons change.
    pub fn set_active(&mut self, hwnd: Option<Hwnd>) -> bool {
        if self.active == hwnd {
            return false;
        }
        self.active = hwnd;
        if let Some(h) = hwnd {
            let t = self.bump();
            if let Some(w) = self.wins.iter_mut().find(|w| w.hwnd == h) {
                w.last_active = t;
                w.flashing = false;
            }
        }
        true
    }

    pub fn set_flash(&mut self, hwnd: Hwnd, on: bool) -> bool {
        match self.wins.iter_mut().find(|w| w.hwnd == hwnd) {
            Some(w) if w.flashing != on => {
                w.flashing = on;
                true
            }
            _ => false,
        }
    }

    pub fn groups(&self) -> Vec<Group> {
        let mut out: Vec<Group> = Vec::new();
        for (i, key) in self.pins.iter().enumerate() {
            if out.iter().any(|g| &g.key == key) {
                continue;
            }
            out.push(Group { key: key.clone(), pin: Some(i), wins: Vec::new(), flashing: false });
        }
        for w in &self.wins {
            let g = match out.iter().position(|g| g.key == w.key) {
                Some(i) => &mut out[i],
                None => {
                    out.push(Group { key: w.key.clone(), pin: None, wins: Vec::new(), flashing: false });
                    out.last_mut().unwrap()
                }
            };
            g.wins.push(w.hwnd);
            g.flashing |= w.flashing;
        }
        out
    }

    /// Whether the group holds the foreground window.
    pub fn is_active(&self, g: &Group) -> bool {
        self.active.is_some_and(|a| g.wins.contains(&a))
    }

    /// Left click (or Win+number) on a group; `new_instance` for middle or Shift+click.
    pub fn click(&self, g: &Group, new_instance: bool) -> Action {
        if new_instance || g.wins.is_empty() {
            return Action::Launch;
        }
        if let Some(a) = self.active.filter(|a| g.wins.contains(a)) {
            if g.wins.len() == 1 {
                return Action::Minimize(a);
            }
            let i = g.wins.iter().position(|w| *w == a).unwrap_or(0);
            return Action::Activate(g.wins[(i + 1) % g.wins.len()]);
        }
        let last = |h: &Hwnd| self.wins.iter().find(|w| w.hwnd == *h).map_or(0, |w| w.last_active);
        // The most recently active window of the group; the first one on a tie.
        let mut best = g.wins[0];
        for h in &g.wins {
            if last(h) > last(&best) {
                best = *h;
            }
        }
        Action::Activate(best)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn model() -> Model {
        let mut m = Model { pins: vec!["edge".into(), "calc".into()], ..Default::default() };
        m.upsert(10, "notepad");
        m.upsert(11, "calc");
        m.upsert(12, "settings");
        m.upsert(13, "notepad");
        m
    }

    #[test]
    fn pins_first_then_first_seen() {
        let g = model().groups();
        let keys: Vec<&str> = g.iter().map(|g| g.key.as_str()).collect();
        assert_eq!(keys, ["edge", "calc", "notepad", "settings"]);
        assert_eq!(g[1].wins, [11]);
        assert_eq!(g[2].wins, [10, 13]);
        assert_eq!(g[0].pin, Some(0));
        assert_eq!(g[2].pin, None);
    }

    #[test]
    fn click_launches_activates_minimizes() {
        let mut m = model();
        let g = m.groups();
        assert_eq!(m.click(&g[0], false), Action::Launch);
        assert_eq!(m.click(&g[1], false), Action::Activate(11));
        m.set_active(Some(11));
        assert_eq!(m.click(&g[1], false), Action::Minimize(11));
        assert_eq!(m.click(&g[1], true), Action::Launch);
    }

    #[test]
    fn click_cycles_a_group() {
        let mut m = model();
        let g = m.groups();
        assert_eq!(m.click(&g[2], false), Action::Activate(10));
        m.set_active(Some(10));
        assert_eq!(m.click(&g[2], false), Action::Activate(13));
        m.set_active(Some(13));
        assert_eq!(m.click(&g[2], false), Action::Activate(10));
        // From another app, the last active window of the group comes back.
        m.set_active(Some(12));
        assert_eq!(m.click(&g[2], false), Action::Activate(13));
    }

    #[test]
    fn remove_and_rekey() {
        let mut m = model();
        assert!(m.remove(11));
        assert!(!m.remove(11));
        assert!(m.upsert(12, "calc"));
        assert!(!m.upsert(12, "calc"));
        let g = m.groups();
        assert_eq!(g.len(), 3);
        assert_eq!(g[1].wins, [12]);
    }

    #[test]
    fn flash_clears_on_activation() {
        let mut m = model();
        assert!(m.set_flash(12, true));
        assert!(m.groups()[3].flashing);
        m.set_active(Some(12));
        assert!(!m.groups()[3].flashing);
    }
}
