//! Small string helpers for allocation-sensitive formatting paths.

pub fn char_prefix(s: &str, max_chars: usize) -> (&str, bool) {
    match s.char_indices().nth(max_chars) {
        Some((end, _)) => (&s[..end], true),
        None => (s, false),
    }
}
