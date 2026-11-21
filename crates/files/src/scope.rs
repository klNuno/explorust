//! A search result must be a descendant of its scope. A sibling with the same prefix is outside.

#[cfg(any(target_os = "windows", test))]
pub(crate) fn contains(folder: &str, path: &str) -> bool {
    let key = |s: &str| s.replace('/', "\\").to_lowercase();
    let mut folder = key(folder);
    if !folder.ends_with('\\') {
        folder.push('\\');
    }
    key(path).strip_prefix(&folder).is_some_and(|rest| !rest.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_descendants_only() {
        assert!(contains(r"C:\Documents", r"c:\documents\sub\portrait.jpg"));
        assert!(contains(r"C:\", r"C:\portrait.jpg"));
        assert!(!contains(r"C:\Documents", r"C:\Documents-old\portrait.jpg"));
        assert!(!contains(r"C:\Documents", r"C:\Documents"));
        assert!(!contains(r"C:\Documents", r"N:\Documents\portrait.jpg"));
    }
}
