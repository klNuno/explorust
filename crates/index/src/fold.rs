//! Case folding that keeps byte lengths and can be undone: a character is lowered only when its
//! lowercase form is one character of the same UTF-8 length whose uppercase is the original, so
//! a folded name keeps the original's offsets and one bit per character restores it.

/// The folded form of `c`, or `c` itself.
fn lower(c: char) -> char {
    if c.is_ascii() {
        return c.to_ascii_lowercase();
    }
    let mut l = c.to_lowercase();
    match (l.next(), l.next()) {
        (Some(x), None) if x != c && x.len_utf8() == c.len_utf8() && upper(x) == c => x,
        _ => c,
    }
}

/// The inverse of `lower` for a character it changed.
pub fn upper(c: char) -> char {
    if c.is_ascii() {
        return c.to_ascii_uppercase();
    }
    let mut u = c.to_uppercase();
    match (u.next(), u.next()) {
        (Some(x), None) => x,
        _ => c,
    }
}

/// Appends the folded `s` to `out`; `changed` gets the offset in `s` of each lowered character.
pub fn push_folded(s: &str, out: &mut Vec<u8>, mut changed: impl FnMut(usize)) {
    for (i, c) in s.char_indices() {
        let l = lower(c);
        if l != c {
            changed(i);
        }
        let mut b = [0u8; 4];
        out.extend_from_slice(l.encode_utf8(&mut b).as_bytes());
    }
}

pub fn fold(s: &str) -> String {
    let mut v = Vec::with_capacity(s.len());
    push_folded(s, &mut v, |_| {});
    // Lowering a valid character keeps valid UTF-8.
    String::from_utf8(v).unwrap_or_default()
}

/// Rebuilds the original from the folded text and the changed offsets (`is_changed`).
pub fn unfold(folded: &str, is_changed: impl Fn(usize) -> bool) -> String {
    folded.char_indices().map(|(i, c)| if is_changed(i) { upper(c) } else { c }).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_length_and_back() {
        for s in ["Hello World.TXT", "Évènement Ça", "İstanbul", "ẞig", "Straße", "\u{212A}elvin", "ΣΊΣΥΦΟΣ"] {
            let mut marks = Vec::new();
            let mut v = Vec::new();
            push_folded(s, &mut v, |i| marks.push(i));
            let f = String::from_utf8(v).unwrap();
            assert_eq!(f.len(), s.len(), "{s}");
            assert_eq!(unfold(&f, |i| marks.contains(&i)), s, "{s}");
        }
        assert_eq!(fold("Évènement.TXT"), "évènement.txt");
    }
}
