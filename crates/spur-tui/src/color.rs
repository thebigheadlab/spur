//! Theme tokens as terminal styles: true colour, or the nearest xterm-256 entries.

use std::collections::{BTreeMap, HashMap};

use spur_rpc::Token;

/// A colour as written to the terminal.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Color {
    /// 24-bit.
    Rgb(u8, u8, u8),
    /// An xterm-256 entry.
    Index(u8),
}

/// Text attributes.
pub mod attr {
    /// Bold.
    pub const BOLD: u8 = 1;
    /// Italic.
    pub const ITALIC: u8 = 2;
    /// Underline.
    pub const UNDERLINE: u8 = 4;
    /// Dim.
    pub const DIM: u8 = 8;
}

/// A token ready to paint. Unset colours keep what is under the painted area.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Style {
    /// Text colour.
    pub fg: Option<Color>,
    /// Background.
    pub bg: Option<Color>,
    /// [`attr`] bits.
    pub attrs: u8,
}

/// `#rrggbb` as three bytes.
pub fn parse_hex(s: &str) -> Option<(u8, u8, u8)> {
    let h = s.strip_prefix('#')?;
    if h.len() != 6 || !h.is_ascii() {
        return None;
    }
    let byte = |i: usize| u8::from_str_radix(&h[i..i + 2], 16).ok();
    Some((byte(0)?, byte(2)?, byte(4)?))
}

/// The colour of xterm-256 entry `i` (16-255; the first 16 depend on the terminal).
pub fn xterm(i: u8) -> (u8, u8, u8) {
    const LEVELS: [u8; 6] = [0, 95, 135, 175, 215, 255];
    if i >= 232 {
        let g = 8 + 10 * (i - 232);
        return (g, g, g);
    }
    let n = usize::from(i.saturating_sub(16));
    (LEVELS[n / 36], LEVELS[(n / 6) % 6], LEVELS[n % 6])
}

fn dist(a: (u8, u8, u8), b: (u8, u8, u8)) -> u32 {
    let d = |x: u8, y: u8| u32::from(x.abs_diff(y)).pow(2);
    d(a.0, b.0) + d(a.1, b.1) + d(a.2, b.2)
}

/// Maps colours to entries 16-255, in order: each takes its nearest entry not taken
/// by an earlier, different colour, so distinct colours stay distinct.
pub fn map_256(colors: &[(u8, u8, u8)]) -> Vec<u8> {
    let mut taken: HashMap<u8, (u8, u8, u8)> = HashMap::new();
    let mut seen: HashMap<(u8, u8, u8), u8> = HashMap::new();
    colors
        .iter()
        .map(|&c| {
            if let Some(&i) = seen.get(&c) {
                return i;
            }
            let i = (16..=255u8)
                .filter(|i| taken.get(i).is_none_or(|t| *t == c))
                .min_by_key(|&i| (dist(c, xterm(i)), i))
                .unwrap_or(16);
            taken.insert(i, c);
            seen.insert(c, i);
            i
        })
        .collect()
}

/// A theme's token map, turned into styles.
#[derive(Clone, Debug, Default)]
pub struct Palette {
    styles: HashMap<String, Style>,
}

impl Palette {
    /// Builds the styles; without true colour, colours are mapped in order of first
    /// use in the token map (each token's fg, then bg).
    pub fn new(tokens: &BTreeMap<String, Token>, truecolor: bool) -> Self {
        let mut order = Vec::new();
        for t in tokens.values() {
            for c in [&t.fg, &t.bg].into_iter().flatten() {
                if let Some(rgb) = parse_hex(c) {
                    order.push(rgb);
                }
            }
        }
        let mapped: HashMap<(u8, u8, u8), u8> = if truecolor {
            HashMap::new()
        } else {
            order.iter().copied().zip(map_256(&order)).collect()
        };
        let color = |s: &Option<String>| {
            let rgb = parse_hex(s.as_deref()?)?;
            Some(if truecolor {
                Color::Rgb(rgb.0, rgb.1, rgb.2)
            } else {
                Color::Index(mapped.get(&rgb).copied().unwrap_or(16))
            })
        };
        let styles = tokens
            .iter()
            .map(|(name, t)| {
                let mut attrs = 0;
                for (on, bit) in [
                    (t.bold, attr::BOLD),
                    (t.italic, attr::ITALIC),
                    (t.underline, attr::UNDERLINE),
                    (t.dim, attr::DIM),
                ] {
                    if on {
                        attrs |= bit;
                    }
                }
                let s = Style {
                    fg: color(&t.fg),
                    bg: color(&t.bg),
                    attrs,
                };
                (name.clone(), s)
            })
            .collect();
        Self { styles }
    }

    /// The style named `name`, if the theme has it.
    pub fn get(&self, name: &str) -> Option<Style> {
        self.styles.get(name).copied()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_and_cube() {
        assert_eq!(parse_hex("#0a0B10"), Some((10, 11, 16)));
        assert_eq!(parse_hex("0a0b10"), None);
        assert_eq!(parse_hex("#0a0b1"), None);
        assert_eq!(xterm(16), (0, 0, 0));
        assert_eq!(xterm(231), (255, 255, 255));
        assert_eq!(xterm(196), (255, 0, 0));
        assert_eq!(xterm(232), (8, 8, 8));
        assert_eq!(xterm(255), (238, 238, 238));
    }

    #[test]
    fn distinct_colours_keep_distinct_entries() {
        // Paper's bg and soft are both nearest to entry 255.
        let bg = parse_hex("#f3f3f1").unwrap();
        let soft = parse_hex("#efefec").unwrap();
        let m = map_256(&[bg, soft, bg]);
        assert_eq!(m[0], 255);
        assert_ne!(m[0], m[1]);
        assert_eq!(m[0], m[2], "the same colour keeps its entry");
    }

    fn theme(path: &str) -> BTreeMap<String, Token> {
        // A small reader for the pack's theme files: `name = { fg = "..", .. }`.
        let text = std::fs::read_to_string(path).unwrap();
        let mut out = BTreeMap::new();
        for line in text.lines() {
            let Some((name, rest)) = line.split_once(" = {") else {
                continue;
            };
            let name = name.trim().trim_matches('"').to_owned();
            let field = |k: &str| {
                let at = rest.find(&format!("{k} = \""))?;
                let v = &rest[at + k.len() + 4..];
                Some(v[..v.find('"')?].to_owned())
            };
            let resolve = |v: Option<String>| {
                v.map(|v| {
                    if v.starts_with('#') {
                        v
                    } else {
                        palette_of(&text, &v)
                    }
                })
            };
            out.insert(
                name,
                Token {
                    fg: resolve(field("fg")),
                    bg: resolve(field("bg")),
                    ..Token::default()
                },
            );
        }
        out
    }

    fn palette_of(text: &str, name: &str) -> String {
        text.lines()
            .find_map(|l| {
                let (k, v) = l.split_once('=')?;
                (k.trim() == name).then(|| v.trim().trim_matches('"').to_owned())
            })
            .unwrap_or_default()
    }

    #[test]
    fn every_theme_keeps_its_colours_apart_in_256_colours() {
        let dir = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../extensions/ux_fresh/themes"
        );
        for name in ["graphite", "paper", "sand", "midnight"] {
            let tokens = theme(&format!("{dir}/{name}.toml"));
            assert!(tokens.len() > 10, "{name}: read {} tokens", tokens.len());
            let mut colours: Vec<(u8, u8, u8)> = tokens
                .values()
                .flat_map(|t| [&t.fg, &t.bg])
                .flatten()
                .filter_map(|c| parse_hex(c))
                .collect();
            colours.sort_unstable();
            colours.dedup();
            let m = map_256(&colours);
            let mut entries = m.clone();
            entries.sort_unstable();
            entries.dedup();
            assert_eq!(entries.len(), m.len(), "{name}: two colours share an entry");
            let p = Palette::new(&tokens, false);
            assert!(matches!(
                p.get("app").and_then(|s| s.bg),
                Some(Color::Index(_))
            ));
        }
    }
}
