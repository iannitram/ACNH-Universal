// The generated fonts are checked in as hexadecimal text. A checkout may
// use LF or CRLF, so discard ASCII whitespace before decoding each line.
pub fn decode_hex(text: &str) -> Vec<u8> {
    text.split_ascii_whitespace()
        .flat_map(|line| {
            assert!(line.len().is_multiple_of(2));
            (0..line.len())
                .step_by(2)
                .map(|i| u8::from_str_radix(&line[i..i + 2], 16).unwrap())
        })
        .collect()
}
