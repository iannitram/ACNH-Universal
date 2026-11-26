use super::*;

#[test]
fn env_name_wins_and_skips_the_command() {
    let name = device_name_from(
        Some("deck".into()),
        || panic!("command not needed"),
        || panic!("no suffix"),
    );
    assert_eq!(name, "deck");
}

#[test]
fn falls_back_to_the_host_name_command_when_env_is_missing_or_blank() {
    for env in [None, Some("  ".to_string())] {
        let name = device_name_from(env, || Some("macbook\n".into()), || panic!("no suffix"));
        assert_eq!(name, "macbook");
    }
}

#[test]
fn unresolvable_name_gets_a_suffix_so_machines_differ() {
    let a = device_name_from(None, || None, || "aaaaaa".into());
    let b = device_name_from(None, || Some(String::new()), || "bbbbbb".into());
    assert_eq!(a, "device-aaaaaa");
    assert_ne!(a, b);
}

#[test]
fn random_suffixes_are_six_hex_digits_and_vary() {
    let all: std::collections::HashSet<String> = (0..16).map(|_| random_suffix()).collect();
    assert!(all.len() > 1);
    assert!(all
        .iter()
        .all(|s| s.len() == 6 && s.chars().all(|c| c.is_ascii_hexdigit())));
}
