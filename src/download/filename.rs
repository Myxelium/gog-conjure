use url::Url;

/// Extract the exact GOG CDN filename from a resolved downlink URL.
///
/// Matches Lutris: prefer the `path` query parameter basename, otherwise the
/// percent-decoded URL path basename. This is the real installer name
/// (`setup_game_1.2.3.exe`), not the gameDetails display title.
pub fn filename_from_cdn_url(cdn_url: &str) -> Option<String> {
    let parsed = Url::parse(cdn_url).ok()?;

    for (key, value) in parsed.query_pairs() {
        if key == "path" {
            if let Some(name) = basename_if_usable(&value) {
                return Some(name);
            }
        }
    }

    // `Url::path_segments` leaves percent-escapes intact; GOG names use the
    // decoded form (`(64bit)`, not `%2864bit%29`) — same as Lutris `unquote`.
    let name = parsed.path_segments()?.next_back()?;
    let decoded = percent_decode(name);
    basename_if_usable(&decoded)
}

/// Parse `Content-Disposition` for an exact filename (RFC 5987 / simple forms).
pub fn filename_from_content_disposition(header: &str) -> Option<String> {
    // filename*=UTF-8''encoded-name
    for part in header.split(';') {
        let part = part.trim();
        let Some(rest) = part
            .strip_prefix("filename*")
            .or_else(|| part.strip_prefix("FILENAME*"))
        else {
            continue;
        };
        let rest = rest.trim().trim_start_matches('=').trim();
        if let Some(encoded) = rest.split("''").nth(1) {
            let decoded = percent_decode(encoded);
            if let Some(name) = basename_if_usable(&decoded) {
                return Some(name);
            }
        }
    }

    // filename="name" or filename=name
    for part in header.split(';') {
        let part = part.trim();
        let Some(rest) = part
            .strip_prefix("filename")
            .or_else(|| part.strip_prefix("FILENAME"))
        else {
            continue;
        };
        // Skip filename*
        if rest.starts_with('*') {
            continue;
        }
        let rest = rest.trim().trim_start_matches('=').trim();
        let unquoted = trim_quotes(rest);
        if let Some(name) = basename_if_usable(unquoted) {
            return Some(name);
        }
    }

    None
}

/// Keep the GOG name byte-for-byte, only rejecting path traversal / empty names.
fn basename_if_usable(value: &str) -> Option<String> {
    let name = value
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(value)
        .trim();
    if !is_usable_filename(name) {
        return None;
    }
    Some(name.to_string())
}

fn is_usable_filename(name: &str) -> bool {
    !name.is_empty()
        && name != "."
        && name != ".."
        && !name.contains('\0')
        && !name.contains('/')
        && !name.contains('\\')
}

fn trim_quotes(s: &str) -> &str {
    s.strip_prefix('"')
        .and_then(|s| s.strip_suffix('"'))
        .unwrap_or(s)
}

/// Percent-decode; on invalid sequences, return the input unchanged.
fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => match (from_hex(bytes[i + 1]), from_hex(bytes[i + 2])) {
                (Some(h), Some(l)) => {
                    out.push((h << 4) | l);
                    i += 3;
                }
                _ => {
                    out.push(bytes[i]);
                    i += 1;
                }
            },
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8(out).unwrap_or_else(|_| s.to_string())
}

fn from_hex(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_path_basename_percent_decoded() {
        let url = "https://gog-cdn-fastly.gog.com/token=nva=1~dirs=6~token=abc/secure/offline/1/setup_cyberpunk_2077_2.13_%2864bit%29_%2876205%29-27.bin";
        assert_eq!(
            filename_from_cdn_url(url).as_deref(),
            Some("setup_cyberpunk_2077_2.13_(64bit)_(76205)-27.bin")
        );
    }

    #[test]
    fn from_path_query_param() {
        let url = "https://cdn.gog.com/deliver?path=%2Foffline%2Fsetup_game_1.0.0.2.exe&token=x";
        assert_eq!(
            filename_from_cdn_url(url).as_deref(),
            Some("setup_game_1.0.0.2.exe")
        );
    }

    #[test]
    fn content_disposition_quoted() {
        assert_eq!(
            filename_from_content_disposition(
                r#"attachment; filename="setup_example_2.0.0.1.exe""#
            )
            .as_deref(),
            Some("setup_example_2.0.0.1.exe")
        );
    }

    #[test]
    fn content_disposition_rfc5987() {
        assert_eq!(
            filename_from_content_disposition(
                "attachment; filename*=UTF-8''setup_game_%281%29.exe"
            )
            .as_deref(),
            Some("setup_game_(1).exe")
        );
    }

    #[test]
    fn strips_path_components_keeps_basename() {
        assert_eq!(basename_if_usable("../evil.exe").as_deref(), Some("evil.exe"));
        assert!(!is_usable_filename(".."));
        assert!(!is_usable_filename(""));
        assert!(!is_usable_filename("a/b.exe"));
    }
}
