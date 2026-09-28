//! A list that grows a page at a time as the cursor nears its end.

use crate::cursor::Cursor;
use phonia_ipc::Page;

/// When the cursor is this close to the end of what is loaded, the next page is asked for.
pub const PREFETCH: usize = 10;

/// A list that is loaded a page at a time: the rows loaded so far, how many there are in all, and
/// the cursor. The results of a search, the tracks of an album and an artist's albums all are one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Found<T> {
    pub items: Vec<T>,
    pub total: u64,
    pub cursor: Cursor,
    /// Whether the next page has been asked for and not come yet.
    pub loading: bool,
}

impl<T> Default for Found<T> {
    fn default() -> Self {
        Self {
            items: Vec::new(),
            total: 0,
            cursor: Cursor::default(),
            loading: false,
        }
    }
}

impl<T> Found<T> {
    pub fn from_page(page: Page<T>) -> Self {
        Self {
            items: page.items,
            total: page.total,
            cursor: Cursor::default(),
            loading: false,
        }
    }

    /// If the cursor is near the end of what is loaded and there is more, marks the next page as
    /// asked for and says where it starts. Asking again while it is on its way does nothing.
    pub fn next_offset(&mut self) -> Option<u32> {
        let loaded = self.items.len();
        let near_the_end = self.cursor.selected() + PREFETCH >= loaded;
        if self.loading || !near_the_end || loaded as u64 >= self.total {
            return None;
        }
        self.loading = true;
        Some(loaded as u32)
    }

    /// Adds a page that came. One with nothing in it ends the list, even if the total said more,
    /// so a listing that is short of its total cannot be asked for over and over.
    pub fn append(&mut self, page: Option<Page<T>>) {
        self.loading = false;
        match page {
            Some(page) if !page.items.is_empty() => {
                self.items.extend(page.items);
                self.total = self.total.max(page.total);
            }
            _ => self.total = self.items.len() as u64,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn page(from: u64, count: u64, total: u64) -> Page<u64> {
        Page {
            items: (from..from + count).collect(),
            total,
            offset: from,
        }
    }

    #[test]
    fn a_list_starts_from_its_first_page_with_the_cursor_on_top() {
        let list = Found::from_page(page(0, 50, 123));
        assert_eq!((list.items.len(), list.total), (50, 123));
        assert_eq!(list.cursor.selected(), 0);
        assert!(!list.loading);
        assert!(Found::<u64>::default().items.is_empty());
    }

    #[test]
    fn the_next_page_is_asked_for_only_near_the_end_and_only_once() {
        let mut list = Found::from_page(page(0, 50, 123));
        assert_eq!(list.next_offset(), None, "the cursor is far from the end");
        for _ in 0..(50 - PREFETCH) {
            list.cursor.down(50);
        }
        assert_eq!(list.next_offset(), Some(50), "within ten of the end");
        assert!(list.loading);
        assert_eq!(list.next_offset(), None, "not again while it is on its way");
    }

    #[test]
    fn nothing_more_is_asked_when_everything_is_loaded() {
        let mut list = Found::from_page(page(0, 5, 5));
        list.cursor.last(5);
        assert_eq!(list.next_offset(), None);
    }

    #[test]
    fn a_page_that_comes_is_added_and_the_next_is_asked_for_after_it() {
        let mut list = Found::from_page(page(0, 50, 123));
        list.cursor.last(50);
        assert_eq!(list.next_offset(), Some(50));
        list.append(Some(page(50, 50, 123)));
        assert_eq!(list.items.len(), 100);
        assert!(!list.loading);
        list.cursor.last(100);
        assert_eq!(list.next_offset(), Some(100));
    }

    #[test]
    fn an_empty_page_ends_the_list_even_if_the_total_said_more() {
        let mut list = Found::from_page(page(0, 50, 123));
        list.cursor.last(50);
        list.next_offset();
        list.append(Some(page(50, 0, 123)));
        assert_eq!(list.total, 50);
        assert_eq!(list.next_offset(), None, "no asking for ever");
        // A page that did not come at all ends it the same way.
        let mut lost = Found::from_page(page(0, 50, 123));
        lost.cursor.last(50);
        lost.next_offset();
        lost.append(None);
        assert_eq!(lost.total, 50);
    }
}
