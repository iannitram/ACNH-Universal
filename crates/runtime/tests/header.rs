//! The generated `acnh_runtime.h` is what the C++ wrappers compile against. A `pub type X =
//! extern "C" fn(..)` used as `Option<X>` makes cbindgen declare an `Option_X` struct instead of
//! the `X` function-pointer typedef the wrappers use, so callback aliases must be nullable
//! themselves (`pub type X = Option<extern "C" fn(..)>`).

#[test]
fn the_header_has_plain_callback_typedefs_and_no_option_structs() {
    let path = std::path::Path::new(env!("OUT_DIR")).join("include/acnh_runtime.h");
    let header = std::fs::read_to_string(&path).unwrap();
    let wrapped: Vec<&str> = header.lines().filter(|l| l.contains("Option_")).collect();
    assert!(
        wrapped.is_empty(),
        "cbindgen wrapped an Option: {wrapped:?}"
    );
    for typedef in [
        "AcnhFilesStringCb",
        "AcnhFilesBytesCb",
        "AcnhSavesStringCb",
        "AcnhSavesUserCb",
        "AcnhConfigEntryCb",
    ] {
        assert!(
            header.contains(&format!("(*{typedef})(")),
            "no function-pointer typedef for {typedef}"
        );
    }
}

#[test]
fn the_aligned_vector_register_type_is_a_complete_struct() {
    // `AcnhCpu` embeds it by value: an opaque forward declaration cannot be compiled by C++.
    let path = std::path::Path::new(env!("OUT_DIR")).join("include/acnh_runtime.h");
    let header = std::fs::read_to_string(&path).unwrap();
    assert!(header.contains("} AcnhV128;"), "AcnhV128 is opaque");
}
