use std::io::{self, BufRead};

use serde_json::Value;

/// Streams JSONL records to `on_record`, one parsed value per non-blank
/// line. Lines that fail to parse (malformed, truncated, non-UTF-8) are
/// skipped and returned as 1-based line numbers; they never abort the read.
pub(crate) fn for_each_record(
    mut reader: impl BufRead,
    mut on_record: impl FnMut(Value),
) -> io::Result<Vec<usize>> {
    let mut bad = Vec::new();
    let mut line = Vec::new();
    let mut line_no = 0;
    loop {
        line.clear();
        if reader.read_until(b'\n', &mut line)? == 0 {
            return Ok(bad);
        }
        line_no += 1;
        if line.iter().all(u8::is_ascii_whitespace) {
            continue;
        }
        match serde_json::from_slice(&line) {
            Ok(value) => on_record(value),
            Err(_) => bad.push(line_no),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn collect(input: &[u8]) -> (Vec<Value>, Vec<usize>) {
        let mut records = Vec::new();
        let bad = for_each_record(input, |v| records.push(v)).expect("in-memory read");
        (records, bad)
    }

    #[test]
    fn parses_each_line_and_skips_blank_lines() {
        let (records, bad) = collect(b"{\"a\":1}\n\n{\"a\":2}\n");
        assert_eq!(records.len(), 2);
        assert_eq!(records[1]["a"], 2);
        assert!(bad.is_empty());
    }

    #[test]
    fn malformed_line_is_reported_and_reading_continues() {
        let (records, bad) = collect(b"{\"a\":1}\n{\"a\": \n{\"a\":3}\n");
        assert_eq!(records.len(), 2);
        assert_eq!(records[1]["a"], 3);
        assert_eq!(bad, vec![2]);
    }

    #[test]
    fn truncated_final_line_without_newline_is_reported() {
        let (records, bad) = collect(b"{\"a\":1}\n{\"a\":2,\"b\":\"tru");
        assert_eq!(records.len(), 1);
        assert_eq!(bad, vec![2]);
    }

    #[test]
    fn crlf_line_endings_are_tolerated() {
        let (records, bad) = collect(b"{\"a\":1}\r\n{\"a\":2}\r\n");
        assert_eq!(records.len(), 2);
        assert!(bad.is_empty());
    }

    #[test]
    fn non_utf8_line_is_reported_not_panicked() {
        let (records, bad) = collect(b"{\"a\":\"\xff\xfe\"}\n{\"a\":2}\n");
        assert_eq!(records.len(), 1);
        assert_eq!(bad, vec![1]);
    }
}
