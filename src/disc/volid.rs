/// ISO 9660 volume identifier max length.
pub const VOLID_MAX_LEN: usize = 32;

/// Sanitize a string into a valid ISO `volid`: uppercase `[A-Z0-9_]`, max 32 chars.
pub fn sanitize_volid(raw: &str) -> String {
    let mut out = String::with_capacity(VOLID_MAX_LEN);
    let mut last_underscore = false;
    for ch in raw.chars() {
        if out.len() >= VOLID_MAX_LEN {
            break;
        }
        let mapped = match ch {
            'a'..='z' => Some(ch.to_ascii_uppercase()),
            'A'..='Z' | '0'..='9' => Some(ch),
            ' ' | '-' | '.' | '/' | '\\' | ':' | '+' | '\'' | '"' => Some('_'),
            '_' => Some('_'),
            _ => None,
        };
        if let Some(c) = mapped {
            if c == '_' {
                if last_underscore || out.is_empty() {
                    continue;
                }
                last_underscore = true;
            } else {
                last_underscore = false;
            }
            out.push(c);
        }
    }
    while out.ends_with('_') {
        out.pop();
    }
    out
}

/// Truncate an already-sanitized volid to `max` chars without trailing underscore.
pub fn truncate_volid(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let mut t: String = s.chars().take(max).collect();
    while t.ends_with('_') {
        t.pop();
    }
    t
}

/// Auto volume label from game titles on a disc.
///
/// - One title: sanitized + truncated (optional part suffix `_N` for split discs).
/// - Multiple: join with `_` until the 32-char budget is exhausted.
/// - Empty fallback: `GOG_DISC_{nn}` (1-based disc number).
pub fn auto_volid(titles: &[String], disc_index_1based: usize, part_suffix: Option<u32>) -> String {
    let sanitized: Vec<String> = titles
        .iter()
        .map(|t| sanitize_volid(t))
        .filter(|t| !t.is_empty())
        .collect();

    if sanitized.is_empty() {
        return format!("GOG_DISC_{disc_index_1based:02}");
    }

    if sanitized.len() == 1 {
        let base = &sanitized[0];
        if let Some(part) = part_suffix.filter(|&p| p >= 2) {
            let suffix = format!("_{part}");
            let budget = VOLID_MAX_LEN.saturating_sub(suffix.len());
            let truncated = truncate_volid(base, budget);
            if truncated.is_empty() {
                return format!("GOG_DISC_{disc_index_1based:02}");
            }
            return format!("{truncated}{suffix}");
        }
        let t = truncate_volid(base, VOLID_MAX_LEN);
        return if t.is_empty() {
            format!("GOG_DISC_{disc_index_1based:02}")
        } else {
            t
        };
    }

    let mut out = String::new();
    for (i, title) in sanitized.iter().enumerate() {
        let sep = if i == 0 { "" } else { "_" };
        let remaining = VOLID_MAX_LEN.saturating_sub(out.len() + sep.len());
        if remaining == 0 {
            break;
        }
        let piece = truncate_volid(title, remaining);
        if piece.is_empty() {
            break;
        }
        out.push_str(sep);
        out.push_str(&piece);
    }

    if out.is_empty() {
        format!("GOG_DISC_{disc_index_1based:02}")
    } else {
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_basic() {
        assert_eq!(sanitize_volid("The Witcher 3"), "THE_WITCHER_3");
        assert_eq!(sanitize_volid("game!!!"), "GAME");
        assert_eq!(sanitize_volid(""), "");
    }

    #[test]
    fn sanitize_truncates() {
        let long = "A".repeat(40);
        assert_eq!(sanitize_volid(&long).len(), VOLID_MAX_LEN);
    }

    #[test]
    fn auto_single_with_part() {
        let v = auto_volid(&["Cyberpunk 2077".into()], 1, Some(2));
        assert!(v.ends_with("_2"));
        assert!(v.len() <= VOLID_MAX_LEN);
        assert!(v.starts_with("CYBERPUNK"));
    }

    #[test]
    fn auto_multi_drops_overflow() {
        let titles = vec![
            "Very Long Game Title Alpha".into(),
            "Very Long Game Title Beta".into(),
            "Very Long Game Title Gamma".into(),
        ];
        let v = auto_volid(&titles, 1, None);
        assert!(v.len() <= VOLID_MAX_LEN);
        assert!(!v.is_empty());
    }

    #[test]
    fn auto_fallback() {
        assert_eq!(auto_volid(&[], 3, None), "GOG_DISC_03");
        assert_eq!(auto_volid(&[String::new()], 1, None), "GOG_DISC_01");
    }
}
