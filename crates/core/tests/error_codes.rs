//! Error codes are stable.

use acnh_core::Error;

#[test]
fn codes_are_stable() {
    let cases = [
        (Error::WrongVersion { found: "00".into() }, "wrong_version"),
        (Error::NotAcnh("x".into()), "not_acnh"),
        (Error::BadGameFolder("x".into()), "bad_game_folder"),
        (Error::Env("x".into()), "env"),
        (Error::Unsupported("x".into()), "unsupported"),
        (Error::Config("x".into()), "config"),
        (Error::Steam("x".into()), "steam"),
        (Error::Saves("x".into()), "saves"),
        (Error::Build("x".into()), "build"),
        (Error::Hub("x".into()), "hub"),
        (Error::RelaySignIn, "relay_sign_in"),
        (Error::Io("x".into()), "io"),
    ];
    for (err, code) in cases {
        assert_eq!(err.code(), code);
    }
}
