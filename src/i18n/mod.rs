//! Compile-time locale selection.
//!
//! Set `[ui] locale = "zh"` in config.toml to compile Chinese strings.
//! Defaults to English when the key is absent.

#[cfg(locale_zh)]
mod zh;
#[cfg(locale_zh)]
pub use zh::*;

#[cfg(not(locale_zh))]
mod en;
#[cfg(not(locale_zh))]
pub use en::*;

fn html_escape(s: &str) -> String {
    let mut out = String::with_capacity(html_escaped_len(s));
    push_html_escaped(&mut out, s);
    out
}

fn html_escaped_len(s: &str) -> usize {
    let mut len = s.len();
    for byte in s.bytes() {
        match byte {
            b'&' => len += 4,
            b'<' | b'>' => len += 3,
            _ => {}
        }
    }
    len
}

fn push_html_escaped(out: &mut String, s: &str) {
    for ch in s.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            ch => out.push(ch),
        }
    }
}
