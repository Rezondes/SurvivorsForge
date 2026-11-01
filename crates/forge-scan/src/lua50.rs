//! Lua 5.0, which the client runs, reads long brackets differently from the 5.1 grammar of
//! full_moon: `[[` nests, and there is no `[=[`, so `--[=[` is only a line comment. This rewrites
//! the brackets into 5.1 form with the same meaning and the same lines, so the scanner reads the
//! code the client runs.
//!
//! The source is cut only at bytes of ASCII brackets, dashes and quotes, never inside a
//! character, so names in any script are safe.

pub fn to_lua51(source: &str) -> String {
    // A string continued with `\` at the end of a line saved with CRLF: full_moon takes only `\` and LF.
    let source = &source.replace("\\\r\n", "\\\n");
    let b = source.as_bytes();
    let mut out = String::with_capacity(source.len());
    let mut copied = 0;
    let mut i = 0;
    while i < b.len() {
        i = match b[i] {
            b'"' | b'\'' => short_string_end(b, i),
            b'-' if b.get(i + 1) == Some(&b'-') => {
                if b[i + 2..].starts_with(b"[[") {
                    long_bracket(source, i + 2, &mut out, &mut copied)
                } else {
                    // `--[=[` would open a block comment for full_moon; a space keeps it a line comment.
                    if b.get(i + 2) == Some(&b'[') {
                        out.push_str(&source[copied..i + 2]);
                        out.push(' ');
                        copied = i + 2;
                    }
                    b[i..].iter().position(|&c| c == b'\n').map_or(b.len(), |n| i + n)
                }
            }
            b'[' if b.get(i + 1) == Some(&b'[') => long_bracket(source, i, &mut out, &mut copied),
            _ => i + 1,
        };
    }
    out.push_str(&source[copied..]);
    out
}

/// The index after the quoted string that starts at `open`.
fn short_string_end(b: &[u8], open: usize) -> usize {
    let mut j = open + 1;
    while j < b.len() && b[j] != b[open] && b[j] != b'\n' {
        j += if b[j] == b'\\' { 2 } else { 1 };
    }
    (j + 1).min(b.len())
}

/// The 5.0 long string or comment whose `[[` is at `open`; a nested one is written out with a
/// level that its text does not contain. Returns the index after it.
fn long_bracket(source: &str, open: usize, out: &mut String, copied: &mut usize) -> usize {
    let b = source.as_bytes();
    let (mut depth, mut nested, mut j) = (1, false, open + 2);
    while j < b.len() && depth > 0 {
        if b[j..].starts_with(b"[[") {
            depth += 1;
            nested = true;
            j += 2;
        } else if b[j..].starts_with(b"]]") {
            depth -= 1;
            j += 2;
        } else {
            j += 1;
        }
    }
    // Unclosed: the client cannot load it either.
    if depth > 0 || !nested {
        return j;
    }
    let content = &source[open + 2..j - 2];
    let mut level = "=".to_string();
    while format!("{content}]{level}]").find(&format!("]{level}]")) != Some(content.len()) {
        level.push('=');
    }
    out.push_str(&source[*copied..open]);
    out.push_str(&format!("[{level}[{content}]{level}]"));
    *copied = j;
    j
}

#[cfg(test)]
mod tests {
    use super::to_lua51;

    #[test]
    fn rewrites_only_what_5_0_reads_differently() {
        assert_eq!(to_lua51("a = [[x]]"), "a = [[x]]");
        assert_eq!(to_lua51("a = [[x [[y]] z]]"), "a = [=[x [[y]] z]=]");
        assert_eq!(to_lua51("--[[a [[b]] ]=]c]]"), "--[==[a [[b]] ]=]c]==]");
        assert_eq!(to_lua51("--[==[\nx()"), "-- [==[\nx()");
        assert_eq!(to_lua51("s = '[[' .. \"--[=[\""), "s = '[[' .. \"--[=[\"");
    }
}
