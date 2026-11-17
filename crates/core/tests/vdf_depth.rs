// Nesting bound for vdf::read: untrusted bytes must not recurse without limit.
use acnh_core::vdf::read;
use acnh_core::Error;

fn nested(levels: usize) -> Vec<u8> {
    // Each level is a map with key "a": 0x00 'a' 0x00, closed by 0x08 at the end.
    let mut b = Vec::with_capacity(levels * 3 + levels + 1);
    for _ in 0..levels {
        b.extend_from_slice(b"\x00a\x00");
    }
    b.extend(std::iter::repeat_n(0x08, levels + 1));
    b
}

#[test]
fn shallow_nesting_still_reads() {
    assert!(read(&nested(8)).is_ok());
}

#[test]
fn deep_nesting_is_an_error_not_a_stack_overflow() {
    assert_eq!(
        read(&nested(100_000)),
        Err(Error::Steam("shortcuts.vdf is nested too deep".to_string()))
    );
}
