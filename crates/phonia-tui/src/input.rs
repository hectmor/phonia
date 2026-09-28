//! A line of text being typed: what it says and where the cursor is.
//!
//! The cursor counts characters, not bytes, so accents and other multi-byte characters are one
//! step each. (It does not know about characters wider than one column, such as CJK: they are
//! stepped over correctly, but the cursor is drawn one column per character.)

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TextInput {
    text: String,
    /// In characters, from 0 to the length of the text.
    cursor: usize,
}

impl TextInput {
    pub fn text(&self) -> &str {
        &self.text
    }

    /// The cursor's place, counted in characters from the start.
    pub fn cursor(&self) -> usize {
        self.cursor
    }

    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    fn len(&self) -> usize {
        self.text.chars().count()
    }

    /// The byte offset of the character at `chars`.
    fn byte_at(&self, chars: usize) -> usize {
        self.text
            .char_indices()
            .nth(chars)
            .map_or(self.text.len(), |(byte, _)| byte)
    }

    pub fn insert(&mut self, c: char) {
        let at = self.byte_at(self.cursor);
        self.text.insert(at, c);
        self.cursor += 1;
    }

    /// Deletes the character before the cursor.
    pub fn backspace(&mut self) {
        if self.cursor == 0 {
            return;
        }
        self.cursor -= 1;
        let at = self.byte_at(self.cursor);
        self.text.remove(at);
    }

    /// Deletes the character under the cursor.
    pub fn delete(&mut self) {
        if self.cursor < self.len() {
            let at = self.byte_at(self.cursor);
            self.text.remove(at);
        }
    }

    pub fn left(&mut self) {
        self.cursor = self.cursor.saturating_sub(1);
    }

    pub fn right(&mut self) {
        self.cursor = (self.cursor + 1).min(self.len());
    }

    pub fn home(&mut self) {
        self.cursor = 0;
    }

    pub fn end(&mut self) {
        self.cursor = self.len();
    }

    /// Deletes the word before the cursor, and the spaces between it and the cursor.
    pub fn delete_word_back(&mut self) {
        let chars: Vec<char> = self.text.chars().collect();
        let mut start = self.cursor;
        while start > 0 && chars[start - 1].is_whitespace() {
            start -= 1;
        }
        while start > 0 && !chars[start - 1].is_whitespace() {
            start -= 1;
        }
        self.remove_range(start, self.cursor);
    }

    /// Deletes everything before the cursor.
    pub fn delete_to_start(&mut self) {
        self.remove_range(0, self.cursor);
    }

    pub fn clear(&mut self) {
        self.text.clear();
        self.cursor = 0;
    }

    fn remove_range(&mut self, from: usize, to: usize) {
        let (from_byte, to_byte) = (self.byte_at(from), self.byte_at(to));
        self.text.replace_range(from_byte..to_byte, "");
        self.cursor = from;
    }

    /// The part of the text that fits in `width` columns with the cursor in it, and the column
    /// the cursor is at within that part. A line longer than the room scrolls to keep the cursor
    /// on screen, showing the end of what is before it.
    pub fn window(&self, width: usize) -> (String, usize) {
        if width == 0 {
            return (String::new(), 0);
        }
        // One column is kept for the cursor itself when it is at the end.
        let start = (self.cursor + 1).saturating_sub(width);
        let shown: String = self.text.chars().skip(start).take(width).collect();
        (shown, self.cursor - start)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn typed(text: &str) -> TextInput {
        let mut input = TextInput::default();
        for c in text.chars() {
            input.insert(c);
        }
        input
    }

    #[test]
    fn typing_inserts_at_the_cursor() {
        let mut input = typed("kon");
        input.left();
        input.insert('r');
        assert_eq!(input.text(), "korn");
        assert_eq!(input.cursor(), 3);
    }

    #[test]
    fn a_multi_byte_character_is_one_step() {
        let mut input = typed("añb");
        assert_eq!(input.cursor(), 3);
        input.left();
        input.left();
        input.delete();
        assert_eq!(input.text(), "ab", "the ñ went, whole");
        input.end();
        input.backspace();
        assert_eq!(input.text(), "a");
        let mut emoji = typed("a🎵b");
        emoji.left();
        emoji.backspace();
        assert_eq!(emoji.text(), "ab");
    }

    #[test]
    fn backspace_and_delete_stop_at_the_ends() {
        let mut input = typed("ab");
        input.home();
        input.backspace();
        assert_eq!(input.text(), "ab", "nothing before the start");
        input.end();
        input.delete();
        assert_eq!(input.text(), "ab", "nothing after the end");
        input.backspace();
        input.backspace();
        input.backspace();
        assert!(input.is_empty() && input.cursor() == 0);
    }

    #[test]
    fn the_cursor_stays_inside_the_text() {
        let mut input = typed("ab");
        input.right();
        assert_eq!(input.cursor(), 2);
        input.home();
        input.left();
        assert_eq!(input.cursor(), 0);
        input.end();
        assert_eq!(input.cursor(), 2);
    }

    #[test]
    fn ctrl_w_deletes_a_word_and_the_spaces_after_it() {
        let mut input = typed("nu metal  ");
        input.delete_word_back();
        assert_eq!(input.text(), "nu ");
        input.delete_word_back();
        assert_eq!(input.text(), "");
        input.delete_word_back();
        assert_eq!(input.text(), "", "nothing to delete is fine");

        let mut middle = typed("korn untouchables");
        for _ in 0.."untouchables".len() {
            middle.left();
        }
        middle.delete_word_back();
        assert_eq!(
            middle.text(),
            "untouchables",
            "only what is before the cursor"
        );
        assert_eq!(middle.cursor(), 0);
    }

    #[test]
    fn ctrl_u_deletes_everything_before_the_cursor() {
        let mut input = typed("korn untouchables");
        for _ in 0.."untouchables".len() {
            input.left();
        }
        input.delete_to_start();
        assert_eq!(input.text(), "untouchables");
        assert_eq!(input.cursor(), 0);
    }

    #[test]
    fn a_line_that_fits_is_shown_whole_and_a_longer_one_scrolls_with_the_cursor() {
        let input = typed("korn");
        assert_eq!(input.window(10), ("korn".to_string(), 4));

        let long = typed("korn untouchables");
        let (shown, column) = long.window(8);
        // The seven characters before the cursor, and the eighth column left for the cursor.
        assert_eq!(shown, "chables");
        assert_eq!(column, 7);

        let mut moved = typed("korn untouchables");
        moved.home();
        assert_eq!(moved.window(8), ("korn unt".to_string(), 0));
        assert_eq!(typed("abc").window(0), (String::new(), 0));
    }
}
