//! Colours. The default scheme, `buzz-dark`, uses the dark palette of the Buzz desktop app (`src/shared/design-system/styles/tokens.css`
//! in the desktop app's repo: neutral ramp, citrus accent) and paints its own background.
//!
//! `BUZZR_THEME` picks another one: `buzz-light` is the desktop app's light palette, `high-contrast` is black-on-white picked for
//! at least WCAG AAA (7:1) against its own backgrounds, `dark` keeps the terminal's own background (and ANSI accents) and `light`
//! is for a light background. Pure ANSI "dark grey" (bright black) is close to invisible on many dark themes, so secondary text
//! uses an explicit mid-grey that stays readable on black and on dark grey.
//!
//! Colour is never the only signal: the selection has its bar glyph, unread has its count, the connection state has its word. So
//! when `NO_COLOR` is set (any non-empty value, per <https://no-color.org>) or `TERM=dumb`, every colour becomes the terminal
//! default and the screen still reads.

use ratatui::style::Color;
use std::sync::OnceLock;

pub struct Theme {
    /// Background painted under everything. `None` leaves the terminal's own background alone.
    pub bg: Option<Color>,
    /// Default text colour. `None` leaves the terminal's own foreground alone.
    pub fg: Option<Color>,
    /// Background of popups (help, search, panels). Falls back to `bg`.
    pub popup_bg: Option<Color>,
    /// Secondary text: message numbers and times, hints, the status line, the footer descriptions.
    pub dim: Color,
    /// Lines and decorations: dividers, scrollbar, quote bars.
    pub faint: Color,
    pub sel_bg: Color,
    pub bar_on: Color,
    pub bar_off: Color,
    pub code_fg: Color,
    pub code_bg: Color,
    /// Message header parts that should be easy to read: the time.
    pub time: Color,
    /// Borders of the focused panel, other people's names, panel headings.
    pub accent: Color,
    /// Links in messages.
    pub link: Color,
    /// Unread counts, hints, warnings.
    pub warn: Color,
    /// Own name, the "live" indicator.
    pub ok: Color,
    pub err: Color,
}


/// The scheme with this name (case-insensitive); anything unknown gives the default.
pub fn by_name(name: &str) -> Theme {
    if name.eq_ignore_ascii_case("buzz-light") {
        buzz_light()
    } else if name.eq_ignore_ascii_case("high-contrast") {
        high_contrast()
    } else if name.eq_ignore_ascii_case("light") {
        light()
    } else if name.eq_ignore_ascii_case("dark") {
        dark()
    } else {
        buzz_dark()
    }
}

/// The scheme to draw with: `BUZZR_THEME` picks it, but `NO_COLOR` (any non-empty value, per no-color.org) or `TERM=dumb` beats
/// it and gives the colourless scheme.
pub fn select(theme: Option<&str>, no_color: Option<&str>, term: Option<&str>) -> Theme {
    let colourless = no_color.is_some_and(|v| !v.is_empty()) || term.is_some_and(|t| t.eq_ignore_ascii_case("dumb"));
    if colourless {
        plain()
    } else {
        by_name(theme.unwrap_or_default())
    }
}

pub fn get() -> &'static Theme {
    static THEME: OnceLock<Theme> = OnceLock::new();
    THEME.get_or_init(resolve)
}

/// What this process draws with. The unit tests pin the scheme from `BUZZR_THEME` alone, so a runner that sets `NO_COLOR` or
/// `TERM=dumb` cannot change what they render; `select` covers the colourless rules directly.
fn resolve() -> Theme {
    let theme = crate::config::env("THEME");
    #[cfg(test)]
    {
        by_name(theme.as_deref().unwrap_or_default())
    }
    #[cfg(not(test))]
    {
        let no_color = std::env::var("NO_COLOR").ok();
        let term = std::env::var("TERM").ok();
        select(theme.as_deref(), no_color.as_deref(), term.as_deref())
    }
}

/// No colour at all: for `NO_COLOR` and for a dumb terminal. Everything falls back to the terminal's own colours, and the screen
/// still reads on structure alone: the selection bar and the gutter glyph, `bold` on the open channel, the unread count in the
/// badge, `● live` in the header, the borders of a popup.
fn plain() -> Theme {
    Theme {
        bg: None,
        fg: None,
        popup_bg: None,
        dim: Color::Reset,
        faint: Color::Reset,
        sel_bg: Color::Reset,
        bar_on: Color::Reset,
        bar_off: Color::Reset,
        code_fg: Color::Reset,
        code_bg: Color::Reset,
        time: Color::Reset,
        accent: Color::Reset,
        link: Color::Reset,
        warn: Color::Reset,
        ok: Color::Reset,
        err: Color::Reset,
    }
}

/// High contrast: black background, white text, and every other colour picked to reach at least 7:1 against the backgrounds it is
/// drawn on (WCAG AAA for body text), for low vision and for washed-out or glare-lit screens. The selection still has the bar.
fn high_contrast() -> Theme {
    Theme {
        bg: Some(Color::Rgb(0, 0, 0)),
        fg: Some(Color::Rgb(0xff, 0xff, 0xff)),
        popup_bg: Some(Color::Rgb(0, 0, 0)),
        dim: Color::Rgb(0xd0, 0xd0, 0xd0),
        faint: Color::Rgb(0x9e, 0x9e, 0x9e),
        sel_bg: Color::Rgb(0x26, 0x26, 0x26),
        bar_on: Color::Rgb(0xff, 0xff, 0x00),
        bar_off: Color::Rgb(0x9e, 0x9e, 0x9e),
        code_fg: Color::Rgb(0xff, 0xff, 0x00),
        code_bg: Color::Rgb(0x1a, 0x1a, 0x1a),
        time: Color::Rgb(0xd0, 0xd0, 0xd0),
        accent: Color::Rgb(0x6f, 0xd8, 0xff),
        link: Color::Rgb(0x6f, 0xd8, 0xff),
        warn: Color::Rgb(0xff, 0xd6, 0x0a),
        ok: Color::Rgb(0x3d, 0xd6, 0x8c),
        err: Color::Rgb(0xff, 0x95, 0x92),
    }
}

/// The dark palette of the Buzz desktop app. Hex values are from its tokens: panel = neutral-3, sidebar/base = neutral-1,
/// selected = neutral-5, text = neutral-12/11/10, accent = citrus-9, status colours = green-11, amber-10, red-11.
fn buzz_dark() -> Theme {
    Theme {
        bg: Some(Color::Rgb(0x1a, 0x1a, 0x1a)),
        fg: Some(Color::Rgb(0xff, 0xff, 0xff)),
        popup_bg: Some(Color::Rgb(0x28, 0x28, 0x28)),
        dim: Color::Rgb(0xc7, 0xc7, 0xc7),
        faint: Color::Rgb(0x73, 0x73, 0x73),
        sel_bg: Color::Rgb(0x33, 0x33, 0x33),
        bar_on: Color::Rgb(0xbe, 0xda, 0x36),
        bar_off: Color::Rgb(0x80, 0x80, 0x80),
        code_fg: Color::Rgb(0xce, 0xe2, 0x8b),
        code_bg: Color::Rgb(0x2e, 0x2e, 0x2e),
        time: Color::Rgb(0xa4, 0xa4, 0xa4),
        accent: Color::Rgb(0xbe, 0xda, 0x36),
        link: Color::Rgb(0xcb, 0xe6, 0x48),
        warn: Color::Rgb(0xff, 0xd6, 0x0a),
        ok: Color::Rgb(0x3d, 0xd6, 0x8c),
        err: Color::Rgb(0xff, 0x95, 0x92),
    }
}

/// The light palette of the Buzz desktop app: white panel, neutral-3 selection, near-black text. The citrus accent is too pale to read
/// as text on white, so text-sized accents use the desktop's darker citrus-11 and link colours.
/// The warning, ok and error colours are a little darker than the desktop's amber/green/red-11 so they stay readable on the selected-row grey.
fn buzz_light() -> Theme {
    Theme {
        bg: Some(Color::Rgb(0xff, 0xff, 0xff)),
        fg: Some(Color::Rgb(0x0f, 0x0f, 0x0f)),
        popup_bg: Some(Color::Rgb(0xff, 0xff, 0xff)),
        dim: Color::Rgb(0x52, 0x52, 0x52),
        faint: Color::Rgb(0x95, 0x95, 0x95),
        sel_bg: Color::Rgb(0xe8, 0xe8, 0xe8),
        bar_on: Color::Rgb(0x48, 0x53, 0x11),
        bar_off: Color::Rgb(0x80, 0x80, 0x80),
        code_fg: Color::Rgb(0x2b, 0x31, 0x10),
        code_bg: Color::Rgb(0xf5, 0xf5, 0xf6),
        time: Color::Rgb(0x5f, 0x5f, 0x5f),
        accent: Color::Rgb(0x48, 0x53, 0x11),
        link: Color::Rgb(0x48, 0x54, 0x00),
        warn: Color::Rgb(0x96, 0x58, 0x00),
        ok: Color::Rgb(0x1a, 0x70, 0x48),
        err: Color::Rgb(0xbf, 0x22, 0x27),
    }
}

/// The previous default: the terminal's own background with ANSI accents.
fn dark() -> Theme {
    Theme {
        bg: None,
        fg: None,
        popup_bg: None,
        dim: Color::Rgb(184, 184, 198),
        faint: Color::Rgb(110, 110, 124),
        sel_bg: Color::Rgb(46, 46, 56),
        bar_on: Color::Rgb(215, 215, 225),
        bar_off: Color::Rgb(125, 125, 138),
        code_fg: Color::Rgb(230, 200, 120),
        code_bg: Color::Rgb(40, 40, 40),
        time: Color::Rgb(206, 206, 220),
        accent: Color::Cyan,
        link: Color::Cyan,
        warn: Color::Yellow,
        ok: Color::Green,
        err: Color::Red,
    }
}

fn light() -> Theme {
    Theme {
        bg: None,
        fg: None,
        popup_bg: None,
        dim: Color::Rgb(85, 85, 98),
        faint: Color::Rgb(160, 160, 172),
        sel_bg: Color::Rgb(226, 226, 236),
        bar_on: Color::Rgb(50, 50, 62),
        bar_off: Color::Rgb(140, 140, 152),
        code_fg: Color::Rgb(120, 70, 0),
        code_bg: Color::Rgb(238, 238, 238),
        time: Color::Rgb(70, 70, 84),
        accent: Color::Cyan,
        link: Color::Cyan,
        warn: Color::Yellow,
        ok: Color::Green,
        err: Color::Red,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Relative luminance of an sRGB colour (WCAG), to check that text stays readable against the backgrounds we expect.
    fn luminance(c: Color) -> f64 {
        let Color::Rgb(r, g, b) = c else { return 0.0 };
        let f = |v: u8| {
            let s = v as f64 / 255.0;
            if s <= 0.03928 { s / 12.92 } else { ((s + 0.055) / 1.055).powf(2.4) }
        };
        0.2126 * f(r) + 0.7152 * f(g) + 0.0722 * f(b)
    }

    fn contrast(a: Color, b: Color) -> f64 {
        let (x, y) = (luminance(a), luminance(b));
        (x.max(y) + 0.05) / (x.min(y) + 0.05)
    }

    fn rgb(c: Option<Color>) -> Color {
        c.expect("scheme paints a background")
    }

    #[test]
    fn secondary_text_is_readable_on_dark_backgrounds() {
        // the desktop palette uses its metadata grey for times, which is a little softer than the old terminal-relative default
        for (t, min) in [(dark(), 6.0), (buzz_dark(), 4.5)] {
            let backgrounds = [Color::Rgb(0, 0, 0), Color::Rgb(30, 30, 30), t.sel_bg, Color::Rgb(40, 44, 52)];
            for bg in backgrounds {
                for (name, fg) in [("dim", t.dim), ("time", t.time)] {
                    assert!(contrast(fg, bg) >= min, "{name} on {bg:?} has contrast {:.1}", contrast(fg, bg));
                }
            }
            // dividers and the scrollbar sit on the normal background, not on the selection tint
            for bg in [Color::Rgb(0, 0, 0), Color::Rgb(30, 30, 30), Color::Rgb(40, 44, 52)] {
                assert!(contrast(t.faint, bg) >= 2.5, "faint lines must still be visible on {bg:?}");
            }
        }
        // plain ANSI dark grey (xterm: 85,85,85; many themes much darker) would not pass
        assert!(contrast(Color::Rgb(85, 85, 85), Color::Rgb(30, 30, 30)) < 3.0);
    }

    #[test]
    fn buzz_dark_is_the_default_and_matches_the_desktop_palette() {
        for name in ["", "buzz-dark", "BUZZ-DARK", "nonsense"] {
            assert_eq!(by_name(name).bg, Some(Color::Rgb(0x1a, 0x1a, 0x1a)), "{name:?}");
        }
        let t = buzz_dark();
        assert_eq!(t.fg, Some(Color::Rgb(255, 255, 255)));
        assert_eq!(t.sel_bg, Color::Rgb(0x33, 0x33, 0x33));
        assert_eq!(t.accent, Color::Rgb(0xbe, 0xda, 0x36));
        assert!(by_name("dark").bg.is_none() && by_name("light").bg.is_none(), "the other schemes keep the terminal background");
    }

    #[test]
    fn no_color_or_a_dumb_terminal_turns_colour_off_whatever_the_scheme_says() {
        let t = select(Some("buzz-dark"), Some("1"), Some("xterm-256color"));
        assert!(t.bg.is_none() && t.accent == Color::Reset, "NO_COLOR wins over BUZZR_THEME");
        assert_eq!(select(Some("buzz-dark"), Some(""), None).bg, Some(Color::Rgb(0x1a, 0x1a, 0x1a)), "an empty NO_COLOR is not a request");
        assert_eq!(select(Some("buzz-dark"), None, Some("DUMB")).accent, Color::Reset, "TERM=dumb, case-insensitively");
        assert_eq!(select(Some("high-contrast"), None, Some("xterm-256color")).bg, Some(Color::Rgb(0, 0, 0)), "and the scheme is back when neither is set");
        assert_eq!(select(None, None, None).bg, Some(Color::Rgb(0x1a, 0x1a, 0x1a)), "no scheme and no colourless request: the default");
    }

    #[test]
    fn the_plain_scheme_uses_no_colour_at_all() {
        let t = plain();
        assert!(t.bg.is_none() && t.fg.is_none() && t.popup_bg.is_none(), "no background is painted");
        for (name, c) in [
            ("dim", t.dim),
            ("faint", t.faint),
            ("sel_bg", t.sel_bg),
            ("bar_on", t.bar_on),
            ("bar_off", t.bar_off),
            ("code_fg", t.code_fg),
            ("code_bg", t.code_bg),
            ("time", t.time),
            ("accent", t.accent),
            ("link", t.link),
            ("warn", t.warn),
            ("ok", t.ok),
            ("err", t.err),
        ] {
            assert_eq!(c, Color::Reset, "{name}");
        }
    }

    #[test]
    fn the_high_contrast_scheme_reaches_wcag_aaa() {
        let t = high_contrast();
        assert_eq!(by_name("High-Contrast").bg, t.bg);
        let (panel, popup) = (rgb(t.bg), rgb(t.popup_bg));
        for bg in [panel, popup, t.sel_bg, t.code_bg] {
            for (name, fg) in [("text", rgb(t.fg)), ("dim", t.dim), ("time", t.time), ("accent", t.accent), ("link", t.link), ("warn", t.warn), ("ok", t.ok), ("err", t.err), ("code", t.code_fg)] {
                assert!(contrast(fg, bg) >= 7.0, "{name} on {bg:?} has contrast {:.1}, below AAA", contrast(fg, bg));
            }
        }
        for bg in [panel, popup] {
            assert!(contrast(t.faint, bg) >= 4.5, "faint lines on {bg:?}");
        }
    }

    #[test]
    fn buzz_light_colours_are_readable_on_its_own_backgrounds() {
        let t = buzz_light();
        assert_eq!(by_name("Buzz-Light").bg, t.bg);
        let (panel, popup) = (rgb(t.bg), rgb(t.popup_bg));
        for bg in [panel, popup, t.sel_bg, t.code_bg] {
            for (name, fg) in [("text", rgb(t.fg)), ("dim", t.dim), ("time", t.time), ("accent", t.accent), ("link", t.link), ("warn", t.warn), ("ok", t.ok), ("err", t.err), ("code", t.code_fg)] {
                assert!(contrast(fg, bg) >= 4.5, "{name} on {bg:?} has contrast {:.1}", contrast(fg, bg));
            }
        }
        for bg in [panel, popup] {
            assert!(contrast(t.faint, bg) >= 2.5, "faint lines on {bg:?}");
        }
    }

    #[test]
    fn buzz_dark_colours_are_readable_on_its_own_backgrounds() {
        let t = buzz_dark();
        let (panel, popup) = (rgb(t.bg), rgb(t.popup_bg));
        for bg in [panel, popup, t.sel_bg, t.code_bg] {
            for (name, fg) in [("text", rgb(t.fg)), ("dim", t.dim), ("time", t.time), ("accent", t.accent), ("link", t.link), ("warn", t.warn), ("ok", t.ok), ("err", t.err), ("code", t.code_fg)] {
                assert!(contrast(fg, bg) >= 4.5, "{name} on {bg:?} has contrast {:.1}", contrast(fg, bg));
            }
        }
        for bg in [panel, popup] {
            assert!(contrast(t.faint, bg) >= 2.5, "faint lines on {bg:?}");
        }
    }
}
