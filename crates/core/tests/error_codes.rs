//! The skeleton's own test: error codes are stable, and a stub says it is not built.

use acnh_core::{Error, NotBuilt};

#[test]
fn codes_are_stable() {
    let cases = [
        (Error::NotBuilt(NotBuilt("x")), "not_built"),
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

// The stub helper itself, not a stub: a module's issue builds its stubs without touching this.
#[test]
fn a_stub_is_not_built() {
    let err = acnh_core::error::not_built::<()>("x").unwrap_err();
    assert_eq!(err.code(), "not_built");
    assert_eq!(err.to_string(), "not built yet: x");
}
