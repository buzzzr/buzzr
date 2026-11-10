//! The emoji the reaction picker offers, and the search over their names. A fixed table: no network, no unicode data, nothing
//! to keep in sync with the system — the picker is for the common reactions, and anything missing can still be typed (the
//! picker sends what you typed when no name matches).

/// `(glyph, name)`: the glyph sent in the `kind:7`, and the lower-case words the picker searches.
pub const EMOJI: &[(&str, &str)] = &[
    ("👍", "thumbs up yes like approve +1"),
    ("👎", "thumbs down no dislike -1"),
    ("❤️", "heart love red"),
    ("😂", "joy laugh lol tears"),
    ("😍", "heart eyes love"),
    ("🎉", "party popper tada celebrate congrats"),
    ("🔥", "fire hot lit"),
    ("🚀", "rocket ship launch ship it"),
    ("👀", "eyes look watching"),
    ("🙏", "pray thanks please namaste"),
    ("👏", "clap applause bravo"),
    ("🤔", "thinking hmm question"),
    ("🤯", "exploding head mind blown wow"),
    ("😅", "sweat smile nervous phew"),
    ("😭", "crying sob sad"),
    ("😞", "disappointed sad"),
    ("😡", "angry mad rage"),
    ("🫠", "melting face"),
    ("🤝", "handshake deal agreed"),
    ("💪", "muscle strong flex"),
    ("🤞", "crossed fingers luck hope"),
    ("✌️", "victory peace"),
    ("👋", "wave hello hi bye"),
    ("🙌", "raised hands hooray yay"),
    ("💯", "hundred 100 perfect"),
    ("✅", "check mark done ok yes"),
    ("☑️", "ballot check done"),
    ("❌", "cross mark no wrong"),
    ("⚠️", "warning careful"),
    ("🐛", "bug issue defect"),
    ("🔧", "wrench fix tool"),
    ("🔨", "hammer build"),
    ("📌", "pin pinned"),
    ("📝", "memo note write"),
    ("📈", "chart up growth"),
    ("📉", "chart down drop"),
    ("💡", "bulb idea light"),
    ("🧠", "brain smart"),
    ("🧪", "test tube experiment"),
    ("⚡", "zap lightning fast"),
    ("✨", "sparkles shiny new"),
    ("🌟", "star glow"),
    ("⭐", "star favourite favorite"),
    ("🏆", "trophy win award"),
    ("🎯", "target bullseye"),
    ("🥇", "gold medal first"),
    ("📦", "package box release"),
    ("🚢", "ship deploy"),
    ("🧹", "broom clean tidy"),
    ("🗑️", "wastebasket trash delete"),
    ("⏰", "alarm clock time"),
    ("⏳", "hourglass waiting"),
    ("🕒", "clock time"),
    ("📅", "calendar date"),
    ("💬", "speech balloon comment talk"),
    ("🔗", "link url"),
    ("🔒", "lock private secret"),
    ("🔓", "unlock open"),
    ("🔑", "key auth"),
    ("🛡️", "shield secure"),
    ("🔍", "magnifier search find"),
    ("📣", "megaphone announce"),
    ("📢", "loudspeaker announce"),
    ("☕", "coffee break"),
    ("🍕", "pizza food"),
    ("🎂", "cake birthday"),
    ("🍺", "beer cheers"),
    ("🌧️", "rain sad"),
    ("☀️", "sun nice"),
    ("🌙", "moon night"),
    ("❄️", "snowflake cold"),
    ("🌈", "rainbow colours colors"),
    ("🌊", "wave water ocean"),
    ("🌱", "seedling grow new"),
    ("♻️", "recycle reuse"),
    ("🚧", "construction wip work in progress"),
    ("🧵", "thread yarn"),
    ("📚", "books docs documentation"),
    ("🧾", "receipt log"),
    ("🔬", "microscope research"),
    ("🛠️", "tools build repair"),
    ("🎨", "art palette design"),
    ("🖼️", "picture image"),
    ("🎵", "music note song"),
    ("🎮", "game controller"),
    ("💤", "sleep zzz tired"),
    ("🤖", "robot bot agent"),
    ("👻", "ghost spooky"),
    ("💩", "poop crap"),
    ("🫡", "salute yes sir"),
    ("🤷", "shrug dunno"),
    ("🙃", "upside down smile"),
    ("🥳", "partying face celebrate"),
    ("😴", "sleeping tired"),
    ("🤮", "vomit sick"),
    ("🆗", "ok okay"),
    ("🆕", "new"),
    ("🆘", "sos help"),
];

/// The entries whose name contains every word of `query` (lower-cased, in table order). An empty query keeps the whole table.
pub fn search(query: &str) -> Vec<(&'static str, &'static str)> {
    let words: Vec<String> = query.to_lowercase().split_whitespace().map(|w| w.to_owned()).collect();
    EMOJI
        .iter()
        .copied()
        .filter(|(_, name)| words.iter().all(|w| name.contains(w.as_str())))
        .collect()
}

/// The table entry for a glyph, when the table has it (used to name what the picker is about to send).
pub fn name_of(glyph: &str) -> Option<&'static str> {
    EMOJI.iter().find(|(e, _)| *e == glyph).map(|(_, name)| *name)
}

/// The first emoji in what has been typed, so a query that also carries a word ("fire 🔥") can still be sent when no table
/// name matches. Empty when there is no emoji in the query: a plain word is a search, and the picker will not send it as a
/// reaction. The whole cluster is kept — the base character, its variation selector and any joiner that follows — so "❤️"
/// goes out as one reaction, not "❤" plus a stray selector.
pub fn emoji_of(query: &str) -> String {
    let chars: Vec<char> = query.chars().collect();
    let Some(start) = chars.iter().position(|c| is_emoji(*c)) else { return String::new() };
    chars[start..].iter().take_while(|c| is_emoji_part(**c)).collect()
}

/// A character that can begin an emoji: the pictographs, dingbats and symbol blocks a reaction is drawn from.
fn is_emoji(c: char) -> bool {
    matches!(c as u32,
        0x1F000..=0x1FAFF // pictographs, emoticons, transport, supplemental symbols, flags (regional indicators included)
        | 0x2600..=0x27BF // misc symbols and dingbats (☀ ☕ ⚠ ✔ ✌ …)
        | 0x2B00..=0x2BFF // ⭐ ⬛ …
        | 0x2300..=0x23FF // ⌚ ⏰ ⏳ …
        | 0x2190..=0x21FF // ← ↕ …
        | 0x2900..=0x297F // ⤴ …
        | 0x3030 | 0x303D | 0x3297 | 0x3299 // 〰 〽 ㊗ ㊙
    )
}

/// A character that belongs to an emoji already begun: the emoji blocks plus the invisible joiners that hold a sequence together.
fn is_emoji_part(c: char) -> bool {
    is_emoji(c) || matches!(c as u32, 0x200D | 0xFE0E | 0xFE0F)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_empty_query_offers_the_whole_table() {
        assert_eq!(search("").len(), EMOJI.len());
        assert_eq!(search("   ").len(), EMOJI.len());
    }

    #[test]
    fn a_word_matches_any_part_of_a_name_and_every_word_has_to_match() {
        let thumbs: Vec<&str> = search("thumbs").iter().map(|(e, _)| *e).collect();
        assert!(thumbs.contains(&"👍") && thumbs.contains(&"👎"), "{thumbs:?}");
        assert_eq!(search("thumbs up").iter().map(|(e, _)| *e).collect::<Vec<_>>(), vec!["👍"]);
        assert!(search("THUMBS").iter().any(|(e, _)| *e == "👍"), "the query is case-insensitive");
        assert!(search("thumbs zzz").is_empty(), "every word must match");
    }

    #[test]
    fn the_table_names_a_glyph_it_has() {
        assert_eq!(name_of("👍"), Some("thumbs up yes like approve +1"));
        assert_eq!(name_of("🍕"), Some("pizza food"));
        assert_eq!(name_of("nope"), None);
    }

    #[test]
    fn every_glyph_is_unique_so_a_row_is_one_reaction() {
        let mut seen = std::collections::HashSet::new();
        for (glyph, _) in EMOJI {
            assert!(seen.insert(*glyph), "{glyph} appears twice");
        }
    }

    #[test]
    fn a_typed_emoji_is_found_wherever_it_sits_in_the_query() {
        assert_eq!(emoji_of("🔥"), "🔥");
        assert_eq!(emoji_of("fire 🔥"), "🔥", "a word before the emoji does not hide it");
        assert_eq!(emoji_of("👍 thanks"), "👍", "and nothing after it is pulled in");
        assert_eq!(emoji_of("❤️"), "❤️", "the variation selector stays with the glyph");
        assert_eq!(emoji_of(""), "");
        assert_eq!(emoji_of("fire"), "", "a plain word is a search, not a reaction");
        assert_eq!(emoji_of("café"), "", "an accented letter is not an emoji");
    }

    #[test]
    fn the_table_sends_a_glyph_it_can_name_and_the_query_can_send_one_it_cannot() {
        // `name_of` names the rows the picker draws from the table; `emoji_of` is the escape hatch for anything else.
        assert_eq!(name_of(&emoji_of("fire 🔥")), Some("fire hot lit"), "🔥 is in the table");
        assert_eq!(name_of("🦀"), None, "🦀 is not");
        assert_eq!(emoji_of("crab 🦀"), "🦀", "and a query can still send it");
    }
}
