//! A reader for the part of Zig's ZON that `build.zig.zon` manifests use:
//! struct and tuple literals, strings, enum literals, numbers and booleans,
//! with `//` comments. It reads the fields the vendoring step needs and drops
//! the rest. It runs no code and resolves no references, so a manifest can
//! only describe data.

/// How deep struct and tuple literals may nest. A real manifest nests about
/// four levels; the bound stops a hostile one from exhausting the stack.
const MAX_DEPTH: usize = 32;

/// Largest manifest read. Real ones are a few kilobytes.
pub const MAX_MANIFEST_BYTES: usize = 1 << 20;

/// A `build.zig.zon` manifest, reduced to the fields the vendoring step reads.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Manifest {
    pub name: Option<String>,
    pub version: Option<String>,
    /// The package fingerprint. Its low 32 bits are the id in a package hash.
    pub fingerprint: Option<u64>,
    pub minimum_zig_version: Option<String>,
    /// The `.paths` filter. `None` when the field is absent (every file).
    pub paths: Option<Vec<String>>,
    /// The `.dependencies` in the order the manifest lists them.
    pub dependencies: Vec<Dependency>,
}

/// One entry of `.dependencies`. Exactly one of `url` and `path` is set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Dependency {
    pub name: String,
    pub url: Option<String>,
    /// The declared package hash, such as `pixman-0.3.0-LClM...`.
    pub hash: Option<String>,
    /// A local directory dependency. Not fetched.
    pub path: Option<String>,
    pub lazy: bool,
}

/// One parsed ZON value.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Value {
    Str(String),
    /// An enum literal (`.river`) or a quoted field name's identifier.
    Enum(String),
    /// A number, kept as written (`0xf5e3672b8e8d6efc`).
    Num(String),
    Bool(bool),
    /// `.{ .name = value, ... }`.
    Struct(Vec<(String, Value)>),
    /// `.{ value, ... }`.
    Tuple(Vec<Value>),
}

/// Parse a `build.zig.zon` manifest. Errors name the line.
pub fn parse(src: &str) -> Result<Manifest, String> {
    if src.len() > MAX_MANIFEST_BYTES {
        return Err(format!(
            "build.zig.zon is larger than {MAX_MANIFEST_BYTES} bytes"
        ));
    }
    let mut p = Parser {
        src: src.as_bytes(),
        pos: 0,
    };
    p.skip_ws();
    if p.peek() != Some(b'.') || p.peek_at(1) != Some(b'{') {
        return Err(p.err("the manifest must be a `.{ ... }` literal"));
    }
    p.pos += 2;
    let root = p.aggregate(1)?;
    p.skip_ws();
    if p.pos != p.src.len() {
        return Err(p.err("unexpected text after the manifest"));
    }
    let Value::Struct(fields) = root else {
        return Err("the manifest must be a struct".to_string());
    };
    manifest_from(fields)
}

struct Parser<'a> {
    src: &'a [u8],
    pos: usize,
}

impl Parser<'_> {
    fn err(&self, what: &str) -> String {
        let at = self.pos.min(self.src.len());
        let line = 1 + self.src[..at].iter().filter(|&&b| b == b'\n').count();
        format!("build.zig.zon line {line}: {what}")
    }

    fn peek(&self) -> Option<u8> {
        self.src.get(self.pos).copied()
    }

    fn peek_at(&self, n: usize) -> Option<u8> {
        self.src.get(self.pos + n).copied()
    }

    /// Skip spaces, newlines and `//` comments.
    fn skip_ws(&mut self) {
        loop {
            match self.peek() {
                Some(b' ' | b'\t' | b'\r' | b'\n') => self.pos += 1,
                Some(b'/') if self.peek_at(1) == Some(b'/') => {
                    while self.peek().is_some_and(|c| c != b'\n') {
                        self.pos += 1;
                    }
                }
                _ => break,
            }
        }
    }

    fn eat(&mut self, c: u8) -> bool {
        self.skip_ws();
        if self.peek() == Some(c) {
            self.pos += 1;
            true
        } else {
            false
        }
    }

    fn expect(&mut self, c: u8) -> Result<(), String> {
        if self.eat(c) {
            Ok(())
        } else {
            Err(self.err(&format!("expected `{}`", c as char)))
        }
    }

    /// An identifier, or a quoted `@"..."` one. Called after the leading `.`.
    fn ident(&mut self) -> Result<String, String> {
        if self.peek() == Some(b'@') && self.peek_at(1) == Some(b'"') {
            self.pos += 1;
            return self.string();
        }
        let start = self.pos;
        while self
            .peek()
            .is_some_and(|c| c.is_ascii_alphanumeric() || c == b'_')
        {
            self.pos += 1;
        }
        if self.pos == start {
            return Err(self.err("expected an identifier"));
        }
        Ok(String::from_utf8_lossy(&self.src[start..self.pos]).into_owned())
    }

    /// A double-quoted string with Zig's escapes. Called at the opening quote.
    fn string(&mut self) -> Result<String, String> {
        self.pos += 1;
        let mut out: Vec<u8> = Vec::new();
        loop {
            match self.peek() {
                None => return Err(self.err("unterminated string")),
                Some(b'"') => {
                    self.pos += 1;
                    break;
                }
                Some(b'\n') => return Err(self.err("a newline inside a string")),
                Some(b'\\') => {
                    self.pos += 1;
                    self.escape(&mut out)?;
                }
                Some(c) => {
                    out.push(c);
                    self.pos += 1;
                }
            }
        }
        String::from_utf8(out).map_err(|_| self.err("a string is not UTF-8"))
    }

    fn escape(&mut self, out: &mut Vec<u8>) -> Result<(), String> {
        let c = self.peek().ok_or_else(|| self.err("unterminated escape"))?;
        self.pos += 1;
        let plain = match c {
            b'n' => Some(b'\n'),
            b'r' => Some(b'\r'),
            b't' => Some(b'\t'),
            b'\\' => Some(b'\\'),
            b'\'' => Some(b'\''),
            b'"' => Some(b'"'),
            b'0' => Some(0),
            _ => None,
        };
        if let Some(b) = plain {
            out.push(b);
            return Ok(());
        }
        let ch = match c {
            b'x' => {
                let hex = self.take_hex(2)?;
                char::from_u32(hex).ok_or_else(|| self.err("a bad \\x escape"))?
            }
            b'u' => {
                self.expect(b'{')?;
                let start = self.pos;
                while self.peek().is_some_and(|c| c.is_ascii_hexdigit()) {
                    self.pos += 1;
                }
                let digits = std::str::from_utf8(&self.src[start..self.pos]).unwrap_or("");
                let code =
                    u32::from_str_radix(digits, 16).map_err(|_| self.err("a bad \\u escape"))?;
                self.expect(b'}')?;
                char::from_u32(code).ok_or_else(|| self.err("a bad \\u escape"))?
            }
            _ => return Err(self.err("an unknown escape")),
        };
        let mut buf = [0u8; 4];
        out.extend_from_slice(ch.encode_utf8(&mut buf).as_bytes());
        Ok(())
    }

    fn take_hex(&mut self, n: usize) -> Result<u32, String> {
        let end = self.pos + n;
        let digits = self.src.get(self.pos..end).unwrap_or(&[]);
        let text = std::str::from_utf8(digits).unwrap_or("");
        if text.len() != n || !text.bytes().all(|c| c.is_ascii_hexdigit()) {
            return Err(self.err("a bad hex escape"));
        }
        self.pos = end;
        u32::from_str_radix(text, 16).map_err(|_| self.err("a bad hex escape"))
    }

    fn number(&mut self) -> Result<Value, String> {
        let start = self.pos;
        if self.peek() == Some(b'-') {
            self.pos += 1;
        }
        while self
            .peek()
            .is_some_and(|c| c.is_ascii_alphanumeric() || c == b'_')
        {
            self.pos += 1;
        }
        let text = String::from_utf8_lossy(&self.src[start..self.pos]).into_owned();
        if !text.bytes().any(|c| c.is_ascii_digit()) {
            return Err(self.err("expected a number"));
        }
        Ok(Value::Num(text))
    }

    fn keyword(&mut self) -> Result<Value, String> {
        let start = self.pos;
        while self
            .peek()
            .is_some_and(|c| c.is_ascii_alphanumeric() || c == b'_')
        {
            self.pos += 1;
        }
        match &self.src[start..self.pos] {
            b"true" => Ok(Value::Bool(true)),
            b"false" => Ok(Value::Bool(false)),
            _ => Err(self.err("expected a value")),
        }
    }

    fn value(&mut self, depth: usize) -> Result<Value, String> {
        if depth > MAX_DEPTH {
            return Err(self.err("the manifest nests too deeply"));
        }
        self.skip_ws();
        match self.peek() {
            Some(b'"') => Ok(Value::Str(self.string()?)),
            Some(b'.') => {
                self.pos += 1;
                if self.eat(b'{') {
                    self.aggregate(depth + 1)
                } else {
                    Ok(Value::Enum(self.ident()?))
                }
            }
            Some(b'-' | b'0'..=b'9') => self.number(),
            Some(c) if c.is_ascii_alphabetic() => self.keyword(),
            _ => Err(self.err("expected a value")),
        }
    }

    /// The body of `.{ ... }`, after the `{`. Consumes the closing `}`.
    fn aggregate(&mut self, depth: usize) -> Result<Value, String> {
        if depth > MAX_DEPTH {
            return Err(self.err("the manifest nests too deeply"));
        }
        if self.eat(b'}') {
            return Ok(Value::Struct(Vec::new()));
        }
        if self.is_field_start() {
            let mut fields = Vec::new();
            loop {
                self.expect(b'.')?;
                let name = self.ident()?;
                self.expect(b'=')?;
                fields.push((name, self.value(depth)?));
                if !self.eat(b',') {
                    self.expect(b'}')?;
                    return Ok(Value::Struct(fields));
                }
                if self.eat(b'}') {
                    return Ok(Value::Struct(fields));
                }
            }
        }
        let mut items = Vec::new();
        loop {
            items.push(self.value(depth)?);
            if !self.eat(b',') {
                self.expect(b'}')?;
                return Ok(Value::Tuple(items));
            }
            if self.eat(b'}') {
                return Ok(Value::Tuple(items));
            }
        }
    }

    /// Whether the next item is `.name =`, which makes the literal a struct.
    fn is_field_start(&mut self) -> bool {
        let saved = self.pos;
        self.skip_ws();
        let found = self.peek() == Some(b'.') && {
            self.pos += 1;
            self.ident().is_ok() && {
                self.skip_ws();
                self.peek() == Some(b'=')
            }
        };
        self.pos = saved;
        found
    }
}

fn manifest_from(fields: Vec<(String, Value)>) -> Result<Manifest, String> {
    let mut m = Manifest::default();
    for (key, value) in fields {
        match key.as_str() {
            "name" => m.name = Some(text(&value, "name")?),
            "version" => m.version = Some(text(&value, "version")?),
            "minimum_zig_version" => {
                m.minimum_zig_version = Some(text(&value, "minimum_zig_version")?)
            }
            "fingerprint" => m.fingerprint = Some(fingerprint(&value)?),
            "paths" => m.paths = Some(paths(&value)?),
            "dependencies" => m.dependencies = dependencies(value)?,
            _ => {}
        }
    }
    Ok(m)
}

/// A string, or an enum literal such as `.river` (zig's form for names).
fn text(v: &Value, field: &str) -> Result<String, String> {
    match v {
        Value::Str(s) | Value::Enum(s) => Ok(s.clone()),
        _ => Err(format!(".{field} must be a string")),
    }
}

fn fingerprint(v: &Value) -> Result<u64, String> {
    let Value::Num(raw) = v else {
        return Err(".fingerprint must be a number".to_string());
    };
    let clean: String = raw.chars().filter(|&c| c != '_').collect();
    let parsed = match clean.strip_prefix("0x") {
        Some(hex) => u64::from_str_radix(hex, 16),
        None => clean.parse::<u64>(),
    };
    parsed.map_err(|_| format!(".fingerprint `{raw}` is not a 64-bit number"))
}

/// `.paths = .{ "src", "build.zig" }`. An empty tuple is refused, as zig 0.15.2
/// refuses it: it would keep nothing, which zig does not accept.
fn paths(v: &Value) -> Result<Vec<String>, String> {
    let items = match v {
        Value::Tuple(items) => items.as_slice(),
        Value::Struct(fields) if fields.is_empty() => &[],
        _ => return Err(".paths must be a tuple of strings".to_string()),
    };
    if items.is_empty() {
        return Err("`.paths = .{}` lists no path, which zig 0.15.2 refuses".to_string());
    }
    items
        .iter()
        .map(|item| match item {
            Value::Str(s) => Ok(s.clone()),
            _ => Err(".paths must be a tuple of strings".to_string()),
        })
        .collect()
}

fn dependencies(v: Value) -> Result<Vec<Dependency>, String> {
    let fields = match v {
        Value::Struct(fields) => fields,
        _ => return Err(".dependencies must be a struct".to_string()),
    };
    let mut out: Vec<Dependency> = Vec::new();
    for (name, value) in fields {
        if out.iter().any(|d| d.name == name) {
            return Err(format!("dependency `{name}` is listed twice"));
        }
        out.push(dependency(name, value)?);
    }
    Ok(out)
}

fn dependency(name: String, value: Value) -> Result<Dependency, String> {
    let Value::Struct(fields) = value else {
        return Err(format!("dependency `{name}` must be a struct"));
    };
    let mut dep = Dependency {
        name,
        url: None,
        hash: None,
        path: None,
        lazy: false,
    };
    for (key, v) in fields {
        match key.as_str() {
            "url" => dep.url = Some(text(&v, "url")?),
            "hash" => dep.hash = Some(text(&v, "hash")?),
            "path" => dep.path = Some(text(&v, "path")?),
            "lazy" => {
                let Value::Bool(b) = v else {
                    return Err(format!("dependency `{}`: .lazy must be a bool", dep.name));
                };
                dep.lazy = b;
            }
            _ => {}
        }
    }
    match (&dep.url, &dep.path) {
        (Some(_), Some(_)) => Err(format!(
            "dependency `{}` has both a url and a path",
            dep.name
        )),
        (None, None) => Err(format!(
            "dependency `{}` has neither a url nor a path",
            dep.name
        )),
        _ => Ok(dep),
    }
}
