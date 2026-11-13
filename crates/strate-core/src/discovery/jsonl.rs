use serde_json::Value;

/// Parses one JSONL line. `None` for a blank line; an error for anything
/// that is not one JSON value (malformed, truncated, non-UTF-8).
pub(crate) fn parse_line(line: &[u8]) -> Option<serde_json::Result<Value>> {
    if line.iter().all(u8::is_ascii_whitespace) {
        None
    } else {
        Some(serde_json::from_slice(line))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_line_classifies_blank_valid_and_malformed() {
        assert!(parse_line(b"  \r\n").is_none());
        let value = parse_line(b"{\"a\":1}\r\n")
            .expect("not blank")
            .expect("valid");
        assert_eq!(value["a"], 1);
        assert!(parse_line(b"{\"a\":\n").expect("not blank").is_err());
        assert!(
            parse_line(b"{\"a\":\"\xff\xfe\"}\n")
                .expect("not blank")
                .is_err()
        );
    }
}
