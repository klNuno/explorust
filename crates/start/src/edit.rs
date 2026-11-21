//! Search text selection. Offsets are UTF-8 byte boundaries in the current query.

use std::ops::Range;

#[derive(Default)]
pub struct Edit {
    pub caret: usize,
    anchor: usize,
}

impl Edit {
    pub fn place(&mut self, at: usize, extend: bool) {
        self.caret = at;
        if !extend {
            self.anchor = at;
        }
    }

    pub fn selection(&self) -> Range<usize> {
        self.anchor.min(self.caret)..self.anchor.max(self.caret)
    }

    pub fn replace(&mut self, text: &str, input: &str) -> String {
        let mut out = text.to_owned();
        let selection = self.selection();
        let end = selection.start + input.len();
        out.replace_range(selection, input);
        self.place(end, false);
        out
    }

    pub fn delete(&mut self, text: &str, right: bool, word: bool) -> String {
        if self.selection().is_empty() {
            self.anchor = step(text, self.caret, right, word);
        }
        self.replace(text, "")
    }

    pub fn step(&mut self, text: &str, right: bool, extend: bool, word: bool) {
        let selection = self.selection();
        let at = if !extend && !word && !selection.is_empty() {
            if right { selection.end } else { selection.start }
        } else {
            step(text, self.caret, right, word)
        };
        self.place(at, extend);
    }

    pub fn select_all(&mut self, text: &str) {
        self.anchor = 0;
        self.caret = text.len();
    }

    pub fn select_word(&mut self, text: &str, at: usize) {
        let Some(c) = text[at..].chars().next() else {
            self.place(at, false);
            return;
        };
        // A double click on whitespace or punctuation highlights that run.
        let class = group(c);
        self.anchor = text[..at].char_indices().rev()
            .take_while(|(_, c)| group(*c) == class).last().map_or(at, |(i, _)| i);
        self.caret = at + text[at..].chars().take_while(|c| group(*c) == class).map(char::len_utf8).sum::<usize>();
    }
}

fn group(c: char) -> u8 {
    if c.is_alphanumeric() || c == '_' { 0 } else if c.is_whitespace() { 1 } else { 2 }
}

fn step(text: &str, at: usize, right: bool, word: bool) -> usize {
    if !word {
        return if right {
            at + text[at..].chars().next().map_or(0, char::len_utf8)
        } else {
            text[..at].char_indices().next_back().map_or(at, |(i, _)| i)
        };
    }
    if right {
        let mut iter = text[at..].char_indices().peekable();
        if let Some(&(_, first)) = iter.peek() {
            while iter.peek().is_some_and(|(_, c)| group(*c) == group(first)) {
                iter.next();
            }
        }
        iter.find(|(_, c)| !c.is_whitespace()).map_or(text.len(), |(i, _)| at + i)
    } else {
        let mut iter = text[..at].char_indices().rev().skip_while(|(_, c)| c.is_whitespace());
        let Some((i, last)) = iter.next() else { return 0 };
        iter.take_while(|(_, c)| group(*c) == group(last)).last().map_or(i, |(i, _)| i)
    }
}

/// Maps a layout's UTF-16 insertion point to a UTF-8 byte offset.
pub fn byte_at_utf16(text: &str, units: u32) -> usize {
    let mut end = 0;
    for (at, c) in text.char_indices() {
        end += c.len_utf16() as u32;
        if units < end {
            return at;
        }
    }
    text.len()
}

#[cfg(test)]
mod tests {
    use super::Edit;

    #[test]
    fn typing_replaces_a_backward_selection_without_touching_its_neighbors() {
        let text = "test café 😀";
        let mut edit = Edit::default();
        edit.place(10, false);
        edit.place(5, true);
        assert_eq!(edit.replace(text, "ici"), "test ici 😀");
        assert_eq!(edit.selection(), 8..8);
    }

    #[test]
    fn moves_extends_and_deletes_across_non_ascii_characters() {
        let mut edit = Edit::default();
        let text = "a café 😀";
        edit.place(text.len(), false);
        edit.step(text, false, true, false);
        assert_eq!(&text[edit.selection()], "😀");
        edit.step(text, false, false, false);
        assert!(edit.selection().is_empty());
        assert_eq!(edit.delete(text, true, false), "a café ");
        assert_eq!(edit.delete("a café ", false, true), "a ");
        edit.select_all("a ");
        assert_eq!(edit.delete("a ", false, false), "");
    }

    #[test]
    fn hit_positions_and_double_clicks_handle_utf16_and_word_boundaries() {
        let text = "😀 café au lait";
        assert_eq!(super::byte_at_utf16(text, 1), 0);
        assert_eq!(super::byte_at_utf16(text, 2), 4);
        let mut edit = Edit::default();
        edit.select_word(text, 8);
        assert_eq!(&text[edit.selection()], "café");
        edit.step(text, true, false, true);
        assert_eq!(&text[edit.caret..], "au lait");
    }
}
