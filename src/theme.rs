//! Colours. Pure ANSI "dark grey" (bright black) is close to invisible on many dark themes, so secondary text uses an explicit
//! mid-grey that stays readable on black and on dark grey. `BUZZR_THEME=light` selects darker greys for a light background.

use ratatui::style::Color;
use std::sync::OnceLock;

pub struct Theme {
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
}

pub fn get() -> &'static Theme {
    static THEME: OnceLock<Theme> = OnceLock::new();
    THEME.get_or_init(|| {
        if crate::config::env("THEME").map(|v| v.eq_ignore_ascii_case("light")).unwrap_or(false) {
            Theme {
                dim: Color::Rgb(85, 85, 98),
                faint: Color::Rgb(160, 160, 172),
                sel_bg: Color::Rgb(226, 226, 236),
                bar_on: Color::Rgb(50, 50, 62),
                bar_off: Color::Rgb(140, 140, 152),
                code_fg: Color::Rgb(120, 70, 0),
                code_bg: Color::Rgb(238, 238, 238),
                time: Color::Rgb(70, 70, 84),
            }
        } else {
            Theme {
                dim: Color::Rgb(184, 184, 198),
                faint: Color::Rgb(110, 110, 124),
                sel_bg: Color::Rgb(46, 46, 56),
                bar_on: Color::Rgb(215, 215, 225),
                bar_off: Color::Rgb(125, 125, 138),
                code_fg: Color::Rgb(230, 200, 120),
                code_bg: Color::Rgb(40, 40, 40),
                time: Color::Rgb(206, 206, 220),
            }
        }
    })
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

    #[test]
    fn secondary_text_is_readable_on_dark_backgrounds() {
        let t = get();
        let backgrounds = [Color::Rgb(0, 0, 0), Color::Rgb(30, 30, 30), t.sel_bg, Color::Rgb(40, 44, 52)];
        for bg in backgrounds {
            for (name, fg) in [("dim", t.dim), ("time", t.time)] {
                assert!(contrast(fg, bg) >= 6.0, "{name} on {bg:?} has contrast {:.1}", contrast(fg, bg));
            }
        }
        // dividers and the scrollbar sit on the normal background, not on the selection tint
        for bg in [Color::Rgb(0, 0, 0), Color::Rgb(30, 30, 30), Color::Rgb(40, 44, 52)] {
            assert!(contrast(t.faint, bg) >= 2.5, "faint lines must still be visible on {bg:?}");
        }
        // plain ANSI dark grey (xterm: 85,85,85; many themes much darker) would not pass
        assert!(contrast(Color::Rgb(85, 85, 85), Color::Rgb(30, 30, 30)) < 3.0);
    }
}
