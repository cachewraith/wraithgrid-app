//! Launch args as typed in the New pane dialog (`src/shared/args.ts`): split the way a POSIX
//! shell would for plain words and quotes, with no expansion; the result goes straight to spawn.

pub fn parse_args(input: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut has = false;
    let mut quote: Option<char> = None;
    let mut chars = input.chars().peekable();
    while let Some(ch) = chars.next() {
        if let Some(q) = quote {
            if ch == q {
                quote = None;
            } else if ch == '\\' && q == '"' && chars.peek().is_some() {
                cur.push(chars.next().unwrap());
            } else {
                cur.push(ch);
            }
            continue;
        }
        if ch == '"' || ch == '\'' {
            quote = Some(ch);
            has = true;
        } else if ch == '\\' && chars.peek().is_some() {
            cur.push(chars.next().unwrap());
            has = true;
        } else if ch.is_whitespace() {
            if has {
                out.push(std::mem::take(&mut cur));
            }
            cur.clear();
            has = false;
        } else {
            cur.push(ch);
            has = true;
        }
    }
    if has {
        out.push(cur);
    }
    out
}

/// Renders argv for display, quoting only where needed.
pub fn format_args(args: &[String]) -> String {
    let plain = |a: &str| {
        !a.is_empty()
            && a.chars()
                .all(|c| c.is_alphanumeric() || c == '_' || "@%+=:,./-".contains(c))
    };
    args.iter()
        .map(|a| {
            if plain(a) {
                a.clone()
            } else {
                format!("'{}'", a.replace('\'', "'\\''"))
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_like_a_shell() {
        assert_eq!(
            parse_args(r#"--model "claude x" -p 'a b' c\ d"#),
            vec!["--model", "claude x", "-p", "a b", "c d"]
        );
        assert_eq!(parse_args(r#"'' "x\"y""#), vec!["", "x\"y"]);
        assert!(parse_args("   ").is_empty());
    }

    #[test]
    fn round_trips() {
        let args: Vec<String> = ["a", "b c", "it's", "", "--x=1"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(parse_args(&format_args(&args)), args);
        assert_eq!(format_args(&args[..1]), "a");
    }
}
