//! The player's runtime ABI number (`nx::kRuntimeAbi`, `runtime/include/nx/runtime.h`) and the one
//! this crate implements must be the same, because `acnh_loader_boot` stops the boot otherwise.

#[test]
fn the_player_header_and_the_crate_agree_on_the_runtime_abi() {
    let path =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../runtime/include/nx/runtime.h");
    let header = std::fs::read_to_string(&path).unwrap();
    let line = header
        .lines()
        .find(|l| l.contains("kRuntimeAbi") && l.contains('='))
        .expect("runtime.h declares kRuntimeAbi");
    let value: u32 = line
        .split('=')
        .nth(1)
        .and_then(|v| v.trim().trim_end_matches(';').trim().parse().ok())
        .expect("kRuntimeAbi is a plain number");
    assert_eq!(value, acnh_runtime::RUNTIME_ABI_VERSION);
    assert_eq!(acnh_runtime::acnh_runtime_abi_version(), value);
}
