//! UI strings in English and French, picked from the user's UI language.

#[derive(Clone, Copy)]
pub struct Strings {
    pub new: &'static str,
    pub new_folder: &'static str,
    pub sort: &'static str,
    pub view: &'static str,
    pub details: &'static str,
    pub search_in: &'static str,
    pub home: &'static str,
    pub sort_by: [&'static str; 4],
    pub ascending: &'static str,
    pub descending: &'static str,
    pub views: [&'static str; 8],
    pub select_all: &'static str,
    pub select_none: &'static str,
    pub properties: &'static str,
    pub items: [&'static str; 2],
    pub selected: [&'static str; 2],
}

pub const EN: Strings = Strings {
    new: "New folder",
    new_folder: "New folder",
    sort: "Sort",
    view: "View",
    details: "Details",
    search_in: "Search",
    home: "Home",
    sort_by: ["Name", "Date modified", "Type", "Size"],
    ascending: "Ascending",
    descending: "Descending",
    views: ["Extra large icons", "Large icons", "Medium icons", "Small icons", "List", "Details", "Tiles", "Content"],
    select_all: "Select all",
    select_none: "Select none",
    properties: "Properties",
    items: ["item", "items"],
    selected: ["item selected", "items selected"],
};

pub const FR: Strings = Strings {
    new: "Dossier",
    new_folder: "Nouveau dossier",
    sort: "Trier",
    view: "Afficher",
    details: "D\u{e9}tails",
    search_in: "Rechercher dans :",
    home: "Accueil",
    sort_by: ["Nom", "Modifi\u{e9} le", "Type", "Taille"],
    ascending: "Croissant",
    descending: "D\u{e9}croissant",
    views: [
        "Tr\u{e8}s grandes ic\u{f4}nes",
        "Grandes ic\u{f4}nes",
        "Ic\u{f4}nes moyennes",
        "Petites ic\u{f4}nes",
        "Liste",
        "D\u{e9}tails",
        "Mosa\u{ef}ques",
        "Contenu",
    ],
    select_all: "S\u{e9}lectionner tout",
    select_none: "Ne rien s\u{e9}lectionner",
    properties: "Propri\u{e9}t\u{e9}s",
    items: ["\u{e9}l\u{e9}ment", "\u{e9}l\u{e9}ments"],
    selected: ["\u{e9}l\u{e9}ment s\u{e9}lectionn\u{e9}", "\u{e9}l\u{e9}ments s\u{e9}lectionn\u{e9}s"],
};

/// Strings for a UI language tag such as `fr-FR`; English for anything else.
pub fn for_language(tag: &str) -> &'static Strings {
    if tag.to_ascii_lowercase().starts_with("fr") { &FR } else { &EN }
}

/// The search box hint for the folder `title`.
pub fn search_hint(s: &Strings, title: &str) -> String {
    format!("{} {title}", s.search_in)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn language_pick() {
        assert_eq!(for_language("fr-FR").sort, "Trier");
        assert_eq!(for_language("FR").view, "Afficher");
        assert_eq!(for_language("en-US").sort, "Sort");
        assert_eq!(for_language("de-DE").sort, "Sort");
        assert_eq!(search_hint(&EN, "Users"), "Search Users");
    }
}
