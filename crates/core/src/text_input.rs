//! Single-line text edit model for the Start panel search box. Indices are positions in a
//! `Vec<char>` (Unicode scalar values), so an emoji is one step.

pub const MAX_INPUT_CHARS: usize = 256;

#[derive(Debug, Clone, Default)]
pub struct TextInput {
    text: Vec<char>,
    caret: usize,
}

impl TextInput {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn text(&self) -> String {
        self.text.iter().collect()
    }

    pub fn caret(&self) -> usize {
        self.caret
    }

    /// Length in chars.
    pub fn len(&self) -> usize {
        self.text.len()
    }

    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    /// Inserts at the caret. Newlines and tabs become spaces, other control characters are
    /// dropped, and the total length is capped at `MAX_INPUT_CHARS`.
    pub fn insert(&mut self, s: &str) {
        let room = MAX_INPUT_CHARS.saturating_sub(self.text.len());
        let clean: Vec<char> = s
            .chars()
            .filter_map(|c| match c {
                '\n' | '\r' | '\t' => Some(' '),
                c if c.is_control() => None,
                c => Some(c),
            })
            .take(room)
            .collect();
        let n = clean.len();
        self.text.splice(self.caret..self.caret, clean);
        self.caret += n;
    }

    pub fn backspace(&mut self) {
        if self.caret > 0 {
            self.caret -= 1;
            self.text.remove(self.caret);
        }
    }

    pub fn delete(&mut self) {
        if self.caret < self.text.len() {
            self.text.remove(self.caret);
        }
    }

    /// Ctrl+Backspace: deletes back to the start of the previous word.
    pub fn backspace_word(&mut self) {
        let start = self.word_left_pos();
        self.text.drain(start..self.caret);
        self.caret = start;
    }

    pub fn left(&mut self) {
        self.caret = self.caret.saturating_sub(1);
    }

    pub fn right(&mut self) {
        if self.caret < self.text.len() {
            self.caret += 1;
        }
    }

    pub fn word_left(&mut self) {
        self.caret = self.word_left_pos();
    }

    pub fn word_right(&mut self) {
        let n = self.text.len();
        let mut i = self.caret;
        while i < n && !self.text[i].is_whitespace() {
            i += 1;
        }
        while i < n && self.text[i].is_whitespace() {
            i += 1;
        }
        self.caret = i;
    }

    pub fn home(&mut self) {
        self.caret = 0;
    }

    pub fn end(&mut self) {
        self.caret = self.text.len();
    }

    pub fn clear(&mut self) {
        self.text.clear();
        self.caret = 0;
    }

    /// [`Self::clear`] that first overwrites the characters in place (the Wi-Fi password box:
    /// the text does not linger in the freed buffer).
    pub fn wipe(&mut self) {
        self.text.fill('\0');
        self.clear();
    }

    fn word_left_pos(&self) -> usize {
        let mut i = self.caret;
        while i > 0 && self.text[i - 1].is_whitespace() {
            i -= 1;
        }
        while i > 0 && !self.text[i - 1].is_whitespace() {
            i -= 1;
        }
        i
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn wipe_empties_the_input() {
        let mut t = TextInput::new();
        t.insert("segreto");
        t.wipe();
        assert!(t.is_empty());
        assert_eq!((t.caret(), t.len()), (0, 0));
    }
    #[test]
    fn insert_and_caret() {
        let mut t = TextInput::new();
        t.insert("ciao");
        t.left();
        t.insert("X");
        assert_eq!((t.text().as_str(), t.caret()), ("ciaXo", 4));
    }
    #[test]
    fn newlines_become_spaces_and_controls_drop() {
        let mut t = TextInput::new();
        t.insert("a\nb\tc\u{7}");
        assert_eq!(t.text(), "a b c");
    }
    #[test]
    fn length_capped() {
        let mut t = TextInput::new();
        t.insert(&"x".repeat(1000));
        assert_eq!(t.text().chars().count(), MAX_INPUT_CHARS);
    }
    #[test]
    fn emoji_is_one_char_step() {
        let mut t = TextInput::new();
        t.insert("a😀");
        t.backspace();
        assert_eq!(t.text(), "a");
    }
    #[test]
    fn word_ops() {
        let mut t = TextInput::new();
        t.insert("open file now");
        t.backspace_word();
        assert_eq!(t.text(), "open file ");
        t.home();
        t.word_right();
        assert_eq!(t.caret(), 5);
    }
}
