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
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}
