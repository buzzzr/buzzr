//! The message box: a small multi-line text editor (cursor, word movement, kill commands, wrapping for display).

use unicode_width::UnicodeWidthChar;

#[derive(Debug, Default, Clone)]
pub struct Editor {
    chars: Vec<char>,
    /// Cursor position as an index into `chars` (0..=len).
    cur: usize,
}

/// One display row: `chars[start..end]`; `hard_end` is true for the last row of a line (the next character is a newline or the
/// end of the text), where the cursor may sit just past the last character.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Row {
    pub start: usize,
    pub end: usize,
    pub hard_end: bool,
}

fn cw(c: char) -> usize {
    c.width().unwrap_or(0)
}

impl Editor {
    pub fn text(&self) -> String {
        self.chars.iter().collect()
    }

    pub fn is_empty(&self) -> bool {
        self.chars.is_empty()
    }

    pub fn clear(&mut self) {
        self.chars.clear();
        self.cur = 0;
    }

    pub fn insert_char(&mut self, c: char) {
        self.chars.insert(self.cur, c);
        self.cur += 1;
    }

    /// Insert pasted or typed text: `\r\n` and `\r` become `\n`, control characters (except tab and newline) are dropped.
    pub fn insert_str(&mut self, s: &str) {
        let normalised = s.replace("\r\n", "\n").replace('\r', "\n");
        for c in normalised.chars() {
            if c == '\n' || c == '\t' || !c.is_control() {
                self.insert_char(c);
            }
        }
    }

    pub fn backspace(&mut self) {
        if self.cur > 0 {
            self.cur -= 1;
            self.chars.remove(self.cur);
        }
    }

    pub fn delete(&mut self) {
        if self.cur < self.chars.len() {
            self.chars.remove(self.cur);
        }
    }

    pub fn left(&mut self) {
        self.cur = self.cur.saturating_sub(1);
    }

    pub fn right(&mut self) {
        self.cur = (self.cur + 1).min(self.chars.len());
    }

    fn line_start(&self) -> usize {
        self.chars[..self.cur].iter().rposition(|&c| c == '\n').map(|i| i + 1).unwrap_or(0)
    }

    fn line_end(&self) -> usize {
        self.chars[self.cur..].iter().position(|&c| c == '\n').map(|i| self.cur + i).unwrap_or(self.chars.len())
    }

    pub fn home(&mut self) {
        self.cur = self.line_start();
    }

    pub fn end(&mut self) {
        self.cur = self.line_end();
    }

    pub fn word_left(&mut self) {
        while self.cur > 0 && self.chars[self.cur - 1].is_whitespace() {
            self.cur -= 1;
        }
        while self.cur > 0 && !self.chars[self.cur - 1].is_whitespace() {
            self.cur -= 1;
        }
    }

    pub fn word_right(&mut self) {
        let n = self.chars.len();
        while self.cur < n && self.chars[self.cur].is_whitespace() {
            self.cur += 1;
        }
        while self.cur < n && !self.chars[self.cur].is_whitespace() {
            self.cur += 1;
        }
    }

    /// Ctrl+W or Alt+Backspace: delete the word before the cursor.
    pub fn kill_word_back(&mut self) {
        let end = self.cur;
        self.word_left();
        self.chars.drain(self.cur..end);
    }

    /// Ctrl+U: delete from the start of the line to the cursor.
    pub fn kill_to_start(&mut self) {
        let start = self.line_start();
        self.chars.drain(start..self.cur);
        self.cur = start;
    }

    /// Ctrl+K: delete from the cursor to the end of the line (or the newline itself when the line is already at its end).
    pub fn kill_to_end(&mut self) {
        let end = self.line_end();
        if end == self.cur {
            self.delete();
        } else {
            self.chars.drain(self.cur..end);
        }
    }

    /// Break the text into display rows for a box `width` cells wide (characters are wrapped, not words: this is an editor).
    pub fn rows(&self, width: usize) -> Vec<Row> {
        let width = width.max(2);
        let mut rows = Vec::new();
        let mut start = 0;
        loop {
            let line_end = self.chars[start..].iter().position(|&c| c == '\n').map(|i| start + i).unwrap_or(self.chars.len());
            let mut row_start = start;
            let mut w = 0;
            let mut i = start;
            while i < line_end {
                let c = cw(self.chars[i]);
                if w + c > width && i > row_start {
                    rows.push(Row { start: row_start, end: i, hard_end: false });
                    row_start = i;
                    w = 0;
                }
                w += c;
                i += 1;
            }
            rows.push(Row { start: row_start, end: line_end, hard_end: true });
            if line_end >= self.chars.len() {
                break;
            }
            start = line_end + 1;
        }
        rows
    }

    pub fn row_text(&self, row: &Row) -> String {
        self.chars[row.start..row.end].iter().collect()
    }

    /// `(row, column in cells)` of the cursor.
    pub fn cursor_pos(&self, width: usize) -> (usize, usize) {
        let rows = self.rows(width);
        for (i, r) in rows.iter().enumerate() {
            if (self.cur >= r.start && self.cur < r.end) || (self.cur == r.end && r.hard_end) {
                return (i, self.chars[r.start..self.cur].iter().map(|&c| cw(c)).sum());
            }
        }
        let last = rows.len() - 1;
        (last, self.chars[rows[last].start..self.cur.min(self.chars.len())].iter().map(|&c| cw(c)).sum())
    }

    fn move_rows(&mut self, width: usize, delta: isize) -> bool {
        let rows = self.rows(width);
        let (row, col) = self.cursor_pos(width);
        let target = row as isize + delta;
        if target < 0 || target as usize >= rows.len() {
            return false;
        }
        let r = rows[target as usize];
        let mut w = 0;
        let mut i = r.start;
        while i < r.end && w + cw(self.chars[i]) <= col {
            w += cw(self.chars[i]);
            i += 1;
        }
        self.cur = i;
        true
    }

    /// Move up one display row; false when already on the first row.
    pub fn up(&mut self, width: usize) -> bool {
        self.move_rows(width, -1)
    }

    pub fn down(&mut self, width: usize) -> bool {
        self.move_rows(width, 1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ed(s: &str) -> Editor {
        let mut e = Editor::default();
        e.insert_str(s);
        e
    }

    #[test]
    fn typing_editing_and_newlines() {
        let mut e = ed("hello");
        e.insert_char('\n');
        e.insert_str("world");
        assert_eq!(e.text(), "hello\nworld");
        e.home();
        e.backspace(); // joins the lines
        assert_eq!(e.text(), "helloworld");
        e.delete();
        assert_eq!(e.text(), "helloorld");
        e.clear();
        assert!(e.is_empty());
    }

    #[test]
    fn paste_is_normalised() {
        let e = ed("a\r\nb\rc\u{1b}[31m\td");
        assert_eq!(e.text(), "a\nb\nc[31m\td", "line endings unified, escape characters dropped");
    }

    #[test]
    fn word_movement_and_kills() {
        let mut e = ed("one two  three");
        e.word_left();
        assert_eq!(e.cur, 9);
        e.word_left();
        assert_eq!(e.cur, 4);
        e.word_right();
        assert_eq!(e.cur, 7);
        e.kill_word_back();
        assert_eq!(e.text(), "one   three");
        e.end();
        e.kill_to_start();
        assert_eq!(e.text(), "");
        let mut e = ed("abc\ndef");
        e.up(80);
        e.home();
        e.right();
        e.kill_to_end();
        assert_eq!(e.text(), "a\ndef");
        e.kill_to_end(); // at the end of the line: removes the newline
        assert_eq!(e.text(), "adef");
    }

    #[test]
    fn rows_wrap_and_the_cursor_follows() {
        let e = ed("abcdefghij\nxy");
        let rows = e.rows(4);
        let texts: Vec<String> = rows.iter().map(|r| e.row_text(r)).collect();
        assert_eq!(texts, ["abcd", "efgh", "ij", "xy"]);
        assert_eq!(e.cursor_pos(4), (3, 2), "cursor after 'xy'");
        let mut e = e;
        e.cur = 10; // right after 'j'
        assert_eq!(e.cursor_pos(4), (2, 2));
    }

    #[test]
    fn up_and_down_keep_the_column() {
        let mut e = ed("abcdef\nxy\nabcdef");
        assert_eq!(e.cursor_pos(80), (2, 6));
        assert!(e.up(80));
        assert_eq!(e.cursor_pos(80), (1, 2), "short line: column clamps");
        assert!(e.up(80));
        assert_eq!(e.cursor_pos(80), (0, 2));
        assert!(!e.up(80), "already on the first row");
        assert!(e.down(80) && e.down(80));
        assert!(!e.down(80));
    }

    #[test]
    fn wide_characters_take_two_cells() {
        let e = ed("日本語");
        assert_eq!(e.cursor_pos(80), (0, 6));
        let rows = e.rows(4);
        assert_eq!(rows.len(), 2, "two wide characters fit in four cells");
    }

    #[test]
    fn an_empty_editor_has_one_row_and_the_cursor_at_the_start() {
        let e = Editor::default();
        assert_eq!(e.rows(10).len(), 1);
        assert_eq!(e.cursor_pos(10), (0, 0));
    }
}
