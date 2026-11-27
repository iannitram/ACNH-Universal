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

/// Drift check (issue #402). The header is generated into `OUT_DIR` on every build and is not
/// committed, so it cannot be stale against the Rust. What can drift is the C++ side: each
/// wrapper in `runtime/src/*_wrapper.cpp` hand-declares the `acnh_*` functions it calls. Each
/// such declaration must match the generated header (same name, return type and parameter types;
/// parameter names are free). If this fails, a Rust signature changed: update the wrapper's
/// declaration (or the Rust) so both say the same, then rerun `cargo test -p acnh-runtime --test
/// header`.
#[test]
fn the_wrappers_hand_declared_prototypes_match_the_generated_header() {
    let header_path = std::path::Path::new(env!("OUT_DIR")).join("include/acnh_runtime.h");
    let header = std::fs::read_to_string(&header_path).unwrap();
    let generated = prototypes(&header);
    assert!(
        generated.len() > 200,
        "parsed {} header prototypes",
        generated.len()
    );

    let wrappers = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../runtime/src");
    let mut checked = 0;
    let mut problems = Vec::new();
    let mut files: Vec<_> = std::fs::read_dir(&wrappers)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.to_string_lossy().ends_with("_wrapper.cpp"))
        .collect();
    files.sort();
    for file in files {
        let name = file.file_name().unwrap().to_string_lossy().into_owned();
        for (symbol, sig) in prototypes(&std::fs::read_to_string(&file).unwrap()) {
            checked += 1;
            match generated.iter().find(|(s, _)| *s == symbol) {
                None => problems.push(format!("{name}: {symbol} is not in the generated header")),
                Some((_, want)) if *want != sig => problems.push(format!(
                    "{name}: {symbol} is declared `{sig}`, the header has `{want}`"
                )),
                Some(_) => {}
            }
        }
    }
    assert!(checked > 100, "only {checked} wrapper prototypes found");
    assert!(
        problems.is_empty(),
        "the C++ wrappers drifted from the generated header:\n{}",
        problems.join("\n")
    );
}

/// Every top-level `ret acnh_name(params);` statement in `text`, as (name, normalised signature):
/// whitespace dropped, `(void)` as `()`, parameter names removed.
fn prototypes(text: &str) -> Vec<(String, String)> {
    let mut found = Vec::new();
    let mut pending = String::new();
    for line in text.lines() {
        if pending.is_empty() {
            let starts_decl = line.chars().next().is_some_and(|c| c.is_ascii_lowercase())
                && !line.starts_with("typedef")
                && !line.starts_with("return")
                && !line.starts_with("static")
                && !line.starts_with("extern")
                && !line.starts_with("using")
                && !line.starts_with("namespace")
                && !line.starts_with("struct")
                && !line.starts_with("inline");
            if !starts_decl || !line.contains("acnh_") {
                continue;
            }
        }
        pending.push_str(line);
        pending.push(' ');
        if line.contains('{') || line.contains('=') {
            pending.clear();
        } else if line.trim_end().ends_with(';') {
            if let Some(p) = normalise(&pending) {
                found.push(p);
            }
            pending.clear();
        }
    }
    found
}

fn normalise(stmt: &str) -> Option<(String, String)> {
    let open = stmt.find('(')?;
    let close = stmt.rfind(')')?;
    let head = stmt[..open].trim_end();
    let split = head.rfind(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))? + 1;
    let (ret, name) = (head[..split].trim(), &head[split..]);
    if !name.starts_with("acnh_") || ret.is_empty() || ret.contains('(') {
        return None;
    }
    let params: Vec<String> = split_params(&stmt[open + 1..close])
        .iter()
        .map(|p| p.trim())
        .filter(|p| !p.is_empty() && *p != "void")
        .map(|p| {
            // The wrappers name callback types (`AcnhSavesStringCb`, `AcnhHleFn`) where the header spells the
            // function pointer out: both are a callback.
            if p.contains('(')
                || p.split_whitespace()
                    .any(|w| w.ends_with("Cb") || w.ends_with("Fn"))
            {
                return "callback".to_string();
            }
            let p = p.replace('*', " * ");
            let mut words: Vec<&str> = p.split_whitespace().filter(|w| *w != "struct").collect();
            // `type name`: drop the trailing name (a lone word is a type with no name).
            if words.len() > 1 && words.last().is_some_and(|w| *w != "*") {
                words.pop();
            }
            // `size_t` is `uintptr_t` on every target the player builds for.
            words
                .concat()
                .replace("size_t", "uintptr_t")
                .replace("uintptruintptr_t", "uintptr_t")
        })
        .collect();
    let ret: String = ret
        .split_whitespace()
        .filter(|w| *w != "struct")
        .collect::<String>()
        .replace("size_t", "uintptr_t")
        .replace("uintptruintptr_t", "uintptr_t");
    Some((name.to_string(), format!("{ret}({})", params.join(","))))
}

/// Split at the commas outside parentheses (a function-pointer parameter has its own list).
fn split_params(params: &str) -> Vec<&str> {
    let (mut out, mut depth, mut start) = (Vec::new(), 0, 0);
    for (i, c) in params.char_indices() {
        match c {
            '(' => depth += 1,
            ')' => depth -= 1,
            ',' if depth == 0 => {
                out.push(&params[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    out.push(&params[start..]);
    out
}
