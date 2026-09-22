use cstore::*;

#[test]
fn keys_reject_escape_and_nonportable_spellings_including_deserialized_input() {
    for value in [
        "",
        "/etc/passwd",
        "../secret",
        "x/../secret",
        "x//y",
        "x\\y",
        "C:/x",
        "x\0y",
        ".",
    ] {
        assert!(Key::new(value).is_err(), "{value:?}");
        assert!(serde_json::from_str::<Key>(&serde_json::to_string(value).unwrap()).is_err());
    }
    assert!(Key::new("opportunities/example/engineer/application.toml").is_ok());
}

#[test]
fn hash_validation_is_exact_and_not_path_injection() {
    assert_eq!(
        ContentHash::of(b"abc").as_str(),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
    assert!(ContentHash::of(b"abc").verify(b"abd").is_err());
    assert!(serde_json::from_str::<ContentHash>("\"../../other\"").is_err());
}
