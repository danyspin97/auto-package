/// `text` with control characters removed, keeping newlines and tabs.
///
/// Text printed to the terminal can come from upstream: a build log line or
/// a package's own message. An escape sequence in it (a title change, a
/// cursor move, OSC 52 clipboard writes) would act on the user's terminal, so
/// C0, DEL and C1 controls, ESC included, are dropped before printing.
pub fn printable(text: &str) -> std::borrow::Cow<'_, str> {
    if !text.chars().any(is_stripped_control) {
        return std::borrow::Cow::Borrowed(text);
    }
    std::borrow::Cow::Owned(strip_controls(text, false))
}

fn is_stripped_control(c: char) -> bool {
    c.is_control() && c != '\n' && c != '\t'
}

/// [`printable`], except that SGR colour sequences (`ESC [ <digits> m`) are
/// kept. The human sink colours its own output with them; upstream text can
/// still set a colour, which is harmless, but cannot move the cursor, set a
/// title or write the clipboard.
pub(super) fn strip_controls(text: &str, keep_sgr: bool) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            match chars.peek() {
                // CSI: parameters, intermediates, one final byte. Dropped whole,
                // so `ESC [ 2 K` leaves nothing behind; only a plain SGR colour
                // is kept, and only for the human sink.
                Some('[') => {
                    chars.next();
                    let mut params = String::new();
                    while let Some(&p) = chars.peek() {
                        if !('\u{20}'..='\u{3f}').contains(&p) {
                            break;
                        }
                        params.push(p);
                        chars.next();
                    }
                    if let Some(fin) = chars.next_if(|f| ('\u{40}'..='\u{7e}').contains(f))
                        && keep_sgr
                        && fin == 'm'
                        && params.chars().all(|p| p.is_ascii_digit() || p == ';')
                    {
                        out.push_str("\u{1b}[");
                        out.push_str(&params);
                        out.push('m');
                    }
                }
                // OSC (title, clipboard): up to BEL or ESC \.
                Some(']') => {
                    chars.next();
                    while let Some(o) = chars.next() {
                        if o == '\u{7}' {
                            break;
                        }
                        if o == '\u{1b}' {
                            if chars.peek() == Some(&'\\') {
                                chars.next();
                            }
                            break;
                        }
                    }
                }
                _ => {}
            }
            continue;
        }
        if !is_stripped_control(c) {
            out.push(c);
        }
    }
    out
}
