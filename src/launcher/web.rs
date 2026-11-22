//! Web search (5.4.8): the engines, the custom template rules, percent-encoding and the typed-URL check. The dock never
//! makes a network request; the row only opens the URL through the shell.

pub const MAX_TEMPLATE_CHARS: usize = 512;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Engine {
    Google,
    Bing,
    DuckDuckGo,
    Custom,
}

impl Engine {
    pub fn from_name(name: &str) -> Option<Engine> {
        Some(match name {
            "google" => Engine::Google,
            "bing" => Engine::Bing,
            "duckduckgo" => Engine::DuckDuckGo,
            "custom" => Engine::Custom,
            _ => return None,
        })
    }

    pub fn name(self) -> &'static str {
        match self {
            Engine::Google => "google",
            Engine::Bing => "bing",
            Engine::DuckDuckGo => "duckduckgo",
            Engine::Custom => "custom",
        }
    }

    /// The template of a built-in engine; None for Custom (it comes from the config).
    pub fn template(self) -> Option<&'static str> {
        match self {
            Engine::Google => Some("https://www.google.com/search?q={q}"),
            Engine::Bing => Some("https://www.bing.com/search?q={q}"),
            Engine::DuckDuckGo => Some("https://duckduckgo.com/?q={q}"),
            Engine::Custom => None,
        }
    }
}

/// A template that passed validate_template.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Template {
    text: Box<str>,
    /// Byte range of the host inside the text.
    host: std::ops::Range<usize>,
}

impl Template {
    pub fn as_str(&self) -> &str {
        &self.text
    }

    /// The host the web row names ("Search www.google.com for ...").
    pub fn host(&self) -> &str {
        self.text.get(self.host.clone()).unwrap_or("")
    }
}

const SCHEME: &str = "https://";
const HOLE: &str = "{q}";

impl Default for Template {
    /// Google, the default engine (5.6): the template `validate_template` makes of it (tested).
    fn default() -> Self {
        const GOOGLE: &str = "https://www.google.com/search?q={q}";
        const HOST: &str = "www.google.com";
        Template { text: GOOGLE.into(), host: SCHEME.len()..SCHEME.len() + HOST.len() }
    }
}

/// https only; an ASCII host of letters, digits, hyphens and dots (an IDN host is written in its punycode form); no
/// userinfo ("@" before the host); no port; exactly one {q}.
pub fn validate_template(t: &str) -> Result<Template, &'static str> {
    if t.chars().count() > MAX_TEMPLATE_CHARS {
        return Err("template too long");
    }
    if !t.chars().all(|c| c.is_ascii_graphic()) {
        return Err("only visible ASCII characters are allowed");
    }
    let Some(rest) = t.strip_prefix(SCHEME) else {
        return Err("the template must start with https://");
    };
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let authority = rest.get(..end).unwrap_or("");
    if authority.contains('@') {
        return Err("a user name before the host is not allowed");
    }
    if authority.contains(':') {
        return Err("a port is not allowed");
    }
    if authority.is_empty()
        || !authority.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '.')
        || authority.split('.').any(str::is_empty)
    {
        return Err("the host may only hold letters, digits, hyphens and dots");
    }
    if t.matches(HOLE).count() != 1 {
        return Err("the template needs exactly one {q}");
    }
    Ok(Template { text: t.into(), host: SCHEME.len()..SCHEME.len() + end })
}

/// The template with {q} replaced by the query in UTF-8 percent-encoding; unreserved characters stay as they are.
pub fn url_for(t: &Template, query: &str) -> String {
    let mut enc = String::with_capacity(query.len() * 3);
    for b in query.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
            enc.push(char::from(b));
        } else {
            enc.push('%');
            for nibble in [b >> 4, b & 15] {
                enc.push(char::from_digit(u32::from(nibble), 16).unwrap_or('0').to_ascii_uppercase());
            }
        }
    }
    t.text.replacen(HOLE, &enc, 1)
}

/// A typed address: "http://" or "https://" as typed (the scheme in lowercase), or "www." which gets "https://".
/// No whitespace, no control character, something after the scheme.
pub fn parse_typed_url(text: &str) -> Option<String> {
    let text = text.trim();
    if text.is_empty() || text.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return None;
    }
    let lower = |n: usize| text.get(..n).map(str::to_ascii_lowercase);
    for scheme in ["https://", "http://"] {
        if lower(scheme.len()).as_deref() == Some(scheme) {
            let rest = text.get(scheme.len()..)?;
            return (!rest.is_empty()).then(|| format!("{scheme}{rest}"));
        }
    }
    if lower(4).as_deref() == Some("www.") && text.len() > 4 {
        return Some(format!("https://{text}"));
    }
    None
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
mod tests {
    use super::*;

    #[test]
    fn template_rules() {
        let ok = validate_template("https://www.example.com/search?q={q}").unwrap();
        assert_eq!(ok.host(), "www.example.com");
        assert_eq!(validate_template("https://example.org/{q}").unwrap().host(), "example.org");
        assert_eq!(validate_template("https://example.org?x={q}").unwrap().host(), "example.org");
        // A punycode host is accepted.
        assert_eq!(validate_template("https://xn--citt-5qa.example/?q={q}").unwrap().host(), "xn--citt-5qa.example");
        for bad in [
            "http://example.com/?q={q}",
            "HTTPS://example.com/?q={q}",
            "//example.com/?q={q}",
            "example.com/?q={q}",
            "https://user@example.com/?q={q}",
            "https://user:pw@example.com/?q={q}",
            "https://example.com:8443/?q={q}",
            "https://example.com:443/?q={q}",
            "https://città.example/?q={q}",
            "https://exa mple.com/?q={q}",
            "https://example.com/?q={q}&r={q}",
            "https://example.com/?q=",
            "https://example.com/?q={Q}",
            "https://{q}.example.com/",
            "https://exam_ple.com/?q={q}",
            "https:///?q={q}",
            "https://.example.com/?q={q}",
            "https://example..com/?q={q}",
            "https://example.com/?q={q}\n",
            "https://example.com/ü?q={q}",
            "",
        ] {
            assert!(validate_template(bad).is_err(), "{bad:?}");
        }
        // An "@" in the path is only a path character; an "@" before the first slash is refused.
        assert!(validate_template("https://example.com/@x?q={q}").is_ok());
        assert!(validate_template("https://google.com@evil.example/?q={q}").is_err());
        let long = format!("https://example.com/{}{{q}}", "a".repeat(MAX_TEMPLATE_CHARS));
        assert!(validate_template(&long).is_err());
    }

    #[test]
    fn built_in_engines_validate() {
        for (e, host) in
            [(Engine::Google, "www.google.com"), (Engine::Bing, "www.bing.com"), (Engine::DuckDuckGo, "duckduckgo.com")]
        {
            let t = validate_template(e.template().unwrap()).unwrap();
            assert_eq!(t.host(), host);
            assert_eq!(Engine::from_name(e.name()), Some(e));
        }
        assert_eq!(Engine::Custom.template(), None);
        assert_eq!(Engine::from_name("custom"), Some(Engine::Custom));
        assert_eq!(Engine::from_name("yahoo"), None);
        let g = validate_template(Engine::Google.template().unwrap()).unwrap();
        assert_eq!(Template::default(), g);
        assert_eq!(Template::default().host(), "www.google.com");
        assert_eq!(url_for(&g, "rust lang"), "https://www.google.com/search?q=rust%20lang");
    }

    #[test]
    fn encoding() {
        let t = validate_template("https://e.example/s?q={q}&x=1").unwrap();
        assert_eq!(url_for(&t, "a b&c"), "https://e.example/s?q=a%20b%26c&x=1");
        assert_eq!(url_for(&t, "città"), "https://e.example/s?q=citt%C3%A0&x=1");
        assert_eq!(url_for(&t, "A-z_0.9~"), "https://e.example/s?q=A-z_0.9~&x=1");
        assert_eq!(url_for(&t, "{q}#/?=+%"), "https://e.example/s?q=%7Bq%7D%23%2F%3F%3D%2B%25&x=1");
        assert_eq!(url_for(&t, "😀"), "https://e.example/s?q=%F0%9F%98%80&x=1");
        assert_eq!(url_for(&t, ""), "https://e.example/s?q=&x=1");
    }

    #[test]
    fn typed_urls() {
        assert_eq!(parse_typed_url("https://example.com/a?b=1").as_deref(), Some("https://example.com/a?b=1"));
        assert_eq!(parse_typed_url("  http://example.com ").as_deref(), Some("http://example.com"));
        assert_eq!(parse_typed_url("HTTP://Example.com/X").as_deref(), Some("http://Example.com/X"));
        assert_eq!(parse_typed_url("www.example.com").as_deref(), Some("https://www.example.com"));
        assert_eq!(parse_typed_url("WWW.example.com/a").as_deref(), Some("https://WWW.example.com/a"));
        for bad in [
            "",
            "www.",
            "http://",
            "https://",
            "ftp://example.com",
            "example.com",
            "file:///c:/x",
            "http://a b",
            "www.a b",
            "javascript:alert(1)",
            "http://a\u{7}b",
        ] {
            assert_eq!(parse_typed_url(bad), None, "{bad:?}");
        }
    }
}
