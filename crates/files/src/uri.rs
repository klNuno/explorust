//! The `search-ms:` URI the search box browses to: Windows Search runs the query and the
//! browser shows its results folder, as Explorer's own search box does.

fn encode(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(b as char),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// A search for `query` under the file system folder `location` (the whole index when `None`).
pub fn search(query: &str, location: Option<&str>, title: &str) -> String {
    let mut s = format!("search-ms:displayname={}&crumb=System.Generic.String%3A{}", encode(title), encode(query));
    if let Some(loc) = location {
        s.push_str(&format!("&crumb=location:{}", encode(loc)));
    }
    s
}

/// The `file:///` URL of a file system path, as Explorer reports in `LocationURL`.
pub fn file_url(path: &str) -> String {
    let mut out = String::from("file:///");
    for c in path.chars() {
        match c {
            '\\' => out.push('/'),
            ' ' => out.push_str("%20"),
            '#' => out.push_str("%23"),
            '%' => out.push_str("%25"),
            _ => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_urls() {
        assert_eq!(file_url("C:\\Windows"), "file:///C:/Windows");
        assert_eq!(file_url("C:\\a b\\#1"), "file:///C:/a%20b/%231");
    }

    #[test]
    fn encodes_query_and_location() {
        assert_eq!(
            search("a b", Some("C:\\Users"), "Users"),
            "search-ms:displayname=Users&crumb=System.Generic.String%3Aa%20b&crumb=location:C%3A%5CUsers"
        );
        assert_eq!(search("é", None, "x"), "search-ms:displayname=x&crumb=System.Generic.String%3A%C3%A9");
    }
}
