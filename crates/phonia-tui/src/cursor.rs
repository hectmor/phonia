//! A selected row in a list, and the vim motions that move it.
//!
//! It knows nothing of what the list holds: every motion is told how long the list is, so the
//! same cursor serves the sidebar now and the queue and the search results later, and a list that
//! shrinks under it (a track removed by someone else) is handled by [`Cursor::clamp`]. `V`
//! (vim's visual-line mode) anchors a block at the cursor; see [`Cursor::marked`].

use std::ops::RangeInclusive;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Cursor {
    selected: usize,
    /// The first row shown, as last drawn (see `view::keep_in_view`): the view sets it after each
    /// render, the motions below never touch it.
    scroll: usize,
    /// The row `V` was pressed on; `None` outside visual mode.
    anchor: Option<usize>,
}

impl Cursor {
    /// The row selected; 0 for an empty list, where nothing is.
    pub fn selected(&self) -> usize {
        self.selected
    }

    pub fn scroll(&self) -> usize {
        self.scroll
    }

    pub fn set_scroll(&mut self, scroll: usize) {
        self.scroll = scroll;
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

    /// Brings the cursor back inside a list that is now `len` long. An empty list drops visual
    /// mode; otherwise the anchor is pulled back the same way the cursor is.
    pub fn clamp(&mut self, len: usize) {
        self.selected = self.selected.min(len.saturating_sub(1));
        self.anchor = self
            .anchor
            .filter(|_| len > 0)
            .map(|a| a.min(len.saturating_sub(1)));
    }

    /// Starts a block selection anchored at the cursor, or ends one already open (`V` again
    /// leaves, as in vim).
    pub fn toggle_visual(&mut self) {
        self.anchor = match self.anchor {
            Some(_) => None,
            None => Some(self.selected),
        };
    }

    /// Drops the block selection without acting on it (`Esc`).
    pub fn end_visual(&mut self) {
        self.anchor = None;
    }

    /// Whether a block selection is open.
    pub fn visual(&self) -> bool {
        self.anchor.is_some()
    }

    /// The rows a block selection marks, from the anchor to the cursor in either direction; with
    /// no block open this is just the cursor's own row.
    pub fn marked(&self) -> RangeInclusive<usize> {
        let anchor = self.anchor.unwrap_or(self.selected);
        anchor.min(self.selected)..=anchor.max(self.selected)
    }

    /// Moves the cursor and, while a block is open, the anchor with it — so the block moves as a
    /// unit instead of growing or shrinking. `down` moves one row down, otherwise one row up; the
    /// caller checks the bounds first.
    pub fn slide(&mut self, down: bool) {
        fn shift(row: usize, down: bool) -> usize {
            if down { row + 1 } else { row.saturating_sub(1) }
        }
        self.selected = shift(self.selected, down);
        self.anchor = self.anchor.map(|row| shift(row, down));
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

    #[test]
    fn visual_marks_from_the_anchor_to_the_cursor_in_either_direction() {
        let mut cursor = Cursor::default();
        assert!(!cursor.visual());
        assert_eq!(cursor.marked(), 0..=0);

        cursor.down(10);
        cursor.toggle_visual();
        assert!(cursor.visual());
        cursor.down(10);
        cursor.down(10);
        assert_eq!(cursor.marked(), 1..=3);

        cursor.up();
        cursor.up();
        cursor.up();
        assert_eq!(cursor.marked(), 0..=1);

        cursor.toggle_visual();
        assert!(!cursor.visual());
        assert_eq!(cursor.marked(), cursor.selected()..=cursor.selected());
    }

    #[test]
    fn a_shrinking_list_pulls_the_anchor_back_and_an_empty_one_drops_it() {
        let mut cursor = Cursor::default();
        cursor.last(10);
        cursor.toggle_visual();
        cursor.up();
        cursor.up();
        assert_eq!(cursor.marked(), 7..=9);

        cursor.clamp(8);
        assert!(cursor.visual());
        assert_eq!(cursor.marked(), 7..=7);

        cursor.clamp(0);
        assert!(!cursor.visual());
    }

    #[test]
    fn sliding_moves_the_anchor_with_the_cursor() {
        let mut cursor = Cursor::default();
        cursor.down(10);
        cursor.toggle_visual();
        cursor.down(10);
        assert_eq!(cursor.marked(), 1..=2);

        cursor.slide(true);
        assert_eq!(cursor.marked(), 2..=3);

        cursor.slide(false);
        cursor.slide(false);
        assert_eq!(cursor.marked(), 0..=1);
    }

    #[test]
    fn end_visual_drops_the_block_without_moving_the_cursor() {
        let mut cursor = Cursor::default();
        cursor.down(10);
        cursor.toggle_visual();
        cursor.down(10);
        cursor.end_visual();
        assert!(!cursor.visual());
        assert_eq!(cursor.selected(), 2);
        assert_eq!(cursor.marked(), 2..=2);
    }
}
