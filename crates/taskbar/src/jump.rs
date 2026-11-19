//! A button's jump list as data, in Windows' order: the app's pinned items, its recent (or
//! frequent) items, its tasks, then the app name (starts a new instance), pin or unpin, and close
//! window or close all windows. Menu ids say which list an item came from.

pub const ID_LAUNCH: u32 = 1;
pub const ID_PIN: u32 = 2;
pub const ID_CLOSE: u32 = 3;
pub const ID_PINNED: u32 = 100;
pub const ID_RECENT: u32 = 300;
pub const ID_TASK: u32 = 500;
const MAX_PER_LIST: usize = 10;

#[derive(Default)]
pub struct Input {
    pub app: String,
    pub pinned_items: Vec<String>,
    pub recent: Vec<String>,
    /// True when `recent` holds the frequent list (apps that ask for it).
    pub frequent: bool,
    pub tasks: Vec<String>,
    pub pinned_to_bar: bool,
    pub windows: usize,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Entry {
    Header(&'static str),
    Item(u32, String),
    Separator,
}

/// What a chosen id means.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pick {
    Launch,
    Pin,
    Close,
    Pinned(usize),
    Recent(usize),
    Task(usize),
}

pub fn pick(id: u32) -> Option<Pick> {
    Some(match id {
        ID_LAUNCH => Pick::Launch,
        ID_PIN => Pick::Pin,
        ID_CLOSE => Pick::Close,
        i if (ID_PINNED..ID_RECENT).contains(&i) => Pick::Pinned((i - ID_PINNED) as usize),
        i if (ID_RECENT..ID_TASK).contains(&i) => Pick::Recent((i - ID_RECENT) as usize),
        i if (ID_TASK..ID_TASK + 200).contains(&i) => Pick::Task((i - ID_TASK) as usize),
        _ => return None,
    })
}

/// Escapes `&` so a file name is not read as an access key.
fn plain(s: &str) -> String {
    s.replace('&', "&&")
}

pub fn build(i: &Input, french: bool) -> Vec<Entry> {
    let t = |en: &'static str, fr: &'static str| if french { fr } else { en };
    let mut out = Vec::new();
    let section = |out: &mut Vec<Entry>, title: &'static str, items: &[String], base: u32| {
        if items.is_empty() {
            return;
        }
        out.push(Entry::Header(title));
        for (n, s) in items.iter().take(MAX_PER_LIST).enumerate() {
            out.push(Entry::Item(base + n as u32, plain(s)));
        }
        out.push(Entry::Separator);
    };
    section(&mut out, t("Pinned", "Épinglé"), &i.pinned_items, ID_PINNED);
    let recent_title = if i.frequent { t("Frequent", "Fréquent") } else { t("Recent", "Récent") };
    section(&mut out, recent_title, &i.recent, ID_RECENT);
    section(&mut out, t("Tasks", "Tâches"), &i.tasks, ID_TASK);
    out.push(Entry::Item(ID_LAUNCH, plain(&i.app)));
    let pin = if i.pinned_to_bar {
        t("Unpin from taskbar", "Détacher de la barre des tâches")
    } else {
        t("Pin to taskbar", "Épingler à la barre des tâches")
    };
    out.push(Entry::Item(ID_PIN, pin.to_string()));
    match i.windows {
        0 => {}
        1 => out.push(Entry::Item(ID_CLOSE, t("Close window", "Fermer la fenêtre").to_string())),
        _ => out.push(Entry::Item(ID_CLOSE, t("Close all windows", "Fermer toutes les fenêtres").to_string())),
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn running_app_without_lists() {
        let e = build(&Input { app: "Notepad".into(), windows: 1, ..Default::default() }, false);
        assert_eq!(
            e,
            [
                Entry::Item(ID_LAUNCH, "Notepad".into()),
                Entry::Item(ID_PIN, "Pin to taskbar".into()),
                Entry::Item(ID_CLOSE, "Close window".into()),
            ]
        );
    }

    #[test]
    fn sections_in_order() {
        let i = Input {
            app: "A & B".into(),
            pinned_items: vec!["p".into()],
            recent: (0..12).map(|n| format!("r{n}")).collect(),
            tasks: vec!["New window".into()],
            pinned_to_bar: true,
            windows: 2,
            ..Default::default()
        };
        let e = build(&i, true);
        assert_eq!(e[0], Entry::Header("Épinglé"));
        assert_eq!(e[1], Entry::Item(ID_PINNED, "p".into()));
        assert_eq!(e[2], Entry::Separator);
        assert_eq!(e[3], Entry::Header("Récent"));
        assert_eq!(e[4 + 9], Entry::Item(ID_RECENT + 9, "r9".into()));
        assert_eq!(e[14], Entry::Separator);
        assert_eq!(e[15], Entry::Header("Tâches"));
        assert_eq!(e[16], Entry::Item(ID_TASK, "New window".into()));
        assert_eq!(e[18], Entry::Item(ID_LAUNCH, "A && B".into()));
        assert_eq!(e[19], Entry::Item(ID_PIN, "Détacher de la barre des tâches".into()));
        assert_eq!(e[20], Entry::Item(ID_CLOSE, "Fermer toutes les fenêtres".into()));
    }

    #[test]
    fn picks() {
        assert_eq!(pick(ID_RECENT + 3), Some(Pick::Recent(3)));
        assert_eq!(pick(ID_PINNED), Some(Pick::Pinned(0)));
        assert_eq!(pick(ID_TASK + 1), Some(Pick::Task(1)));
        assert_eq!(pick(ID_CLOSE), Some(Pick::Close));
        assert_eq!(pick(0), None);
    }
}
