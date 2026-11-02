//! The XML layer for OBS documents that are read by hand.
//!
//! OBS answers are small, flat and machine-generated. `osc.rs`,
//! `osc/branch.rs`, `maintenance.rs` and `obs_status.rs` read them with tag
//! scanners that get tag text and fragments rather than whole documents, so
//! a document parser does not fit: this module holds the one attribute reader
//! and the one entity decoder they all share, plus the matching escaper.
//! `quick-xml` still reads the service files, where the document is whole
//! (`osc::parse_service_xml`).
//!
//! Lenient by design: attribute order, quote style and spaces around `=` do
//! not matter, valueless attributes are skipped, and an unknown or malformed
//! reference is kept as written.

/// The `name="value"` / `name='value'` pairs in an attribute text, in order,
/// with values still escaped. Whatever is not a `name=value` pair (a valueless
/// attribute, a trailing `/` or `>`) is skipped; a value that never closes
/// ends the scan.
fn raw_attrs(mut s: &str) -> Vec<(&str, &str)> {
    let mut out = Vec::new();
    loop {
        s = s.trim_start();
        let name_end = s
            .find(|c: char| c.is_whitespace() || c == '=')
            .unwrap_or(s.len());
        let name = &s[..name_end];
        if name.is_empty() {
            break;
        }
        let after = s[name_end..].trim_start();
        let Some(value) = after.strip_prefix('=') else {
            // Valueless attribute, or a stray token: skip it.
            s = after;
            continue;
        };
        let value = value.trim_start();
        let Some(quote) = value.chars().next().filter(|c| *c == '"' || *c == '\'') else {
            break;
        };
        let body = &value[1..];
        let Some(close) = body.find(quote) else {
            break;
        };
        out.push((name, &body[..close]));
        s = &body[close + 1..];
    }
    out
}

/// The value of attribute `name` in `tag`, the text of a start tag after the
/// element name (`project="a" name='b'`). The first attribute of that name
/// wins; the value is unescaped, since every caller wants the name the
/// attribute stands for and the callers that write it back escape it again.
pub(crate) fn attr(tag: &str, name: &str) -> Option<String> {
    raw_attrs(tag)
        .into_iter()
        .find(|(k, _)| *k == name)
        .map(|(_, v)| unescape(v))
}

/// Every attribute of `tag` in order, unescaped.
pub(crate) fn attrs(tag: &str) -> Vec<(String, String)> {
    raw_attrs(tag)
        .into_iter()
        .map(|(k, v)| (k.to_string(), unescape(v)))
        .collect()
}

/// Escape text for an attribute value or element text.
pub(crate) fn escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// Undo XML escaping in an attribute value or element text: the five
/// predefined entities and decimal or hex character references, decoded in a
/// single pass (so `&amp;lt;` reads as the text `&lt;`). An unknown or
/// malformed reference is kept as written.
pub(crate) fn unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(amp) = rest.find('&') {
        out.push_str(&rest[..amp]);
        let after = &rest[amp..];
        let Some(semi) = after.find(';') else {
            out.push_str(after);
            return out;
        };
        let entity = &after[1..semi];
        let decoded = match entity {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            _ => entity.strip_prefix('#').and_then(|num| {
                let code = match num.strip_prefix(['x', 'X']) {
                    Some(hex) => u32::from_str_radix(hex, 16).ok(),
                    None => num.parse::<u32>().ok(),
                };
                code.and_then(char::from_u32)
            }),
        };
        match decoded {
            Some(c) => {
                out.push(c);
                rest = &after[semi + 1..];
            }
            None => {
                out.push('&');
                rest = &after[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// The start tag of the element whose attribute is at `at` in `masked`.
pub(crate) fn open_tag(masked: &str, at: usize) -> &str {
    let start = masked[..at].rfind('<').unwrap_or(0);
    let end = masked[start..]
        .find('>')
        .map_or(masked.len(), |i| start + i + 1);
    &masked[start..end]
}

/// The value of the parameter `name`, when the list has it.
pub(crate) fn param_value<'a>(params: &'a [(String, String)], name: &str) -> Option<&'a str> {
    params
        .iter()
        .find(|(k, _)| k == name)
        .map(|(_, v)| v.as_str())
}

/// The offsets (in `masked`) of the `<service>` elements named `name`.
pub(crate) fn service_elements(masked: &str, name: &str) -> Vec<usize> {
    masked
        .match_indices(&format!("name=\"{name}\""))
        .map(|(at, _)| at)
        .filter(|&at| open_tag(masked, at).starts_with("<service"))
        .collect()
}

/// `xml` with every XML comment blanked to spaces (newlines kept). The result
/// has the same length, so an offset in it is the offset in `xml`, and a
/// commented-out element is not read as a live one.
pub(crate) fn mask_xml_comments(xml: &str) -> String {
    let mut out = xml.as_bytes().to_vec();
    let mut from = 0;
    while let Some(open) = xml[from..].find("<!--").map(|i| from + i) {
        let end = xml[open + 4..]
            .find("-->")
            .map_or(xml.len(), |i| open + 4 + i + 3);
        for b in &mut out[open..end] {
            if *b != b'\n' {
                *b = b' ';
            }
        }
        from = end;
    }
    String::from_utf8(out).unwrap_or_default()
}

/// The text of the `<service>` element whose name starts at `at`: up to its
/// closing tag, or only its own tag when it is self-closing, so a parameter of
/// the next service is never taken for its own.
pub(crate) fn service_block(service: &str, at: usize) -> &str {
    let rest = &service[at..];
    let tag_end = rest.find('>').map_or(rest.len(), |i| i + 1);
    if rest[..tag_end].ends_with("/>") {
        return &rest[..tag_end];
    }
    let end = rest.find("</service>").unwrap_or(rest.len());
    &rest[..end]
}

/// The `<param>`s of the source service at `scm`, by name, with their values
/// decoded. Found in `masked`; the values are read from `service`, which has
/// the same offsets.
pub(crate) fn source_params(service: &str, masked: &str, scm: usize) -> Vec<(String, String)> {
    let end = scm + service_block(masked, scm).len();
    let open = "<param name=\"";
    let mut out = Vec::new();
    let mut at = scm;
    while let Some(p) = masked[at..end].find(open) {
        let name_at = at + p + open.len();
        let Some(quote) = masked[name_at..end].find('"') else {
            break;
        };
        let name = masked[name_at..name_at + quote].to_string();
        let tag_at = name_at + quote;
        let Some(gt) = masked[tag_at..end].find('>') else {
            break;
        };
        let value_at = tag_at + gt + 1;
        if masked[tag_at..value_at].ends_with("/>") {
            out.push((name, String::new()));
            at = value_at;
            continue;
        }
        let Some(close) = masked[value_at..end].find("</param>") else {
            break;
        };
        let value_end = value_at + close;
        out.push((name, unescape(service[value_at..value_end].trim())));
        at = value_end;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escape_covers_attribute_metacharacters() {
        assert_eq!(escape("a&b<c>\"d"), "a&amp;b&lt;c&gt;&quot;d");
    }

    #[test]
    fn unescape_decodes_predefined_entities_once() {
        assert_eq!(unescape("a&amp;b&lt;c&gt;&quot;d&apos;e"), "a&b<c>\"d'e");
        // Decodes once: an escaped ampersand stays as the literal text `&lt;`.
        assert_eq!(unescape("&amp;lt;"), "&lt;");
        // A numeric reference for `&` is not decoded a second time either.
        assert_eq!(unescape("&#38;lt;"), "&lt;");
    }

    #[test]
    fn unescape_decodes_character_references() {
        assert_eq!(unescape("&#65;&#x42;&#X43;"), "ABC");
        assert_eq!(unescape("&#x2B;"), "+");
        assert_eq!(unescape("it&#39;s"), "it's");
        assert_eq!(unescape("&#99999999999;"), "&#99999999999;");
    }

    #[test]
    fn unescape_keeps_unknown_and_malformed_references() {
        assert_eq!(unescape("a&nbsp;b"), "a&nbsp;b");
        assert_eq!(unescape("a&#xZZ;b"), "a&#xZZ;b");
        assert_eq!(unescape("a &#; b &#x; &#"), "a &#; b &#x; &#");
        assert_eq!(unescape("tail & more"), "tail & more");
        assert_eq!(unescape("no semicolon &amp"), "no semicolon &amp");
        assert_eq!(unescape("&#xD800;"), "&#xD800;");
    }

    #[test]
    fn unescape_inverts_escape() {
        let raw = "libsigc++ & \"<friends>\" 'x'";
        assert_eq!(unescape(&escape(raw)), raw);
    }

    #[test]
    fn attr_unescapes_value_and_misses_absent_names() {
        let tag = r#"<path project="a&amp;b" repository="x&quot;y&lt;z"/"#;
        assert_eq!(attr(tag, "project").as_deref(), Some("a&b"));
        assert_eq!(attr(tag, "repository").as_deref(), Some("x\"y<z"));
        assert_eq!(attr(tag, "missing"), None);
    }

    #[test]
    fn attr_matches_whole_names_only() {
        assert_eq!(attr(r#"filename="x""#, "name"), None);
        assert_eq!(
            attr(r#" baseproject="b" project="p""#, "project").as_deref(),
            Some("p")
        );
        assert_eq!(attr(r#"<a filename="x">"#, "name"), None);
        assert_eq!(attr(r#"<a name="x">"#, "name").as_deref(), Some("x"));
    }

    #[test]
    fn attr_takes_either_quote_and_spaces_around_equals() {
        assert_eq!(
            attr("name='a \"b\" c'", "name").as_deref(),
            Some("a \"b\" c")
        );
        assert_eq!(attr("name = \"x\"", "name").as_deref(), Some("x"));
        assert_eq!(attr("name=\"x", "name"), None);
    }

    #[test]
    fn attr_skips_valueless_attributes_and_stray_tokens() {
        assert_eq!(attr("hidden name=\"x\"", "name").as_deref(), Some("x"));
        assert_eq!(attr("/ name=\"x\" >", "name").as_deref(), Some("x"));
    }

    #[test]
    fn attr_does_not_read_a_name_inside_another_value() {
        assert_eq!(
            attr(r#"note='a code="b"' code='c'"#, "code").as_deref(),
            Some("c")
        );
        assert_eq!(attr(r#"a="1" a="2""#, "a").as_deref(), Some("1"));
    }

    #[test]
    fn attrs_lists_every_pair_in_order() {
        assert_eq!(
            attrs(r#"x='1' y="a&amp;b""#),
            vec![
                ("x".to_string(), "1".to_string()),
                ("y".to_string(), "a&b".to_string()),
            ]
        );
    }
}
