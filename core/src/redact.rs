//! Removing credentials from text before it is written to a run log.

/// `text` with URL userinfo (`https://user:pass@host` becomes
/// `https://***@host`), `Authorization:` header values and the values of
/// `*_TOKEN=`, `*_PASSWORD=` and `*_SECRET=` assignments replaced by `***`.
pub fn redact(text: &str) -> String {
    redact_assignments(&redact_authorization(&redact_url_userinfo(text)))
}

fn redact_url_userinfo(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(i) = rest.find("://") {
        let (head, tail) = rest.split_at(i + 3);
        out.push_str(head);
        let end = tail
            .find(|c: char| {
                c == '/' || c == '?' || c == '#' || c.is_whitespace() || c == '"' || c == '\''
            })
            .unwrap_or(tail.len());
        match tail[..end].rfind('@') {
            Some(at) => {
                out.push_str("***");
                out.push_str(&tail[at..end]);
                rest = &tail[end..];
            }
            None => rest = tail,
        }
    }
    out.push_str(rest);
    out
}

fn redact_authorization(text: &str) -> String {
    const KEY: &str = "authorization:";
    let lower = text.to_ascii_lowercase();
    let mut out = String::with_capacity(text.len());
    let mut at = 0;
    while let Some(i) = lower[at..].find(KEY) {
        let start = at + i + KEY.len();
        out.push_str(&text[at..start]);
        let end = text[start..]
            .find(['\n', '"', '\''])
            .map_or(text.len(), |e| start + e);
        out.push_str(" ***");
        at = end;
    }
    out.push_str(&text[at..]);
    out
}

fn redact_assignments(text: &str) -> String {
    let is_word = |b: u8| b.is_ascii_alphanumeric() || b == b'_';
    let bytes = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut at = 0;
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != b'=' || i < at {
            i += 1;
            continue;
        }
        let mut s = i;
        while s > at && is_word(bytes[s - 1]) {
            s -= 1;
        }
        let name = text[s..i].to_ascii_uppercase();
        let secret = ["_TOKEN", "_PASSWORD", "_SECRET"]
            .iter()
            .any(|suffix| name.ends_with(suffix));
        let vstart = i + 1 + usize::from(matches!(bytes.get(i + 1), Some(b'"' | b'\'')));
        let end = text[vstart..]
            .find(|c: char| c.is_whitespace() || c == '"' || c == '\'' || c == ';')
            .map_or(text.len(), |e| vstart + e);
        if secret && end > vstart {
            out.push_str(&text[at..vstart]);
            out.push_str("***");
            at = end;
            i = end;
        } else {
            i += 1;
        }
    }
    out.push_str(&text[at..]);
    out
}

#[cfg(test)]
mod tests {
    use super::redact;

    #[test]
    fn url_userinfo_is_replaced() {
        assert_eq!(
            redact("git clone https://user:pa55@host.example/x.git and http://a@b/"),
            "git clone https://***@host.example/x.git and http://***@b/"
        );
        assert_eq!(
            redact("https://host.example/a@b"),
            "https://host.example/a@b"
        );
        assert_eq!(redact("plain text"), "plain text");
    }

    #[test]
    fn authorization_values_are_replaced() {
        assert_eq!(
            redact("curl -H \"Authorization: Bearer abc\" x"),
            "curl -H \"Authorization: ***\" x"
        );
        assert_eq!(
            redact("authorization: Basic zzz\nnext"),
            "authorization: ***\nnext"
        );
    }

    #[test]
    fn secret_assignments_are_replaced() {
        assert_eq!(
            redact("GITHUB_TOKEN=abc DB_PASSWORD=p w=1 MY_SECRET='s3' X=1"),
            "GITHUB_TOKEN=*** DB_PASSWORD=*** w=1 MY_SECRET='***' X=1"
        );
        assert_eq!(redact("MY_SECRET=s3"), "MY_SECRET=***");
        assert_eq!(redact("TOKEN_COUNT=3 PATH=/bin"), "TOKEN_COUNT=3 PATH=/bin");
    }
}
