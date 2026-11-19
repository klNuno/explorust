//! A one-line text editor for the address and search boxes: caret, select all, the keys a
//! Windows text box answers. Drawing and the clipboard stay with the window.

#[derive(Default, Clone, Debug)]
pub struct LineEdit {
    text: Vec<char>,
    caret: usize,
    /// Everything selected (after focus or Ctrl+A); the next edit replaces it.
    pub all: bool,
}

impl LineEdit {
    pub fn set(&mut self, s: &str) {
        self.text = s.chars().collect();
        self.caret = self.text.len();
        self.all = true;
    }

    pub fn text(&self) -> String {
        self.text.iter().collect()
    }

    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    /// The caret position in UTF-16 units, for the text layout.
    pub fn caret_utf16(&self) -> usize {
        self.text[..self.caret].iter().map(|c| c.len_utf16()).sum()
    }

    fn take_all(&mut self) {
        if self.all {
            self.text.clear();
            self.caret = 0;
            self.all = false;
        }
    }

    pub fn insert(&mut self, s: &str) {
        self.take_all();
        for c in s.chars().filter(|c| !c.is_control()) {
            self.text.insert(self.caret, c);
            self.caret += 1;
        }
    }

    pub fn backspace(&mut self) {
        if self.all {
            return self.take_all();
        }
        if self.caret > 0 {
            self.caret -= 1;
            self.text.remove(self.caret);
        }
    }

    pub fn delete(&mut self) {
        if self.all {
            return self.take_all();
        }
        if self.caret < self.text.len() {
            self.text.remove(self.caret);
        }
    }

    pub fn left(&mut self) {
        if self.all {
            self.all = false;
            self.caret = 0;
        } else {
            self.caret = self.caret.saturating_sub(1);
        }
    }

    pub fn right(&mut self) {
        self.all = false;
        self.caret = (self.caret + 1).min(self.text.len());
    }

    pub fn home(&mut self) {
        self.all = false;
        self.caret = 0;
    }

    pub fn end(&mut self) {
        self.all = false;
        self.caret = self.text.len();
    }

    pub fn select_all(&mut self) {
        self.all = !self.text.is_empty();
        self.caret = self.text.len();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typing_replaces_the_selection() {
        let mut e = LineEdit::default();
        e.set("C:\\Users");
        e.insert("D:");
        assert_eq!(e.text(), "D:");
        assert_eq!(e.caret_utf16(), 2);
    }

    #[test]
    fn caret_moves_and_edits() {
        let mut e = LineEdit::default();
        e.insert("abc");
        e.left();
        e.backspace();
        assert_eq!(e.text(), "ac");
        e.home();
        e.delete();
        assert_eq!(e.text(), "c");
        e.end();
        e.insert("é😀");
        assert_eq!(e.text(), "cé😀");
        assert_eq!(e.caret_utf16(), 4);
    }

    #[test]
    fn select_all_then_backspace_clears() {
        let mut e = LineEdit::default();
        e.insert("abc");
        e.select_all();
        e.backspace();
        assert!(e.is_empty());
        e.select_all();
        assert!(!e.all);
    }

    #[test]
    fn left_on_a_selection_goes_home() {
        let mut e = LineEdit::default();
        e.set("abc");
        e.left();
        assert_eq!(e.caret_utf16(), 0);
        e.insert("x");
        assert_eq!(e.text(), "xabc");
    }
}
