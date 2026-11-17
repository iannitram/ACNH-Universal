// Locked tests for config (issue 1.05: config.ini and launcher.ini, SPEC.md M2).
use acnh_core::config::{Config, Line};
use acnh_core::Error;

fn entry(k: &str, v: &str) -> Line {
    Line::Entry {
        key: k.into(),
        value: v.into(),
    }
}

fn other(t: &str) -> Line {
    Line::Other(t.into())
}

#[test]
fn line_counting() {
    assert_eq!(Config::parse("").unwrap().lines.len(), 0);
    assert_eq!(Config::parse("a=1\n").unwrap().lines.len(), 1);
    assert_eq!(Config::parse("a=1").unwrap().lines.len(), 1);
    assert_eq!(Config::parse("a=1\n\n").unwrap().lines.len(), 2);
}

#[test]
fn parses_every_kind_of_line() {
    let text = "# comment\n[video]\nvideo.fullscreen = 1\r\n  ; also a comment\n\n  video.window=  1280x800  \ngame.x = a=b\n";
    let c = Config::parse(text).unwrap();
    assert_eq!(
        c.lines,
        [
            other("# comment"),
            other("[video]"),
            entry("video.fullscreen", "1"),
            other("  ; also a comment"),
            other(""),
            entry("video.window", "1280x800"),
            entry("game.x", "a=b"),
        ]
    );
}

#[test]
fn empty_value_is_allowed() {
    let c = Config::parse("a =\n").unwrap();
    assert_eq!(c.lines, [entry("a", "")]);
}

#[test]
fn errors_name_the_line() {
    assert_eq!(
        Config::parse("a = 1\njust words\n"),
        Err(Error::Config("line 2 has no '=': just words".into()))
    );
    assert_eq!(
        Config::parse("# x\n\n = 3\n"),
        Err(Error::Config("line 3 has no key".into()))
    );
}

#[test]
fn get_takes_the_last_entry() {
    let c = Config::parse("a = 1\nb = 2\na = 3\n").unwrap();
    assert_eq!(c.get("a"), Some("3"));
    assert_eq!(c.get("b"), Some("2"));
    assert_eq!(c.get("c"), None);
    assert_eq!(c.get("A"), None);
}

#[test]
fn set_changes_every_entry_in_place() {
    let mut c = Config::parse("# top\na = 1\nb = 2\na = 3\n").unwrap();
    c.set("a", "9");
    assert_eq!(c.to_text(), "# top\na = 9\nb = 2\na = 9\n");
}

#[test]
fn set_adds_a_missing_key_at_the_end() {
    let mut c = Config::parse("# top\nb = 2\n").unwrap();
    c.set("video.frame_cap", "30");
    assert_eq!(c.to_text(), "# top\nb = 2\nvideo.frame_cap = 30\n");
    let mut empty = Config::default();
    empty.set("x", "1");
    assert_eq!(empty.to_text(), "x = 1\n");
}

#[test]
fn text_round_trips() {
    let text = "# c\n[s]\n  ; kept as is  \nk=v\n\nx   =   y z\n";
    let c = Config::parse(text).unwrap();
    assert_eq!(
        c.to_text(),
        "# c\n[s]\n  ; kept as is  \nk = v\n\nx = y z\n"
    );
    assert_eq!(Config::parse(&c.to_text()).unwrap(), c);
}
