//! Start's UI text in English (the default) and French, picked from the Windows UI language.

pub struct Strings {
    pub french: bool,
    pub search: &'static str,
    pub pinned: &'static str,
    pub all: &'static str,
    pub all_apps: &'static str,
    pub back: &'static str,
    pub recommended: &'static str,
    pub empty_rec: &'static str,
    pub recently_added: &'static str,
    pub results: &'static str,
    pub no_results: &'static str,
    pub pin: &'static str,
    pub unpin: &'static str,
    pub to_front: &'static str,
    pub sleep: &'static str,
    pub shutdown: &'static str,
    pub restart: &'static str,
    pub sign_out: &'static str,
    pub lock: &'static str,
    pub best_match: &'static str,
    pub settings: &'static str,
    pub folders: &'static str,
    pub documents: &'static str,
    pub open: &'static str,
    pub run_as: &'static str,
    pub location: &'static str,
    pub copy_path: &'static str,
    pub copy: &'static str,
    pub kind_app: &'static str,
    pub kind_setting: &'static str,
    pub kind_file: &'static str,
    pub kind_folder: &'static str,
    pub kind_calc: &'static str,
    pub searching: &'static str,
}

pub const EN: Strings = Strings {
    french: false,
    search: "Search for apps, settings, and documents",
    pinned: "Pinned",
    all: "All",
    all_apps: "All apps",
    back: "Back",
    recommended: "Recommended",
    empty_rec: "The more you use your device, the more we'll show your new apps and recent files here.",
    recently_added: "Recently added",
    results: "Apps",
    no_results: "No results",
    pin: "Pin to Start",
    unpin: "Unpin from Start",
    to_front: "Move to front",
    sleep: "Sleep",
    shutdown: "Shut down",
    restart: "Restart",
    sign_out: "Sign out",
    lock: "Lock",
    best_match: "Best match",
    settings: "Settings",
    folders: "Folders",
    documents: "Documents",
    open: "Open",
    run_as: "Run as administrator",
    location: "Open file location",
    copy_path: "Copy full path",
    copy: "Copy",
    kind_app: "App",
    kind_setting: "System settings",
    kind_file: "File",
    kind_folder: "Folder",
    kind_calc: "Calculator",
    searching: "Searching files",
};

pub const FR: Strings = Strings {
    french: true,
    search: "Rechercher des applications, des paramètres et des documents",
    pinned: "Épinglé",
    all: "Tout",
    all_apps: "Toutes les applications",
    back: "Retour",
    recommended: "Recommandé",
    empty_rec: "Plus vous utilisez votre appareil, plus vos nouvelles applications et fichiers récents s'afficheront ici.",
    recently_added: "Ajoutée récemment",
    results: "Applications",
    no_results: "Aucun résultat",
    pin: "Épingler à l'écran de démarrage",
    unpin: "Détacher de l'écran de démarrage",
    to_front: "Placer en premier",
    sleep: "Mettre en veille",
    shutdown: "Arrêter",
    restart: "Redémarrer",
    sign_out: "Se déconnecter",
    lock: "Verrouiller",
    best_match: "Meilleur résultat",
    settings: "Paramètres",
    folders: "Dossiers",
    documents: "Documents",
    open: "Ouvrir",
    run_as: "Exécuter en tant qu'administrateur",
    location: "Ouvrir l'emplacement du fichier",
    copy_path: "Copier le chemin d'accès complet",
    copy: "Copier",
    kind_app: "Application",
    kind_setting: "Paramètres système",
    kind_file: "Fichier",
    kind_folder: "Dossier",
    kind_calc: "Calculatrice",
    searching: "Recherche de fichiers",
};

/// French when the primary language id of the UI language is LANG_FRENCH (0x0C).
pub fn for_langid(langid: u16) -> &'static Strings {
    if langid & 0x3FF == 0x0C { &FR } else { &EN }
}

#[cfg(test)]
mod tests {
    #[test]
    fn picks_french_for_any_french_locale() {
        assert!(super::for_langid(0x040C).french);
        assert!(super::for_langid(0x0C0C).french);
        assert!(!super::for_langid(0x0409).french);
    }
}
