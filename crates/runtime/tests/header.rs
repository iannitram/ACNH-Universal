//! The generated `acnh_runtime.h` is what the C++ wrappers compile against. A `pub type X =
//! extern "C" fn(..)` used as `Option<X>` makes cbindgen declare an opaque `Option_X` struct
//! where the wrappers pass a plain function pointer, so callbacks cross the boundary as a spelled
//! out `Option<extern "C" fn(..)>` (the wrappers declare their own typedefs for them).

#[test]
fn the_header_wraps_no_callback_option_in_a_struct() {
    let path = std::path::Path::new(env!("OUT_DIR")).join("include/acnh_runtime.h");
    let header = std::fs::read_to_string(&path).unwrap();
    let wrapped: Vec<&str> = header.lines().filter(|l| l.contains("Option_")).collect();
    assert!(
        wrapped.is_empty(),
        "cbindgen wrapped an Option: {wrapped:?}"
    );
}

#[test]
fn the_aligned_vector_register_type_is_a_complete_struct() {
    // `AcnhCpu` embeds it by value: an opaque forward declaration cannot be compiled by C++.
    let path = std::path::Path::new(env!("OUT_DIR")).join("include/acnh_runtime.h");
    let header = std::fs::read_to_string(&path).unwrap();
    assert!(header.contains("} AcnhV128;"), "AcnhV128 is opaque");
}
