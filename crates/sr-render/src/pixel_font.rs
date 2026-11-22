//! A small bitmap font for the settings overlay, designed for this project (not taken from
//! any existing font): capitals, digits, the punctuation the menus use and a few arrows.
//! Each glyph is 5 dots wide and 7 tall above the baseline, with one row below it (comma,
//! underscore); text is drawn on a 6-dot advance (monospaced). Lower case is shown in
//! capitals, accented letters as their base letter, anything else as a hollow box.

use std::sync::OnceLock;

/// Dots from one character to the next.
pub const ADVANCE: i32 = 6;
/// Height of capitals and digits (the baseline is below row 6).
pub const CAP: i32 = 7;

/// Rows top to bottom, `#` = dot; an eighth row is below the baseline.
const GLYPHS: &[(char, &str)] = &[
    (' ', "..... ..... ..... ..... ..... ..... ....."),
    ('A', ".###. #...# #...# ##### #...# #...# #...#"),
    ('B', "####. #...# #...# ####. #...# #...# ####."),
    ('C', ".###. #...# #.... #.... #.... #...# .###."),
    ('D', "####. #...# #...# #...# #...# #...# ####."),
    ('E', "##### #.... #.... ####. #.... #.... #####"),
    ('F', "##### #.... #.... ####. #.... #.... #...."),
    ('G', ".###. #...# #.... #.### #...# #...# .###."),
    ('H', "#...# #...# #...# ##### #...# #...# #...#"),
    ('I', ".###. ..#.. ..#.. ..#.. ..#.. ..#.. .###."),
    ('J', "....# ....# ....# ....# ....# #...# .###."),
    ('K', "#...# #..#. #.#.. ##... #.#.. #..#. #...#"),
    ('L', "#.... #.... #.... #.... #.... #.... #####"),
    ('M', "#...# ##.## #.#.# #.#.# #...# #...# #...#"),
    ('N', "#...# #...# ##..# #.#.# #..## #...# #...#"),
    ('O', ".###. #...# #...# #...# #...# #...# .###."),
    ('P', "####. #...# #...# ####. #.... #.... #...."),
    ('Q', ".###. #...# #...# #...# #.#.# #..#. .##.#"),
    ('R', "####. #...# #...# ####. #.#.. #..#. #...#"),
    ('S', ".###. #...# #.... .###. ....# #...# .###."),
    ('T', "##### ..#.. ..#.. ..#.. ..#.. ..#.. ..#.."),
    ('U', "#...# #...# #...# #...# #...# #...# .###."),
    ('V', "#...# #...# #...# #...# .#.#. .#.#. ..#.."),
    ('W', "#...# #...# #...# #.#.# #.#.# ##.## #...#"),
    ('X', "#...# #...# .#.#. ..#.. .#.#. #...# #...#"),
    ('Y', "#...# #...# .#.#. ..#.. ..#.. ..#.. ..#.."),
    ('Z', "##### ....# ...#. ..#.. .#... #.... #####"),
    ('0', ".###. #...# #..## #.#.# ##..# #...# .###."),
    ('1', "..#.. .##.. ..#.. ..#.. ..#.. ..#.. .###."),
    ('2', ".###. #...# ....# ...#. ..#.. .#... #####"),
    ('3', ".###. #...# ....# ..##. ....# #...# .###."),
    ('4', "...#. ..##. .#.#. #..#. ##### ...#. ...#."),
    ('5', "##### #.... ####. ....# ....# #...# .###."),
    ('6', "..##. .#... #.... ####. #...# #...# .###."),
    ('7', "##### ....# ...#. ..#.. ..#.. ..#.. ..#.."),
    ('8', ".###. #...# #...# .###. #...# #...# .###."),
    ('9', ".###. #...# #...# .#### ....# ...#. .##.."),
    ('.', "..... ..... ..... ..... ..... .##.. .##.."),
    (',', "..... ..... ..... ..... ..... .##.. ..#.. .#..."),
    (':', "..... .##.. .##.. ..... ..... .##.. .##.."),
    (';', "..... .##.. .##.. ..... ..... .##.. ..#.. .#..."),
    ('\'', "..#.. ..#.. .#... ..... ..... ..... ....."),
    ('"', ".#.#. .#.#. ..... ..... ..... ..... ....."),
    ('!', "..#.. ..#.. ..#.. ..#.. ..#.. ..... ..#.."),
    ('?', ".###. #...# ....# ...#. ..#.. ..... ..#.."),
    ('(', "...#. ..#.. .#... .#... .#... ..#.. ...#."),
    (')', ".#... ..#.. ...#. ...#. ...#. ..#.. .#..."),
    ('[', ".###. .#... .#... .#... .#... .#... .###."),
    (']', ".###. ...#. ...#. ...#. ...#. ...#. .###."),
    ('-', "..... ..... ..... ##### ..... ..... ....."),
    ('+', "..... ..#.. ..#.. ##### ..#.. ..#.. ....."),
    ('=', "..... ..... ##### ..... ##### ..... ....."),
    ('%', "##..# ##..# ...#. ..#.. .#... #..## #..##"),
    ('/', "....# ....# ...#. ..#.. .#... #.... #...."),
    ('\\', "#.... #.... .#... ..#.. ...#. ....# ....#"),
    ('&', ".##.. #..#. #.#.. .#... #.#.# #..#. .##.#"),
    ('<', "...#. ..#.. .#... #.... .#... ..#.. ...#."),
    ('>', ".#... ..#.. ...#. ....# ...#. ..#.. .#..."),
    ('_', "..... ..... ..... ..... ..... ..... ..... #####"),
    ('*', "..... ..#.. #.#.# .###. #.#.# ..#.. ....."),
    ('#', ".#.#. .#.#. ##### .#.#. ##### .#.#. .#.#."),
    ('|', "..#.. ..#.. ..#.. ..#.. ..#.. ..#.. ..#.."),
    ('^', "..#.. .#.#. #...# ..... ..... ..... ....."),
    ('\u{2190}', "..... ..#.. .#... ##### .#... ..#.. ....."), // left arrow
    ('\u{2191}', "..#.. .###. #.#.# ..#.. ..#.. ..#.. ..#.."), // up arrow
    ('\u{2192}', "..... ..#.. ...#. ##### ...#. ..#.. ....."), // right arrow
    ('\u{2193}', "..#.. ..#.. ..#.. ..#.. #.#.# .###. ..#.."), // down arrow
    ('\u{25C0}', "..... ...#. ..##. .###. ..##. ...#. ....."), // left triangle
    ('\u{25B6}', "..... .#... .##.. .###. .##.. .#... ....."), // right triangle
    ('\u{25B2}', "..... ..... ..#.. .###. ##### ..... ....."), // up triangle
    ('\u{25BC}', "..... ..... ##### .###. ..#.. ..... ....."), // down triangle
    ('\u{2026}', "..... ..... ..... ..... ..... ..... #.#.#"), // ellipsis
];

/// Shown for characters the font does not have.
const MISSING: [u8; 8] = [0x1f, 0x11, 0x11, 0x11, 0x11, 0x11, 0x1f, 0];

/// Rows of a glyph, bit 4 = leftmost dot.
fn parse(rows: &str) -> [u8; 8] {
    let mut g = [0u8; 8];
    for (r, row) in rows.split_whitespace().take(8).enumerate() {
        for (c, ch) in row.chars().take(5).enumerate() {
            if ch == '#' {
                g[r] |= 0x10 >> c;
            }
        }
    }
    g
}

fn table() -> &'static Vec<(char, [u8; 8])> {
    static TABLE: OnceLock<Vec<(char, [u8; 8])>> = OnceLock::new();
    TABLE.get_or_init(|| GLYPHS.iter().map(|&(c, rows)| (c, parse(rows))).collect())
}

/// The character the font shows for `c`.
fn normalize(c: char) -> char {
    match c {
        'à' | 'á' | 'â' | 'ä' | 'ã' | 'å' | 'À' | 'Á' | 'Â' | 'Ä' | 'Ã' | 'Å' => 'A',
        'è' | 'é' | 'ê' | 'ë' | 'È' | 'É' | 'Ê' | 'Ë' => 'E',
        'ì' | 'í' | 'î' | 'ï' | 'Ì' | 'Í' | 'Î' | 'Ï' => 'I',
        'ò' | 'ó' | 'ô' | 'ö' | 'õ' | 'Ò' | 'Ó' | 'Ô' | 'Ö' | 'Õ' => 'O',
        'ù' | 'ú' | 'û' | 'ü' | 'Ù' | 'Ú' | 'Û' | 'Ü' => 'U',
        'ç' | 'Ç' => 'C',
        'ñ' | 'Ñ' => 'N',
        '\u{2013}' | '\u{2014}' | '\u{2212}' => '-',
        '\u{2018}' | '\u{2019}' | '`' => '\'',
        '\u{201C}' | '\u{201D}' => '"',
        '\u{00D7}' => 'X',
        '\t' => ' ',
        c => c.to_ascii_uppercase(),
    }
}

/// The glyph for `c`, if the font has one.
pub fn find(c: char) -> Option<[u8; 8]> {
    let c = normalize(c);
    table().iter().find(|(k, _)| *k == c).map(|(_, g)| *g)
}

/// The glyph for `c` (a hollow box if the font has none).
pub fn glyph(c: char) -> [u8; 8] {
    find(c).unwrap_or(MISSING)
}

/// Width of `s` in dots (no spacing after the last character).
pub fn width(s: &str) -> i32 {
    let n = s.chars().count() as i32;
    if n == 0 { 0 } else { n * ADVANCE - 1 }
}

/// Split `s` into lines of at most `cols` characters, at spaces where possible.
pub fn wrap(s: &str, cols: usize) -> Vec<String> {
    let mut lines = Vec::new();
    let mut line = String::new();
    for word in s.split_whitespace() {
        let mut word = word.to_string();
        loop {
            let len = line.chars().count();
            let wlen = word.chars().count();
            let need = if len == 0 { wlen } else { len + 1 + wlen };
            if need <= cols {
                if len > 0 {
                    line.push(' ');
                }
                line.push_str(&word);
                break;
            }
            if len > 0 {
                lines.push(std::mem::take(&mut line));
                continue;
            }
            // a word longer than a line: cut it
            let head: String = word.chars().take(cols).collect();
            word = word.chars().skip(cols).collect();
            lines.push(head);
            if word.is_empty() {
                break;
            }
        }
    }
    if !line.is_empty() {
        lines.push(line);
    }
    lines
}

/// `s` cut to at most `cols` characters, ending in an ellipsis if it was longer.
pub fn fit(s: &str, cols: usize) -> String {
    if s.chars().count() <= cols {
        return s.to_string();
    }
    let mut t: String = s.chars().take(cols.saturating_sub(1)).collect();
    t.push('\u{2026}');
    t
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn glyphs_are_well_formed() {
        let mut seen = std::collections::HashSet::new();
        for (c, rows) in GLYPHS {
            assert!(seen.insert(*c), "{c:?} twice");
            let rows: Vec<&str> = rows.split_whitespace().collect();
            assert!(rows.len() == 7 || rows.len() == 8, "{c:?}: {} rows", rows.len());
            assert!(rows.iter().all(|r| r.len() == 5 && r.chars().all(|ch| ch == '#' || ch == '.')), "{c:?}");
        }
        // every printable ASCII character the menus may show has a glyph of its own
        for c in "ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789 .,:;'\"!?()[]-+=%/&<>_*#|".chars() {
            assert!(find(c).is_some(), "{c:?}");
        }
        assert_eq!(glyph('a'), glyph('A'));
        assert_eq!(glyph('è'), glyph('E'));
        assert_eq!(glyph('\u{2603}'), MISSING);
        // distinct shapes for look-alikes
        assert_ne!(glyph('0'), glyph('O'));
        assert_ne!(glyph('1'), glyph('I'));
        assert_ne!(glyph('5'), glyph('S'));
        assert_ne!(glyph('8'), glyph('B'));
    }

    #[test]
    fn wrapping() {
        assert_eq!(wrap("one two three", 7), ["one two", "three"]);
        assert_eq!(wrap("abcdefghij", 4), ["abcd", "efgh", "ij"]);
        assert_eq!(wrap("", 4), Vec::<String>::new());
        assert_eq!(fit("abcdef", 4), "abc\u{2026}");
        assert_eq!(fit("abc", 4), "abc");
        assert_eq!(width("AB"), 11);
    }
}
