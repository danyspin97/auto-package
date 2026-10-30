//! The one host-name validator: every host is checked here before it is
//! stored, compared or used, whether it comes from the proxy's CONNECT line,
//! a `url` service parameter, an agent's answer or the user's own input.

use std::net::IpAddr;

/// Names under which anyone may register a name: the suffixes of the public
/// suffix list that are most often mistaken for a domain. A user may not allow
/// one, because that would allow every name under it.
///
/// This is a best-effort copy, not the whole list, and it is only a guard
/// against the common mistakes. Each entry is in the public suffix list
/// (https://publicsuffix.org/list/public_suffix_list.dat, the ICANN section for
/// the country codes, the PRIVATE section for the hosting services). To add
/// one, check it there first; `github.io` is refused, `foo.github.io` is not.
/// The AWS names are not here: see [`is_shared_aws_name`].
const PUBLIC_SUFFIXES: &[&str] = &[
    // Country-code second levels (ICANN section).
    "ac.in",
    "ac.jp",
    "ac.uk",
    "co.id",
    "co.il",
    "co.in",
    "co.jp",
    "co.ke",
    "co.kr",
    "co.nz",
    "co.th",
    "co.uk",
    "co.za",
    "com.ar",
    "com.au",
    "com.bd",
    "com.br",
    "com.cn",
    "com.co",
    "com.eg",
    "com.gr",
    "com.hk",
    "com.my",
    "com.mx",
    "com.ng",
    "com.pe",
    "com.ph",
    "com.pk",
    "com.pl",
    "com.sa",
    "com.sg",
    "com.tr",
    "com.tw",
    "com.ua",
    "com.ve",
    "com.vn",
    "edu.au",
    "gov.au",
    "gov.uk",
    "govt.nz",
    "ne.jp",
    "net.au",
    "net.br",
    "net.cn",
    "net.in",
    "or.jp",
    "org.au",
    "org.br",
    "org.cn",
    "org.in",
    "org.nz",
    "org.uk",
    // Hosting services that give each customer a name (PRIVATE section).
    "appspot.com",
    "azureedge.net",
    "azurewebsites.net",
    "bitbucket.io",
    "blogspot.com",
    "cloudfront.net",
    "codeberg.page",
    "duckdns.org",
    "firebaseapp.com",
    "fly.dev",
    "githubusercontent.com",
    "github.io",
    "gitlab.io",
    "herokuapp.com",
    "netlify.app",
    "ngrok.io",
    "onrender.com",
    "pages.dev",
    "r2.dev",
    "readthedocs.io",
    "repl.co",
    "surge.sh",
    "trycloudflare.com",
    "vercel.app",
    "web.app",
    "workers.dev",
];

/// `raw` as a host name to compare: lowercase, without one trailing dot.
///
/// Refused: anything that is not a plain DNS name (IP literals, numeric
/// forms a resolver may read as an address, ports, schemes, paths, user
/// info, whitespace), non-ASCII input, IDNA (`xn--`) labels that are not
/// valid punycode of a non-ASCII name, wildcards, empty or over-long labels,
/// and names over 253 characters. A valid `xn--` label is the ASCII form of an
/// internationalized name, so it is a plain DNS name and is accepted.
pub fn normalize_host(raw: &str) -> Result<String, String> {
    if raw.contains('*') {
        return Err(format!(
            "`{raw}` is a wildcard; wildcards are not supported, name the host itself"
        ));
    }
    if !raw.is_ascii() {
        return Err(format!(
            "`{raw}` is not ASCII; write an internationalized name in its `xn--` form"
        ));
    }
    let lower = raw.to_ascii_lowercase();
    let host = lower.strip_suffix('.').unwrap_or(&lower);
    if host.starts_with('[') || host.parse::<IpAddr>().is_ok() {
        return Err(format!("`{raw}` is an IP address, not a host name"));
    }
    if host.is_empty() || host.len() > 253 {
        return Err(format!("`{raw}` is not a valid host name"));
    }
    for label in host.split('.') {
        if label.is_empty()
            || label.len() > 63
            || label.starts_with('-')
            || label.ends_with('-')
            || !label
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-')
        {
            return Err(format!(
                "`{raw}` is not a plain host name (no port, scheme, path, user info or spaces)"
            ));
        }
        if let Some(encoded) = label.strip_prefix("xn--")
            && decode_punycode(encoded).is_none()
        {
            return Err(format!(
                "`{raw}` has an `xn--` label that is not valid punycode of a name"
            ));
        }
    }
    // No real name ends in a numeric label (`2130706433`, `0x7f.1`).
    let last = host.rsplit('.').next().unwrap_or("");
    if last.bytes().all(|b| b.is_ascii_digit()) || last.starts_with("0x") {
        return Err(format!("`{raw}` is an IP address, not a host name"));
    }
    Ok(host.to_string())
}

/// [`normalize_host`] for a name the user allows: at least two labels, and not
/// one of [`PUBLIC_SUFFIXES`]. A single label (`com`) or a suffix (`co.uk`)
/// would allow every name under it.
pub fn normalize_user_host(raw: &str) -> Result<String, String> {
    let host = normalize_host(raw)?;
    if !host.contains('.') {
        return Err(format!(
            "`{raw}` is a single label; name a host under a domain, e.g. mirror.example.org"
        ));
    }
    if PUBLIC_SUFFIXES.contains(&host.as_str()) || is_shared_aws_name(&host) {
        return Err(format!(
            "`{raw}` is a public suffix; allowing it would allow every name under it, \
             name the host itself, e.g. mirror.{host}"
        ));
    }
    Ok(host)
}

/// `amazonaws.com`, and the names AWS assigns under it with at most two labels
/// before it (`s3.amazonaws.com`, `s3.us-east-1.amazonaws.com`,
/// `bucket.s3.amazonaws.com`): every customer shares them, so allowing one
/// allows all of them. A name with a third label before the domain
/// (`bucket.s3.us-east-1.amazonaws.com`) is one customer's, and is allowed.
fn is_shared_aws_name(host: &str) -> bool {
    match host.strip_suffix(".amazonaws.com") {
        None => host == "amazonaws.com",
        Some(prefix) => prefix.split('.').count() <= 2,
    }
}

/// The Unicode name an `xn--` label (without its prefix) stands for, when it is
/// valid punycode (RFC 3492) of a name of letters and digits with at least one
/// non-ASCII letter. `None` for anything else.
fn decode_punycode(encoded: &str) -> Option<String> {
    const BASE: u32 = 36;
    const TMIN: u32 = 1;
    const TMAX: u32 = 26;
    const SKEW: u32 = 38;
    const DAMP: u32 = 700;
    const INITIAL_BIAS: u32 = 72;
    const INITIAL_N: u32 = 128;

    let (basic, digits) = match encoded.rfind('-') {
        Some(at) => (&encoded[..at], &encoded[at + 1..]),
        None => ("", encoded),
    };
    if digits.is_empty() || !basic.is_ascii() {
        return None;
    }
    let mut out: Vec<char> = basic.chars().collect();
    let (mut n, mut i, mut bias) = (INITIAL_N, 0u32, INITIAL_BIAS);
    let bytes = digits.as_bytes();
    let mut pos = 0;
    while pos < bytes.len() {
        let old_i = i;
        let mut w: u32 = 1;
        let mut k = BASE;
        loop {
            let c = *bytes.get(pos)?;
            pos += 1;
            let digit = match c {
                b'a'..=b'z' => u32::from(c - b'a'),
                b'A'..=b'Z' => u32::from(c - b'A'),
                b'0'..=b'9' => u32::from(c - b'0') + 26,
                _ => return None,
            };
            i = i.checked_add(digit.checked_mul(w)?)?;
            let t = if k <= bias {
                TMIN
            } else if k >= bias + TMAX {
                TMAX
            } else {
                k - bias
            };
            if digit < t {
                break;
            }
            w = w.checked_mul(BASE - t)?;
            k += BASE;
        }
        let len = u32::try_from(out.len()).ok()? + 1;
        // Adapt the bias to the gap just coded (RFC 3492, section 6.1).
        let mut delta = if old_i == 0 {
            (i - old_i) / DAMP
        } else {
            (i - old_i) / 2
        };
        delta += delta / len;
        let mut step = 0;
        while delta > ((BASE - TMIN) * TMAX) / 2 {
            delta /= BASE - TMIN;
            step += BASE;
        }
        bias = step + ((BASE - TMIN + 1) * delta) / (delta + SKEW);
        n = n.checked_add(i / len)?;
        i %= len;
        let ch = char::from_u32(n)?;
        if !ch.is_alphanumeric() {
            return None;
        }
        out.insert(usize::try_from(i).ok()?, ch);
        i += 1;
    }
    out.iter()
        .any(|c| !c.is_ascii())
        .then(|| out.into_iter().collect())
}
