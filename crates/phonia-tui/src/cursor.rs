//! A selected row in a list, and the vim motions that move it.
//!
//! It knows nothing of what the list holds: every motion is told how long the list is, so the
//! same cursor serves the sidebar now and the queue and the search results later, and a list that
//! shrinks under it (a track removed by someone else) is handled by [`Cursor::clamp`].

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Cursor {
    selected: usize,
}

impl Cursor {
    /// The row selected; 0 for an empty list, where nothing is.
    pub fn selected(&self) -> usize {
        self.selected
    }

    pub fn down(&mut self, len: usize) {
        self.selected = (self.selected + 1).min(len.saturating_sub(1));
    }

    pub fn up(&mut self) {
        self.selected = self.selected.saturating_sub(1);
    }

    pub fn first(&mut self) {
        self.selected = 0;
    }

    pub fn last(&mut self, len: usize) {
        self.selected = len.saturating_sub(1);
    }

    /// Moves `rows` down, stopping at the last one.
    pub fn page_down(&mut self, len: usize, rows: usize) {
        self.selected = (self.selected + rows).min(len.saturating_sub(1));
    }

    pub fn page_up(&mut self, rows: usize) {
        self.selected = self.selected.saturating_sub(rows);
    }

    pub fn select(&mut self, index: usize, len: usize) {
        self.selected = index.min(len.saturating_sub(1));
    }

    /// Brings the cursor back inside a list that is now `len` long.
    pub fn clamp(&mut self, len: usize) {
        self.selected = self.selected.min(len.saturating_sub(1));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn it_moves_one_row_at_a_time_and_stops_at_the_ends() {
        let mut cursor = Cursor::default();
        cursor.up();
        assert_eq!(cursor.selected(), 0);
        for _ in 0..10 {
            cursor.down(3);
        }
        assert_eq!(cursor.selected(), 2);
        cursor.up();
        assert_eq!(cursor.selected(), 1);
    }

    #[test]
    fn first_and_last_jump_to_the_ends() {
        let mut cursor = Cursor::default();
        cursor.last(50);
        assert_eq!(cursor.selected(), 49);
        cursor.first();
        assert_eq!(cursor.selected(), 0);
    }

    #[test]
    fn a_page_moves_that_many_rows_without_passing_the_ends() {
        let mut cursor = Cursor::default();
        cursor.page_down(100, 10);
        assert_eq!(cursor.selected(), 10);
        cursor.page_down(15, 10);
        assert_eq!(cursor.selected(), 14);
        cursor.page_up(10);
        assert_eq!(cursor.selected(), 4);
        cursor.page_up(10);
        assert_eq!(cursor.selected(), 0);
    }

    #[test]
    fn an_empty_list_keeps_the_cursor_at_zero() {
        let mut cursor = Cursor::default();
        cursor.down(0);
        cursor.last(0);
        cursor.page_down(0, 10);
        cursor.select(5, 0);
        assert_eq!(cursor.selected(), 0);
    }

    #[test]
    fn a_list_that_shrinks_pulls_the_cursor_back() {
        let mut cursor = Cursor::default();
        cursor.last(10);
        cursor.clamp(4);
        assert_eq!(cursor.selected(), 3);
        cursor.clamp(0);
        assert_eq!(cursor.selected(), 0);
    }
}
