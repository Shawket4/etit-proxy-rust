//! Tiny string helpers shared across upstream integrations.

use percent_encoding::{utf8_percent_encode, AsciiSet, CONTROLS};

/// Most of the API needs a strict percent-encode (no slashes, no spaces).
pub const FRAGMENT: &AsciiSet = &CONTROLS
    .add(b' ')
    .add(b'"')
    .add(b'<')
    .add(b'>')
    .add(b'`')
    .add(b'#')
    .add(b'?')
    .add(b'{')
    .add(b'}')
    .add(b'/')
    .add(b'\\')
    .add(b'&')
    .add(b'=')
    .add(b'+')
    .add(b':')
    .add(b';')
    .add(b',');

pub fn pct(s: &str) -> String {
    utf8_percent_encode(s, FRAGMENT).to_string()
}

pub fn truncate_for_log(s: &str, n: usize) -> String {
    if s.len() <= n {
        s.to_string()
    } else {
        format!("{}…", &s[..n])
    }
}

/// Legacy ETIT history endpoint sometimes returns "JSON" with **unquoted**
/// top-level keys (`{history:[...]}`). This helper double-quotes them so
/// `serde_json` can parse the payload.
///
/// Conservative: only matches keys that follow `{` or `,` and look like
/// identifiers. Will not perturb keys inside string values.
pub fn fix_unquoted_json_keys(s: &str) -> String {
    static KEY_RE: once_cell::sync::Lazy<regex::Regex> = once_cell::sync::Lazy::new(|| {
        regex::Regex::new(r"([\{,])\s*([a-zA-Z_][a-zA-Z0-9_]*)\s*:").unwrap()
    });
    static COMMA_RE: once_cell::sync::Lazy<regex::Regex> = once_cell::sync::Lazy::new(|| {
        regex::Regex::new(r",\s*([\]\}])").unwrap()
    });

    let s = KEY_RE.replace_all(s, r#"$1"$2":"#);
    COMMA_RE.replace_all(&s, r"$1").into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixes_unquoted_keys() {
        let bad  = r#"{history:[{p:[],d:"1"}],meta:1}"#;
        let good = fix_unquoted_json_keys(bad);
        assert_eq!(
            good,
            r#"{"history":[{"p":[],"d":"1"}],"meta":1}"#
        );
    }

    #[test]
    fn leaves_already_quoted_alone() {
        let s = r#"{"history":[]}"#;
        assert_eq!(fix_unquoted_json_keys(s), s);
    }
}
