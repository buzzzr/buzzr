//! Geometry of the message list: where each message starts, what is on screen, and how the view moves when the content changes.
//!
//! The list is a stack of messages of different heights drawn through a window `view_h` lines tall that starts `scroll` lines from
//! the top. The rules the reader depends on:
//! - new messages never move the view unless it was already at the bottom (then it follows);
//! - older messages loaded above, edits and changed counts keep the same message at the same place on screen;
//! - moving the selection scrolls just enough to show the selected message.

use crate::markdown::Line;

/// One message as drawn: its lines (header, body, reactions), then one blank separator line.
#[derive(Debug, Clone)]
pub struct Rendered {
    pub id: String,
    pub lines: Vec<Line>,
}

impl Rendered {
    pub fn height(&self) -> usize {
        self.lines.len() + 1
    }
}

#[derive(Debug, Default, Clone)]
pub struct Layout {
    pub items: Vec<Rendered>,
    offsets: Vec<usize>,
    pub total: usize,
}

impl Layout {
    pub fn new(items: Vec<Rendered>) -> Layout {
        let mut offsets = Vec::with_capacity(items.len());
        let mut total = 0;
        for it in &items {
            offsets.push(total);
            total += it.height();
        }
        Layout { items, offsets, total }
    }

    pub fn offset(&self, index: usize) -> usize {
        self.offsets.get(index).copied().unwrap_or(self.total)
    }

    pub fn index_of(&self, id: &str) -> Option<usize> {
        self.items.iter().position(|r| r.id == id)
    }

    /// The message that owns a content line, and the line within it (`None` for the separator line).
    pub fn at_line(&self, line: usize) -> Option<(usize, Option<usize>)> {
        if line >= self.total {
            return None;
        }
        let i = match self.offsets.binary_search(&line) {
            Ok(i) => i,
            Err(i) => i - 1,
        };
        let within = line - self.offsets[i];
        Some((i, if within < self.items[i].lines.len() { Some(within) } else { None }))
    }

    pub fn max_scroll(&self, view_h: usize) -> usize {
        self.total.saturating_sub(view_h)
    }

    /// The bottom of the last message as a line number, not counting its trailing separator: where the channel ends.
    /// It is what has to be on screen for the reader to have reached the end.
    pub fn last_bottom(&self) -> usize {
        match self.items.len() {
            0 => 0,
            n => self.offsets[n - 1] + self.items[n - 1].lines.len(),
        }
    }
}

/// Where the reader is, in terms that survive a change of content.
#[derive(Debug, Clone, Default)]
pub struct ListState {
    pub selected: Option<String>,
    pub scroll: usize,
}

/// A snapshot taken before the content changes.
#[derive(Debug, Clone)]
pub struct Anchor {
    /// The message at the top of the view and how many of its lines are scrolled off.
    pub id: Option<String>,
    pub within: usize,
    pub at_bottom: bool,
}

pub fn capture(layout: &Layout, state: &ListState, view_h: usize) -> Anchor {
    let at_bottom = state.scroll >= layout.max_scroll(view_h);
    match layout.at_line(state.scroll) {
        Some((i, _)) => Anchor { id: Some(layout.items[i].id.clone()), within: state.scroll - layout.offset(i), at_bottom },
        None => Anchor { id: None, within: 0, at_bottom },
    }
}

/// Re-apply an anchor to the new layout. `follow` says the reader wanted to stay at the bottom.
pub fn restore(layout: &Layout, state: &mut ListState, view_h: usize, anchor: &Anchor, follow: bool) {
    if follow && anchor.at_bottom {
        state.scroll = layout.max_scroll(view_h);
        return;
    }
    state.scroll = match anchor.id.as_deref().and_then(|id| layout.index_of(id)) {
        Some(i) => layout.offset(i) + anchor.within.min(layout.items[i].height().saturating_sub(1)),
        None => state.scroll,
    };
    state.scroll = state.scroll.min(layout.max_scroll(view_h));
}

/// Index of the selected message, falling back to the last one (or `None` for an empty list).
pub fn selected_index(layout: &Layout, state: &ListState) -> Option<usize> {
    if layout.items.is_empty() {
        return None;
    }
    Some(state.selected.as_deref().and_then(|id| layout.index_of(id)).unwrap_or(layout.items.len() - 1))
}

/// Scroll just enough to show the selected message (its top if it is taller than the view).
pub fn ensure_selected_visible(layout: &Layout, state: &mut ListState, view_h: usize) {
    let Some(i) = selected_index(layout, state) else { return };
    let top = layout.offset(i);
    let bottom = top + layout.items[i].lines.len(); // exclusive; the separator need not be visible
    if top < state.scroll {
        state.scroll = top;
    } else if bottom > state.scroll + view_h {
        state.scroll = if layout.items[i].lines.len() > view_h { top } else { bottom - view_h };
    }
    state.scroll = state.scroll.min(layout.max_scroll(view_h));
}

pub fn select(layout: &Layout, state: &mut ListState, view_h: usize, index: usize) {
    if let Some(it) = layout.items.get(index) {
        state.selected = Some(it.id.clone());
        ensure_selected_visible(layout, state, view_h);
    }
}

pub fn move_selection(layout: &Layout, state: &mut ListState, view_h: usize, delta: isize) {
    let Some(i) = selected_index(layout, state) else { return };
    let j = (i as isize + delta).clamp(0, layout.items.len() as isize - 1) as usize;
    select(layout, state, view_h, j);
}

/// Scroll the view by lines (mouse wheel, Page Up/Down).
pub fn scroll_by(layout: &Layout, state: &mut ListState, view_h: usize, delta: isize) {
    let max = layout.max_scroll(view_h) as isize;
    state.scroll = (state.scroll as isize + delta).clamp(0, max) as usize;
}

/// After scrolling by hand, make sure the selection is on screen: pick the first message that is fully visible.
pub fn select_visible(layout: &Layout, state: &mut ListState, view_h: usize) {
    let Some(i) = selected_index(layout, state) else { return };
    let top = layout.offset(i);
    let bottom = top + layout.items[i].lines.len();
    if top >= state.scroll && bottom <= state.scroll + view_h {
        return;
    }
    let first = (0..layout.items.len()).find(|&j| layout.offset(j) >= state.scroll).unwrap_or(layout.items.len() - 1);
    let last = (0..layout.items.len()).rev().find(|&j| layout.offset(j) + layout.items[j].lines.len() <= state.scroll + view_h).unwrap_or(first);
    state.selected = Some(layout.items[if top < state.scroll { first } else { last.max(first) }].id.clone());
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(id: &str, lines: usize) -> Rendered {
        Rendered { id: id.into(), lines: (0..lines).map(|_| Line::default()).collect() }
    }

    /// Ten messages of 2 lines each (3 with the separator): 30 lines in total.
    fn ten() -> Layout {
        Layout::new((0..10).map(|i| item(&format!("m{i}"), 2)).collect())
    }

    #[test]
    fn offsets_and_hit_testing() {
        let l = ten();
        assert_eq!((l.total, l.offset(3), l.offset(10)), (30, 9, 30));
        assert_eq!(l.at_line(0), Some((0, Some(0))));
        assert_eq!(l.at_line(1), Some((0, Some(1))));
        assert_eq!(l.at_line(2), Some((0, None)), "the separator belongs to no line");
        assert_eq!(l.at_line(9), Some((3, Some(0))));
        assert_eq!(l.at_line(30), None);
        assert_eq!(l.index_of("m4"), Some(4));
    }

    #[test]
    fn the_last_bottom_is_the_end_of_the_newest_message() {
        let l = ten();
        assert_eq!(l.last_bottom(), 29, "m9's body ends at line 29; line 29 is its separator");
        assert_eq!(l.last_bottom(), l.total - 1);
        assert_eq!(Layout::default().last_bottom(), 0, "an empty list has nothing below it");
    }

    #[test]
    fn appending_while_at_the_bottom_follows_but_not_while_scrolled_up() {
        let old = ten();
        let view_h = 10;
        let mut at_bottom = ListState { selected: None, scroll: old.max_scroll(view_h) };
        let a = capture(&old, &at_bottom, view_h);
        assert!(a.at_bottom);
        let mut more: Vec<Rendered> = old.items.clone();
        more.push(item("new", 2));
        let new = Layout::new(more);
        restore(&new, &mut at_bottom, view_h, &a, true);
        assert_eq!(at_bottom.scroll, new.max_scroll(view_h), "following the bottom");

        let mut up = ListState { selected: None, scroll: 6 };
        let a = capture(&old, &up, view_h);
        restore(&new, &mut up, view_h, &a, true);
        assert_eq!(up.scroll, 6, "a reader who scrolled up is left alone");
    }

    #[test]
    fn older_messages_loaded_above_keep_the_same_message_in_place() {
        let old = ten();
        let view_h = 10;
        let mut st = ListState { selected: None, scroll: 7 }; // inside m2 (lines 6..9): one line scrolled off
        let a = capture(&old, &st, view_h);
        assert_eq!((a.id.as_deref(), a.within), (Some("m2"), 1));
        let mut items = vec![item("o1", 3), item("o2", 1)];
        items.extend(old.items.clone());
        let new = Layout::new(items);
        restore(&new, &mut st, view_h, &a, true);
        assert_eq!(st.scroll, 7 + 4 + 2, "shifted by the height of what was added above (4 + 2)");
        assert_eq!(capture(&new, &st, view_h).id.as_deref(), Some("m2"));
    }

    #[test]
    fn selection_scrolls_just_enough() {
        let l = ten();
        let view_h = 10;
        let mut st = ListState { selected: Some("m0".into()), scroll: 0 };
        move_selection(&l, &mut st, view_h, 1);
        assert_eq!((st.selected.as_deref(), st.scroll), (Some("m1"), 0));
        for _ in 0..5 {
            move_selection(&l, &mut st, view_h, 1);
        }
        assert_eq!(st.selected.as_deref(), Some("m6"));
        assert_eq!(st.scroll, 20 - 10, "m6 ends at line 20 (exclusive of its separator)");
        for _ in 0..20 {
            move_selection(&l, &mut st, view_h, -1);
        }
        assert_eq!((st.selected.as_deref(), st.scroll), (Some("m0"), 0), "clamped at the top");
    }

    #[test]
    fn a_message_taller_than_the_view_shows_its_top() {
        let l = Layout::new(vec![item("a", 3), item("big", 20), item("c", 1)]);
        let mut st = ListState { selected: Some("a".into()), scroll: 0 };
        move_selection(&l, &mut st, 8, 1);
        assert_eq!(st.scroll, l.offset(1), "top of the tall message");
    }

    #[test]
    fn manual_scroll_clamps_and_the_selection_follows_the_view() {
        let l = ten();
        let view_h = 10;
        let mut st = ListState { selected: Some("m0".into()), scroll: 0 };
        scroll_by(&l, &mut st, view_h, 100);
        assert_eq!(st.scroll, 20);
        select_visible(&l, &mut st, view_h);
        let sel = l.index_of(st.selected.as_deref().unwrap()).unwrap();
        assert!(l.offset(sel) >= 20, "selection moved onto the screen: m{sel}");
        scroll_by(&l, &mut st, view_h, -100);
        assert_eq!(st.scroll, 0);
    }

    #[test]
    fn a_vanished_selection_falls_back_to_the_last_message() {
        let l = ten();
        let st = ListState { selected: Some("gone".into()), scroll: 0 };
        assert_eq!(selected_index(&l, &st), Some(9));
        assert_eq!(selected_index(&Layout::default(), &st), None);
    }
}
