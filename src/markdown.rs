//! Message text -> styled, wrapped terminal lines, with the position of every link so that a click can find it.
//!
//! Chat conventions: a single newline is a line break, bare URLs become links, headings are shown as bold lines. Control
//! characters are stripped first, so a message can never put an escape sequence on the screen; Markdown is parsed by
//! pulldown-cmark and only ever turned into text and styles, never into terminal escapes.

use pulldown_cmark::{CodeBlockKind, Event, Options, Parser, Tag, TagEnd};
use ratatui::style::{Modifier, Style};
use ratatui::text::Span;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkHit {
    /// Columns `[start, end)` of the link text within the line.
    pub start: usize,
    pub end: usize,
    pub url: String,
}

#[derive(Debug, Clone, Default)]
pub struct Line {
    pub spans: Vec<Span<'static>>,
    pub links: Vec<LinkHit>,
}

impl Line {
    #[cfg(test)]
    pub fn width(&self) -> usize {
        self.spans.iter().map(|s| s.content.width()).sum()
    }

    pub fn text(&self) -> String {
        self.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    pub fn link_at(&self, col: usize) -> Option<&LinkHit> {
        self.links.iter().find(|l| col >= l.start && col < l.end)
    }
}

/// Remove control characters except newline and tab (C0, DEL and C1), which includes ESC and BEL.
pub fn strip_controls(s: &str) -> String {
    s.chars().filter(|&c| c == '\n' || c == '\t' || !(c < ' ' || ('\u{7f}'..='\u{9f}').contains(&c))).collect()
}

fn link_allowed(url: &str) -> bool {
    let u = url.trim().to_ascii_lowercase();
    u.starts_with("http://") || u.starts_with("https://") || u.starts_with("mailto:")
}

/// Wrap bare URLs in `<...>` so they become Markdown autolinks. Code spans and fenced code blocks are left alone, and so is
/// anything already written as a link or an autolink.
fn autolink(text: &str) -> String {
    let mut out = Vec::new();
    let mut fenced = false;
    for line in text.split('\n') {
        if line.trim_start().starts_with("```") {
            fenced = !fenced;
            out.push(line.to_owned());
        } else if fenced {
            out.push(line.to_owned());
        } else {
            let parts: Vec<String> = line.split('`').enumerate().map(|(i, p)| if i % 2 == 0 { link_urls(p) } else { p.to_owned() }).collect();
            out.push(parts.join("`"));
        }
    }
    out.join("\n")
}

fn link_urls(s: &str) -> String {
    let mut out = String::new();
    let mut rest = s;
    let mut before: Option<char> = None;
    while let Some(pos) = find_scheme(rest) {
        let (head, tail) = rest.split_at(pos);
        out.push_str(head);
        let prev = head.chars().next_back().or(before);
        let end = tail.find(|c: char| c.is_whitespace() || matches!(c, '<' | '>' | ')' | ']')).unwrap_or(tail.len());
        let mut url = &tail[..end];
        let trimmed = url.trim_end_matches(['.', ',', ';', ':', '!', '?', '\'', '"']);
        let scheme_len = if url.starts_with("https://") { 8 } else { 7 };
        if trimmed.len() > scheme_len {
            url = trimmed;
        }
        let inside_markdown_link = out.ends_with("](");
        if inside_markdown_link || matches!(prev, Some(']' | '<' | '"' | '\'')) || url.len() <= scheme_len {
            out.push_str(url);
        } else {
            out.push('<');
            out.push_str(url);
            out.push('>');
        }
        before = url.chars().next_back();
        rest = &tail[url.len()..];
    }
    out.push_str(rest);
    out
}

fn find_scheme(s: &str) -> Option<usize> {
    let mut from = 0;
    while let Some(i) = s[from..].find("http") {
        let at = from + i;
        let tail = &s[at..];
        let word_start = s[..at].chars().next_back().map(|c| !c.is_alphanumeric()).unwrap_or(true);
        if word_start && (tail.starts_with("http://") || tail.starts_with("https://")) {
            return Some(at);
        }
        from = at + 4;
    }
    None
}

#[derive(Clone)]
struct Piece {
    text: String,
    style: Style,
    link: Option<String>,
}

enum Item {
    Piece(Piece),
    Break,
}

struct Block {
    first: String,
    rest: String,
    prefix_style: Style,
    items: Vec<Item>,
    blank_before: bool,
    preformatted: bool,
}

struct ListCtx {
    counter: Option<u64>,
    marker_width: usize,
}

struct Builder {
    blocks: Vec<Block>,
    list: Vec<ListCtx>,
    quote: usize,
    bold: usize,
    italic: usize,
    strike: usize,
    heading: bool,
    links: Vec<Option<String>>,
    item_marker: Option<String>,
    cur: Option<usize>,
    first_block_done: bool,
    in_code: bool,
}

const QUOTE: &str = "▎ ";

impl Builder {
    fn style(&self) -> Style {
        let mut s = Style::default();
        if self.bold > 0 || self.heading {
            s = s.add_modifier(Modifier::BOLD);
        }
        if self.italic > 0 {
            s = s.add_modifier(Modifier::ITALIC);
        }
        if self.strike > 0 {
            s = s.add_modifier(Modifier::CROSSED_OUT);
        }
        if self.links.iter().any(Option::is_some) {
            s = s.fg(crate::theme::get().link).add_modifier(Modifier::UNDERLINED);
        }
        s
    }

    /// Start a block with the prefixes for where we are (quote bars, list indentation, the item's bullet).
    fn open_block(&mut self, preformatted: bool) {
        let quote = QUOTE.repeat(self.quote);
        let mut first = quote.clone();
        let mut rest = quote;
        let depth = self.list.len();
        for (i, l) in self.list.iter().enumerate() {
            let pad = " ".repeat(l.marker_width);
            rest.push_str(&pad);
            if i + 1 == depth {
                match self.item_marker.take() {
                    Some(m) => first.push_str(&m),
                    None => first.push_str(&pad),
                }
            } else {
                first.push_str(&pad);
            }
        }
        if preformatted {
            first.push_str("  ");
            rest.push_str("  ");
        }
        self.blocks.push(Block {
            first,
            rest,
            prefix_style: if self.quote > 0 { Style::default().fg(crate::theme::get().faint) } else { Style::default() },
            items: Vec::new(),
            blank_before: self.list.is_empty() && self.first_block_done,
            preformatted,
        });
        self.cur = Some(self.blocks.len() - 1);
        self.first_block_done = true;
    }

    fn block(&mut self) -> &mut Block {
        if self.cur.is_none() {
            self.open_block(false);
        }
        let i = self.cur.expect("a block is open");
        &mut self.blocks[i]
    }

    fn push_text(&mut self, text: &str) {
        let (style, link) = (self.style(), self.links.iter().rev().find_map(|l| l.clone()));
        self.block().items.push(Item::Piece(Piece { text: text.to_owned(), style, link }));
    }

    fn end_block(&mut self) {
        self.cur = None;
    }
}

fn parse(text: &str) -> Vec<Block> {
    let mut b = Builder {
        blocks: Vec::new(),
        list: Vec::new(),
        quote: 0,
        bold: 0,
        italic: 0,
        strike: 0,
        heading: false,
        links: Vec::new(),
        item_marker: None,
        cur: None,
        first_block_done: false,
        in_code: false,
    };
    let options = Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TASKLISTS;
    for event in Parser::new_ext(text, options) {
        match event {
            Event::Start(Tag::Paragraph) => b.open_block(false),
            Event::End(TagEnd::Paragraph) => b.end_block(),
            Event::Start(Tag::Heading { .. }) => {
                b.open_block(false);
                b.heading = true;
            }
            Event::End(TagEnd::Heading(_)) => {
                b.heading = false;
                b.end_block();
            }
            Event::Start(Tag::BlockQuote(_)) => b.quote += 1,
            Event::End(TagEnd::BlockQuote(_)) => {
                b.quote = b.quote.saturating_sub(1);
                b.end_block();
            }
            Event::Start(Tag::List(start)) => {
                b.end_block();
                b.list.push(ListCtx { counter: start, marker_width: 2 });
            }
            Event::End(TagEnd::List(_)) => {
                b.list.pop();
                b.end_block();
            }
            Event::Start(Tag::Item) => {
                b.end_block();
                let marker = match b.list.last_mut() {
                    Some(l) => match l.counter {
                        Some(n) => {
                            l.counter = Some(n + 1);
                            let m = format!("{n}. ");
                            l.marker_width = m.chars().count();
                            m
                        }
                        None => "• ".to_owned(),
                    },
                    None => "• ".to_owned(),
                };
                b.item_marker = Some(marker);
            }
            Event::End(TagEnd::Item) => b.end_block(),
            Event::TaskListMarker(done) => b.push_text(if done { "☑ " } else { "☐ " }),
            Event::Start(Tag::Emphasis) => b.italic += 1,
            Event::End(TagEnd::Emphasis) => b.italic = b.italic.saturating_sub(1),
            Event::Start(Tag::Strong) => b.bold += 1,
            Event::End(TagEnd::Strong) => b.bold = b.bold.saturating_sub(1),
            Event::Start(Tag::Strikethrough) => b.strike += 1,
            Event::End(TagEnd::Strikethrough) => b.strike = b.strike.saturating_sub(1),
            Event::Start(Tag::Link { dest_url, .. }) => b.links.push(link_allowed(&dest_url).then(|| dest_url.to_string())),
            Event::End(TagEnd::Link) => {
                b.links.pop();
            }
            Event::Start(Tag::Image { dest_url, .. }) => {
                b.links.push(link_allowed(&dest_url).then(|| dest_url.to_string()));
                b.push_text("🖼 ");
            }
            Event::End(TagEnd::Image) => {
                b.links.pop();
            }
            Event::Start(Tag::CodeBlock(kind)) => {
                b.end_block();
                b.in_code = true;
                if let CodeBlockKind::Fenced(lang) = kind {
                    let _ = lang;
                }
            }
            Event::End(TagEnd::CodeBlock) => {
                b.in_code = false;
                b.end_block();
            }
            Event::Text(t) if b.in_code => {
                let start = b.first_block_done;
                let _ = start;
                for (i, line) in t.trim_end_matches('\n').split('\n').enumerate() {
                    b.open_block(true);
                    if i > 0 {
                        // code lines directly follow each other
                        let last = b.blocks.len() - 1;
                        b.blocks[last].blank_before = false;
                    }
                    b.block().items.push(Item::Piece(Piece { text: line.to_owned(), style: Style::default().fg(crate::theme::get().code_fg).bg(crate::theme::get().code_bg), link: None }));
                    b.end_block();
                }
            }
            Event::Text(t) => b.push_text(&t),
            Event::Code(t) => {
                let link = b.links.iter().rev().find_map(|l| l.clone());
                b.block().items.push(Item::Piece(Piece { text: t.to_string(), style: Style::default().fg(crate::theme::get().code_fg).bg(crate::theme::get().code_bg), link }));
            }
            Event::SoftBreak | Event::HardBreak => b.block().items.push(Item::Break),
            Event::Html(t) | Event::InlineHtml(t) => b.push_text(&t),
            Event::Rule => {
                b.end_block();
                b.open_block(false);
                b.block().items.push(Item::Piece(Piece { text: "─".repeat(24), style: Style::default().fg(crate::theme::get().faint), link: None }));
                b.end_block();
            }
            _ => {}
        }
    }
    b.blocks
}

/// Accumulates one output line, merging neighbouring text of the same style and recording link columns.
struct LineBuilder {
    line: Line,
    col: usize,
}

impl LineBuilder {
    fn new(prefix: &str, style: Style) -> LineBuilder {
        let mut lb = LineBuilder { line: Line::default(), col: 0 };
        if !prefix.is_empty() {
            lb.push(prefix, style, None);
        }
        lb
    }

    fn push(&mut self, text: &str, style: Style, link: Option<&str>) {
        let w = text.width();
        if let Some(last) = self.line.spans.last_mut() {
            if last.style == style {
                last.content.to_mut().push_str(text);
            } else {
                self.line.spans.push(Span::styled(text.to_owned(), style));
            }
        } else {
            self.line.spans.push(Span::styled(text.to_owned(), style));
        }
        if let Some(url) = link {
            match self.line.links.last_mut() {
                Some(l) if l.url == url && l.end == self.col => l.end += w,
                _ => self.line.links.push(LinkHit { start: self.col, end: self.col + w, url: url.to_owned() }),
            }
        }
        self.col += w;
    }
}

fn wrap_block(block: &Block, width: usize) -> Vec<Line> {
    let prefix_w = block.first.width();
    let avail = width.saturating_sub(prefix_w).max(8);
    let mut lines: Vec<Line> = Vec::new();
    let mut cur = LineBuilder::new(&block.first, block.prefix_style);
    let mut at_start = true; // nothing but the prefix on this line yet
    let finish = |lines: &mut Vec<Line>, cur: &mut LineBuilder, rest: &str| {
        let done = std::mem::replace(cur, LineBuilder::new(rest, block.prefix_style));
        lines.push(done.line);
    };
    for item in &block.items {
        let piece = match item {
            Item::Break => {
                finish(&mut lines, &mut cur, &block.rest);
                at_start = true;
                continue;
            }
            Item::Piece(p) => p,
        };
        if block.preformatted {
            // code: keep spaces, break only where the line is too long
            let mut chunk = String::new();
            let mut w = 0;
            for ch in piece.text.chars() {
                let cw = ch.width().unwrap_or(0);
                if cur.col - prefix_w + w + cw > avail && !(chunk.is_empty() && cur.col == prefix_w) {
                    cur.push(&chunk, piece.style, None);
                    chunk.clear();
                    w = 0;
                    finish(&mut lines, &mut cur, &block.rest);
                }
                chunk.push(ch);
                w += cw;
            }
            cur.push(&chunk, piece.style, None);
            continue;
        }
        for token in tokens(&piece.text) {
            let tw = token.width();
            let is_space = token.chars().all(char::is_whitespace);
            let used = cur.col - prefix_w;
            if is_space {
                if !at_start && used + tw <= avail {
                    cur.push(&token.replace('\t', "    "), piece.style, piece.link.as_deref());
                }
                continue;
            }
            if used + tw > avail && !at_start {
                finish(&mut lines, &mut cur, &block.rest);
            }
            if tw <= avail {
                cur.push(token, piece.style, piece.link.as_deref());
            } else {
                // a word longer than the line: break it anywhere
                let mut chunk = String::new();
                let mut w = 0;
                for ch in token.chars() {
                    let cw = ch.width().unwrap_or(0);
                    if cur.col - prefix_w + w + cw > avail {
                        cur.push(&chunk, piece.style, piece.link.as_deref());
                        chunk.clear();
                        w = 0;
                        finish(&mut lines, &mut cur, &block.rest);
                    }
                    chunk.push(ch);
                    w += cw;
                }
                cur.push(&chunk, piece.style, piece.link.as_deref());
            }
            at_start = false;
        }
    }
    lines.push(cur.line);
    lines
}

/// Split into alternating runs of whitespace and non-whitespace.
fn tokens(s: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0;
    let mut prev: Option<bool> = None;
    for (i, c) in s.char_indices() {
        let space = c.is_whitespace();
        if prev.is_some() && prev != Some(space) {
            out.push(&s[start..i]);
            start = i;
        }
        prev = Some(space);
    }
    if start < s.len() {
        out.push(&s[start..]);
    }
    out
}

/// Render message text for a column of `width` cells.
pub fn render(text: &str, width: usize) -> Vec<Line> {
    let prepared = autolink(&strip_controls(text));
    let blocks = parse(&prepared);
    let mut lines: Vec<Line> = Vec::new();
    for block in &blocks {
        if block.blank_before && !lines.is_empty() {
            lines.push(Line::default());
        }
        lines.extend(wrap_block(block, width));
    }
    while lines.last().map(|l| l.spans.is_empty() || l.text().trim().is_empty()).unwrap_or(false) {
        lines.pop();
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    fn texts(lines: &[Line]) -> Vec<String> {
        lines.iter().map(Line::text).collect()
    }

    fn has_style(lines: &[Line], word: &str, m: Modifier) -> bool {
        lines.iter().flat_map(|l| &l.spans).any(|s| s.content.contains(word) && s.style.add_modifier.contains(m))
    }

    #[test]
    fn bold_italic_strike_and_code() {
        let l = render("**bold** and *it* and ~~gone~~ and `code`", 80);
        assert_eq!(texts(&l), ["bold and it and gone and code"]);
        assert!(has_style(&l, "bold", Modifier::BOLD) && has_style(&l, "it", Modifier::ITALIC) && has_style(&l, "gone", Modifier::CROSSED_OUT));
        assert!(l[0].spans.iter().any(|s| s.content.contains("code") && s.style.bg.is_some()));
    }

    #[test]
    fn single_newlines_are_line_breaks_and_paragraphs_are_separated() {
        assert_eq!(texts(&render("one\ntwo\n\nthree", 80)), ["one", "two", "", "three"]);
    }

    #[test]
    fn headings_are_bold_lines() {
        let l = render("# Title\ntext", 80);
        assert_eq!(texts(&l), ["Title", "", "text"]);
        assert!(has_style(&l, "Title", Modifier::BOLD));
    }

    #[test]
    fn lists_are_numbered_and_indented() {
        let l = render("1. first\n2. second\n   - nested\n3. third\n\n- a\n- b", 80);
        let t = texts(&l);
        assert_eq!(t[0], "1. first");
        assert_eq!(t[1], "2. second");
        assert!(t[2].trim_start().starts_with("• nested") && t[2].starts_with("   "), "{t:?}");
        assert_eq!(t[3], "3. third");
        assert!(t.contains(&"• a".to_string()) && t.contains(&"• b".to_string()));
    }

    #[test]
    fn long_lines_wrap_to_the_width_and_hanging_indent_is_kept() {
        let l = render("- one two three four five six seven eight nine ten eleven twelve", 20);
        assert!(l.len() > 2);
        assert!(l.iter().all(|x| x.width() <= 20), "{:?}", texts(&l));
        assert!(l[1].text().starts_with("  "), "continuation lines align under the text: {:?}", texts(&l));
    }

    #[test]
    fn a_word_longer_than_the_line_is_broken() {
        let l = render(&"x".repeat(50), 20);
        assert!(l.len() >= 3 && l.iter().all(|x| x.width() <= 20));
        assert_eq!(texts(&l).concat(), "x".repeat(50));
    }

    #[test]
    fn links_record_where_they_are() {
        let l = render("see [the docs](https://example.com/docs) now", 80);
        assert_eq!(l[0].text(), "see the docs now");
        assert_eq!(l[0].links, vec![LinkHit { start: 4, end: 12, url: "https://example.com/docs".into() }]);
        assert_eq!(l[0].link_at(5).map(|h| h.url.as_str()), Some("https://example.com/docs"));
        assert!(l[0].link_at(0).is_none() && l[0].link_at(12).is_none());
    }

    #[test]
    fn bare_urls_become_links_without_swallowing_punctuation() {
        let l = render("go to https://example.com/a?b=1, then (https://x.org/y). Done", 80);
        let urls: Vec<&str> = l.iter().flat_map(|x| x.links.iter()).map(|h| h.url.as_str()).collect();
        assert_eq!(urls, ["https://example.com/a?b=1", "https://x.org/y"]);
        assert!(l[0].text().contains("https://example.com/a?b=1,"));
    }

    #[test]
    fn urls_in_code_are_left_alone() {
        let l = render("`https://not-a-link.example` and\n```\nhttps://also-not.example\n```", 80);
        assert!(l.iter().all(|x| x.links.is_empty()));
    }

    #[test]
    fn only_safe_schemes_are_links() {
        let l = render("[x](javascript:alert(1)) [y](file:///etc/passwd) [z](mailto:a@b.c)", 80);
        let urls: Vec<&str> = l.iter().flat_map(|x| x.links.iter()).map(|h| h.url.as_str()).collect();
        assert_eq!(urls, ["mailto:a@b.c"]);
    }

    #[test]
    fn a_wrapped_link_keeps_both_halves_clickable() {
        let l = render("aaaa bbbb [one two three four](https://e.example/x) cccc", 14);
        let linked: Vec<usize> = l.iter().enumerate().filter(|(_, x)| !x.links.is_empty()).map(|(i, _)| i).collect();
        assert!(linked.len() >= 2, "{:?}", texts(&l));
        assert!(l.iter().flat_map(|x| x.links.iter()).all(|h| h.url == "https://e.example/x"));
    }

    #[test]
    fn control_characters_never_reach_the_output() {
        let l = render("hi \u{1b}]8;;http://evil.example\u{7}here\u{1b}[31m red [bold]no markup[/bold]", 200);
        let all = texts(&l).concat();
        assert!(!all.contains('\u{1b}') && !all.contains('\u{7}'));
        assert!(all.contains("[bold]no markup[/bold]"), "markup-like text stays literal");
        assert_eq!(strip_controls("a\u{1b}b\u{9b}c\td\ne"), "abc\td\ne");
    }

    #[test]
    fn quotes_and_code_blocks() {
        let l = render("> quoted line\n\n```\nlet x = 1;\n  indented\n```", 80);
        let t = texts(&l);
        assert!(t[0].starts_with("▎ quoted"), "{t:?}");
        assert!(t.iter().any(|x| x.contains("let x = 1;")) && t.iter().any(|x| x.contains("  indented")));
    }

    #[test]
    fn wide_characters_are_measured_in_cells() {
        let l = render("日本語日本語日本語日本語", 10);
        assert!(l.iter().all(|x| x.width() <= 10));
        assert!(l.len() >= 3);
    }

    #[test]
    fn empty_and_whitespace_only_text_render_nothing() {
        assert!(render("", 40).is_empty());
        assert!(render("   \n  ", 40).is_empty());
    }
}
