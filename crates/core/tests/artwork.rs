// Locked tests for the Steam library images: file names, sizes, folders (M2).
use acnh_core::artwork::{file_name, grid_dir, paths, size, Art, ALL};
use std::path::Path;

#[test]
fn all_is_in_order() {
    assert_eq!(
        ALL,
        [Art::Grid, Art::WideGrid, Art::Hero, Art::Logo, Art::Icon]
    );
}

#[test]
fn file_names() {
    assert_eq!(file_name(Art::Grid, 12345), "12345p.png");
    assert_eq!(file_name(Art::WideGrid, 12345), "12345.png");
    assert_eq!(file_name(Art::Hero, 12345), "12345_hero.png");
    assert_eq!(file_name(Art::Logo, 12345), "12345_logo.png");
    assert_eq!(file_name(Art::Icon, 12345), "12345_icon.png");
}

#[test]
fn app_id_is_plain_decimal() {
    assert_eq!(file_name(Art::Grid, 0), "0p.png");
    assert_eq!(file_name(Art::Icon, 4294967295), "4294967295_icon.png");
}

#[test]
fn sizes() {
    assert_eq!(size(Art::Grid), (600, 900));
    assert_eq!(size(Art::WideGrid), (920, 430));
    assert_eq!(size(Art::Hero), (1920, 620));
    assert_eq!(size(Art::Logo), (1280, 720));
    assert_eq!(size(Art::Icon), (256, 256));
}

#[test]
fn grid_dir_is_config_grid() {
    let user = Path::new("userdata").join("1001");
    assert_eq!(
        grid_dir(&user),
        Path::new("userdata")
            .join("1001")
            .join("config")
            .join("grid")
    );
}

#[test]
fn paths_in_all_order() {
    let user = Path::new("u").join("7");
    let g = Path::new("u").join("7").join("config").join("grid");
    assert_eq!(
        paths(&user, 99),
        vec![
            (Art::Grid, g.join("99p.png")),
            (Art::WideGrid, g.join("99.png")),
            (Art::Hero, g.join("99_hero.png")),
            (Art::Logo, g.join("99_logo.png")),
            (Art::Icon, g.join("99_icon.png")),
        ]
    );
}
