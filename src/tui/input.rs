use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

/// UTF-8 cursor movement. Never split a code point when editing pasted links.
#[derive(Default, Debug)]
pub(super) struct Input {
    pub text: String,
    pub cursor: usize,
}
impl Input {
    pub fn insert(&mut self, text: &str) {
        let clean: String = text
            .chars()
            .filter(|c| !c.is_control())
            .take(16384)
            .collect();
        if self.text.len() + clean.len() > 65536 {
            return;
        }
        self.text.insert_str(self.cursor, &clean);
        self.cursor += clean.len();
    }
    pub fn key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.text.clear();
                self.cursor = 0;
            }
            KeyCode::Char(c)
                if !key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                self.insert(&c.to_string())
            }
            KeyCode::Left => self.cursor = self.previous(),
            KeyCode::Right => self.cursor = self.next(),
            KeyCode::Home => self.cursor = 0,
            KeyCode::End => self.cursor = self.text.len(),
            KeyCode::Backspace if self.cursor > 0 => {
                let previous = self.previous();
                self.text.drain(previous..self.cursor);
                self.cursor = previous;
            }
            KeyCode::Delete if self.cursor < self.text.len() => {
                self.text.drain(self.cursor..self.next());
            }
            _ => {}
        }
    }
    fn previous(&self) -> usize {
        self.text[..self.cursor]
            .char_indices()
            .next_back()
            .map_or(0, |(index, _)| index)
    }
    fn next(&self) -> usize {
        self.text[self.cursor..]
            .chars()
            .next()
            .map_or(self.cursor, |c| self.cursor + c.len_utf8())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn unicode_editing_and_paste_are_safe() {
        let mut input = Input::default();
        input.insert("a猫é\n\u{1b}");
        assert_eq!(input.text, "a猫é");
        input.key(KeyCode::Left.into());
        input.key(KeyCode::Backspace.into());
        assert_eq!(input.text, "aé");
        input.insert("中");
        input.key(KeyCode::Delete.into());
        assert_eq!(input.text, "a中");
        input.key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL));
        assert_eq!(input.cursor, 0);
        assert!(input.text.is_empty());
    }
}
