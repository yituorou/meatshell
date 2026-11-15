//! Argument-line splitting shared by custom local shells (#538) and the SFTP
//! external editor (#553). Compiled in both the desktop and headless builds.

/// Split a user-typed argument line into arguments: whitespace separates,
/// `"…"` or `'…'` group (quotes removed), and backslashes are literal so
/// Windows paths need no escaping.
pub(crate) fn split_args(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut in_word = false;
    let mut quote: Option<char> = None;
    for c in line.chars() {
        match quote {
            Some(q) if c == q => quote = None,
            Some(_) => cur.push(c),
            None if c == '"' || c == '\'' => {
                quote = Some(c);
                in_word = true;
            }
            None if c.is_whitespace() => {
                if in_word {
                    out.push(std::mem::take(&mut cur));
                    in_word = false;
                }
            }
            None => {
                cur.push(c);
                in_word = true;
            }
        }
    }
    if in_word {
        out.push(cur);
    }
    out
}
