//! Make English fit the game's text box.
//!
//! Most VN engines don't word-wrap: they break where the script says, or cut
//! words in half at a fixed number of character cells. So we measure the box
//! from the original Japanese (Japanese characters are two cells wide, ASCII
//! one), word-wrap the English to that width with the engine's own line
//! break, report lines that still need more rows than the box has, and swap
//! out characters the game's text encoding can't show.

use crate::protect::{CODE_RE, PH_RE};
use serde::{Deserialize, Serialize};
use unicode_normalization::UnicodeNormalization;
use unicode_width::UnicodeWidthChar;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct TextBox {
    /// "explicit": the script breaks lines itself (so must we).
    /// "auto": the engine wraps on its own (usually mid-word).
    pub mode: String,
    pub width: usize,
    pub lines: usize,
    #[serde(default)]
    pub sample: usize,
    #[serde(default)]
    pub user_set: bool,
    /// The game draws English in a proportional font: "i" takes less room
    /// than "m", so more fits on a line than a fixed-width count says.
    #[serde(default)]
    pub proportional: bool,
}

impl Default for TextBox {
    fn default() -> Self {
        TextBox { mode: "auto".into(), width: 56, lines: 3, sample: 0, user_set: false, proportional: false }
    }
}

pub fn cell_width(ch: char) -> usize {
    if ch == '\u{200b}' || ch == '\u{feff}' {
        return 0;
    }
    match ch.width() {
        Some(2) => 2,
        _ => 1,
    }
}

pub fn text_cells(s: &str) -> usize {
    let plain = CODE_RE.replace_all(&PH_RE.replace_all(s, ""), "").into_owned();
    plain.chars().map(cell_width).sum()
}

fn percentile(values: &mut [usize], pct: f64) -> usize {
    if values.is_empty() {
        return 0;
    }
    values.sort_unstable();
    let n = values.len();
    let k = ((pct / 100.0) * (n as f64 - 1.0)).round() as usize;
    values[k.min(n - 1)]
}

/// Guess the text box from the original messages (line breaks as '\n').
pub fn measure_box(messages: &[String]) -> TextBox {
    let msgs: Vec<&String> = messages.iter().filter(|m| !m.trim().is_empty()).collect();
    if msgs.is_empty() {
        return TextBox::default();
    }
    let with_breaks = msgs.iter().filter(|m| m.contains('\n')).count();
    let explicit = with_breaks as f64 >= f64::max(5.0, 0.15 * msgs.len() as f64);
    let (width, lines) = if explicit {
        let mut widths = Vec::new();
        let mut counts = Vec::new();
        for m in &msgs {
            let parts: Vec<&str> = m.split('\n').collect();
            counts.push(parts.len());
            widths.extend(parts.iter().map(|p| text_cells(p)));
        }
        (percentile(&mut widths, 99.0), percentile(&mut counts, 99.0).max(2))
    } else {
        let lens: Vec<usize> = msgs.iter().map(|m| text_cells(m)).collect();
        let mut long: Vec<usize> = lens.iter().copied().filter(|&x| x >= 40).collect();
        if long.is_empty() {
            long = lens.clone();
        }
        let w = percentile(&mut long, 50.0).clamp(40, 56);
        (w, 3)
    };
    TextBox {
        mode: if explicit { "explicit" } else { "auto" }.into(),
        width: width.clamp(24, 120),
        lines: lines.min(8),
        sample: msgs.len(),
        user_set: false,
        proportional: false,
    }
}

/// Width of `ch` in a proportional font, in tenths of a cell (an average
/// Latin letter is about one cell, a Japanese character two).
pub fn prop_units(ch: char) -> usize {
    match cell_width(ch) {
        0 => 0,
        2 => 20,
        _ => match ch {
            ' ' => 6,
            'i' | 'l' | 'j' | 'I' | '!' | '|' | '.' | ',' | ':' | ';' | '\'' | '`' => 5,
            'f' | 't' | 'r' | '(' | ')' | '[' | ']' | '{' | '}' | '"' | '-' => 7,
            'm' | 'w' | 'M' | 'W' | '@' | '%' => 15,
            c if c.is_ascii_uppercase() => 12,
            _ => 10,
        },
    }
}

/// Width of `ch` in tenths of a cell.
fn units(ch: char, proportional: bool) -> usize {
    if proportional {
        prop_units(ch)
    } else {
        cell_width(ch) * 10
    }
}

/// Width of `s` in tenths of a cell; codes count as zero.
pub fn text_units(s: &str, proportional: bool) -> usize {
    let plain = CODE_RE.replace_all(&PH_RE.replace_all(s, ""), "").into_owned();
    plain.chars().map(|c| units(c, proportional)).sum()
}

fn cut_at(word: &str, limit: usize, proportional: bool) -> usize {
    let mut total = 0;
    for (i, ch) in word.char_indices() {
        total += units(ch, proportional);
        if total > limit {
            return if i == 0 { ch.len_utf8() } else { i };
        }
    }
    word.len()
}

fn wrap_para(text: &str, width: usize, line_break: &str, proportional: bool) -> (String, usize) {
    let limit = width * 10;
    let space = units(' ', proportional);
    let mut lines: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut cur_w = 0usize;
    for w in text.split_whitespace() {
        let w = w.to_string();
        let ww = text_units(&w, proportional);
        if !cur.is_empty() && (ww == 0 || cur_w + space + ww <= limit) {
            if ww > 0 {
                cur.push(' ');
                cur_w += space + ww;
            }
            cur.push_str(&w);
            continue;
        }
        if !cur.is_empty() {
            lines.push(std::mem::take(&mut cur));
        }
        // A word longer than the box gets a hard break, wherever it falls.
        let (mut w, mut ww) = (w, ww);
        while ww > limit {
            let cut = cut_at(&w, limit, proportional);
            lines.push(w[..cut].to_string());
            w = w[cut..].to_string();
            ww = text_units(&w, proportional);
        }
        cur = w;
        cur_w = ww;
    }
    if !cur.is_empty() {
        lines.push(cur);
    }
    let n = lines.len();
    (lines.join(line_break), n)
}

/// Word-wrap by cells, keeping existing hard breaks. Returns (text, line count).
/// Codes count as zero width and are never split.
pub fn wrap(text: &str, width: usize, line_break: &str) -> (String, usize) {
    wrap_mode(text, width, line_break, false)
}

/// [`wrap`], measuring English in a proportional font when `proportional`.
pub fn wrap_mode(text: &str, width: usize, line_break: &str, proportional: bool) -> (String, usize) {
    if width == 0 || text.is_empty() {
        return (text.to_string(), usize::from(!text.is_empty()));
    }
    let parts: Vec<&str> = if line_break.is_empty() { vec![text] } else { text.split(line_break).collect() };
    let mut out = Vec::with_capacity(parts.len());
    let mut total = 0;
    let last = parts.len() - 1;
    for (k, part) in parts.iter().enumerate() {
        if part.trim().is_empty() {
            out.push(part.to_string());
            if k < last {
                total += 1;
            }
            continue;
        }
        let (w, n) = wrap_para(part, width, line_break, proportional);
        out.push(w);
        total += n;
    }
    (out.join(line_break), total.max(1))
}

/// Wrap text for a box. Returns (wrapped, lines over the limit).
/// In "auto" mode the engine wraps on its own, but usually mid-word, so we
/// still insert breaks unless `insert_breaks` is false.
pub fn fit(text: &str, b: &TextBox, line_break: &str, insert_breaks: bool) -> (String, usize) {
    if !insert_breaks {
        let lines = text_units(text, b.proportional).div_ceil(b.width.max(1) * 10).max(1);
        return (text.to_string(), lines.saturating_sub(b.lines));
    }
    let (wrapped, n) = wrap_mode(text, b.width, line_break, b.proportional);
    (wrapped, n.saturating_sub(b.lines))
}

pub fn can_encode_sjis(ch: char) -> bool {
    if ch.is_ascii() {
        return true;
    }
    let mut buf = [0u8; 4];
    let s = ch.encode_utf8(&mut buf);
    let (_, _, had_errors) = encoding_rs::SHIFT_JIS.encode(s);
    !had_errors
}

/// Encode as Shift-JIS, replacing anything unmappable with '?'.
pub fn encode_sjis(text: &str) -> Vec<u8> {
    let safe: String = text.chars().map(|c| if can_encode_sjis(c) { c } else { '?' }).collect();
    encoding_rs::SHIFT_JIS.encode(&safe).0.into_owned()
}

fn sjis_replace(ch: char) -> char {
    match ch {
        '\u{2014}' => '\u{2015}', // em dash -> horizontal bar (exists in SJIS)
        '\u{2013}' | '\u{2012}' => '-',
        '\u{00a0}' | '\u{2009}' | '\u{200a}' => ' ',
        c => c,
    }
}

/// Make text displayable in the game's script encoding.
pub fn normalize_encoding(text: &str, encoding: &str, straight_quotes: bool) -> String {
    let mut t: String = if straight_quotes {
        text.chars()
            .map(|c| match c {
                '\u{201c}' | '\u{201d}' => '"',
                '\u{2018}' | '\u{2019}' => '\'',
                c => c,
            })
            .collect()
    } else {
        text.to_string()
    };
    let enc = encoding.to_ascii_lowercase();
    if enc.is_empty() || enc.starts_with("utf") || enc == "unicode" {
        return t;
    }
    let mut out = String::with_capacity(t.len());
    for ch in t.drain(..) {
        let ch = sjis_replace(ch);
        if can_encode_sjis(ch) {
            out.push(ch);
        } else {
            let base: String = ch.to_string().nfkd().filter(|c| c.is_ascii()).collect();
            if base.is_empty() {
                out.push('?');
            } else {
                out.push_str(&base);
            }
        }
    }
    out
}
