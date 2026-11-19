//! explorust as the folder handler: the per-user (HKCU) classes the shell reads for the open and
//! explore verbs of folders, directories and drives, and the command lines it then starts.

/// Classes whose verbs open a folder window.
pub const CLASSES: [&str; 3] = ["Folder", "Directory", "Drive"];
/// Verbs pointed at `explorust-open.exe`.
pub const VERBS: [&str; 2] = ["open", "explore"];
/// Value on each verb key we write, so unregistering removes only ours.
pub const MARKER: &str = "explorust";

/// The verb keys under `HKCU`, e.g. `Software\Classes\Folder\shell\open`.
pub fn verb_keys() -> Vec<String> {
    CLASSES
        .iter()
        .flat_map(|c| VERBS.iter().map(move |v| format!("Software\\Classes\\{c}\\shell\\{v}")))
        .collect()
}

/// The command a verb runs: `"<exe>" "%1"`.
pub fn command(exe: &str) -> String {
    format!("\"{exe}\" \"%1\"")
}

/// What a command line asks: the path to show, and whether it names an item to select
/// (Explorer's `/select,<path>`). Explorer's `/e,` `/n,` `/root,` prefixes are dropped.
pub fn parse_args(args: &[String]) -> (Option<String>, bool) {
    let mut path = None;
    let mut select = false;
    for a in args {
        let mut s = a.trim().trim_matches('"').to_string();
        loop {
            let lower = s.to_ascii_lowercase();
            let cut = ["/select,", "/e,", "/n,", "/root,"].iter().find(|p| lower.starts_with(**p)).copied();
            match cut {
                Some(p) => {
                    select |= p == "/select,";
                    s = s[p.len()..].trim().trim_matches('"').to_string();
                }
                None => break,
            }
        }
        let lower = s.to_ascii_lowercase();
        if lower == "/select" {
            select = true;
            continue;
        }
        if matches!(lower.as_str(), "/e" | "/n" | "/separate" | "") {
            continue;
        }
        path = Some(s);
    }
    (path, select)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(a: &[&str]) -> Vec<String> {
        a.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn keys_cover_every_class_and_verb() {
        let k = verb_keys();
        assert_eq!(k.len(), 6);
        assert!(k.contains(&"Software\\Classes\\Drive\\shell\\explore".to_string()));
    }

    #[test]
    fn command_quotes_both() {
        assert_eq!(command("C:\\explorust\\explorust-open.exe"), "\"C:\\explorust\\explorust-open.exe\" \"%1\"");
    }

    #[test]
    fn parses_explorer_style_arguments() {
        assert_eq!(parse_args(&v(&["C:\\Windows"])), (Some("C:\\Windows".into()), false));
        assert_eq!(parse_args(&v(&["/select,C:\\Windows\\win.ini"])), (Some("C:\\Windows\\win.ini".into()), true));
        assert_eq!(parse_args(&v(&["/select,\"C:\\a b\\c.txt\""])), (Some("C:\\a b\\c.txt".into()), true));
        assert_eq!(parse_args(&v(&["/e,", "C:\\"])), (Some("C:\\".into()), false));
        assert_eq!(parse_args(&v(&["/n,/select,C:\\x"])), (Some("C:\\x".into()), true));
        assert_eq!(parse_args(&v(&[])), (None, false));
    }
}
