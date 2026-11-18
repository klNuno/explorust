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
