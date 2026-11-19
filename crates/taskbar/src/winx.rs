//! The Win+X menu (also a right-click on Start) as data: Windows 11's items in its order, their
//! labels in English and French with access keys, and what each one runs.

/// What an item does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Act {
    /// A URI or a file opened through ShellExecute.
    Open(&'static str),
    /// Windows Terminal (falls back to PowerShell), `admin` through the runas verb.
    Terminal { admin: bool },
    /// The Run dialog.
    Run,
    /// explorust's task manager (explorust-taskman.exe beside explorust.exe), else Windows' taskmgr.exe.
    TaskManager,
    /// A bus command answered by another part.
    Search,
    Files,
    Desktop,
    SignOut,
    Sleep,
    ShutDown,
    Restart,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Item {
    pub id: u32,
    pub en: &'static str,
    pub fr: &'static str,
    pub act: Act,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Entry {
    Item(Item),
    Separator,
    /// The "Shut down or sign out" submenu.
    Power { en: &'static str, fr: &'static str },
}

const fn it(id: u32, en: &'static str, fr: &'static str, act: Act) -> Entry {
    Entry::Item(Item { id, en, fr, act })
}

/// The menu top to bottom, as Windows 11 24H2 shows it on a desktop PC (no Mobility Center).
pub const MENU: [Entry; 20] = [
    it(1, "Installed a&pps", "A&pplications installées", Act::Open("ms-settings:appsfeatures")),
    it(2, "Power &Options", "&Options d'alimentation", Act::Open("ms-settings:powersleep")),
    it(3, "Event &Viewer", "Observateur d'é&vénements", Act::Open("eventvwr.msc")),
    it(4, "S&ystem", "S&ystème", Act::Open("ms-settings:about")),
    it(5, "Device &Manager", "Gestionnaire de périp&hériques", Act::Open("devmgmt.msc")),
    it(6, "Net&work Connections", "Connexions ré&seau", Act::Open("ms-settings:network")),
    it(7, "Dis&k Management", "Gestion des dis&ques", Act::Open("diskmgmt.msc")),
    it(8, "Computer Mana&gement", "&Gestion de l'ordinateur", Act::Open("compmgmt.msc")),
    it(9, "Term&inal", "Term&inal", Act::Terminal { admin: false }),
    it(10, "Terminal (&Admin)", "Terminal (&admin)", Act::Terminal { admin: true }),
    Entry::Separator,
    it(11, "&Task Manager", "Gestionnaire des &tâches", Act::TaskManager),
    it(12, "Setti&ngs", "Para&mètres", Act::Open("ms-settings:")),
    it(13, "File &Explorer", "&Explorateur de fichiers", Act::Files),
    it(14, "&Search", "&Rechercher", Act::Search),
    it(15, "&Run", "E&xécuter", Act::Run),
    Entry::Separator,
    Entry::Power { en: "Sh&ut down or sign out", fr: "Arrêter o&u se déconnecter" },
    it(16, "&Desktop", "&Bureau", Act::Desktop),
    Entry::Separator,
];

/// The submenu of `Entry::Power`.
pub const POWER: [Item; 4] = [
    Item { id: 20, en: "S&ign out", fr: "Se &déconnecter", act: Act::SignOut },
    Item { id: 21, en: "&Sleep", fr: "&Mettre en veille", act: Act::Sleep },
    Item { id: 22, en: "Sh&ut down", fr: "&Arrêter", act: Act::ShutDown },
    Item { id: 23, en: "&Restart", fr: "&Redémarrer", act: Act::Restart },
];

/// The entries to show: `MENU` without its trailing separator.
pub fn entries() -> &'static [Entry] {
    &MENU[..MENU.len() - 1]
}

impl Item {
    pub fn label(&self, french: bool) -> &'static str {
        if french { self.fr } else { self.en }
    }
}

/// The item a menu id stands for.
pub fn by_id(id: u32) -> Option<Item> {
    MENU.iter()
        .filter_map(|e| if let Entry::Item(i) = e { Some(*i) } else { None })
        .chain(POWER)
        .find(|i| i.id == id)
}

/// French UI when the user's UI language is French (primary language id 0x0C), English otherwise.
pub fn is_french(langid: u16) -> bool {
    langid & 0x3ff == 0x0c
}

#[cfg(test)]
mod tests {
    use super::*;

    fn access_keys(labels: Vec<&str>) -> Vec<char> {
        labels
            .iter()
            .map(|l| {
                let i = l.find('&').expect(l);
                l[i + 1..].chars().next().unwrap().to_lowercase().next().unwrap()
            })
            .collect()
    }

    #[test]
    fn ids_are_unique_and_found() {
        let mut ids: Vec<u32> = MENU.iter().filter_map(|e| if let Entry::Item(i) = e { Some(i.id) } else { None }).collect();
        ids.extend(POWER.iter().map(|i| i.id));
        let n = ids.len();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), n);
        assert_eq!(by_id(13).unwrap().act, Act::Files);
        assert_eq!(by_id(22).unwrap().act, Act::ShutDown);
        assert!(by_id(99).is_none());
    }

    #[test]
    fn access_keys_are_unique_per_language() {
        for french in [false, true] {
            let mut labels: Vec<&str> = entries()
                .iter()
                .filter_map(|e| match e {
                    Entry::Item(i) => Some(i.label(french)),
                    Entry::Power { en, fr } => Some(if french { *fr } else { *en }),
                    Entry::Separator => None,
                })
                .collect();
            let keys = access_keys(labels.clone());
            let mut k = keys.clone();
            k.sort();
            k.dedup();
            assert_eq!(k.len(), keys.len(), "french {french}: {keys:?}");
            labels = POWER.iter().map(|i| i.label(french)).collect();
            let keys = access_keys(labels);
            let mut k = keys.clone();
            k.sort();
            k.dedup();
            assert_eq!(k.len(), keys.len(), "power french {french}: {keys:?}");
        }
    }

    #[test]
    fn order_and_language() {
        let e = entries();
        assert_eq!(e.len(), 19);
        assert!(matches!(e[0], Entry::Item(Item { id: 1, .. })));
        assert!(matches!(e[10], Entry::Separator));
        assert!(matches!(e[17], Entry::Power { .. }));
        assert!(matches!(e[18], Entry::Item(Item { act: Act::Desktop, .. })));
        assert!(is_french(0x040c) && is_french(0x0c0c) && !is_french(0x0409));
    }
}
