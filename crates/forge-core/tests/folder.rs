use forge_core::folder::{folder_key, windows_safe_folder};

#[test]
fn key_ignores_case_and_trailing_dots_and_spaces() {
    for name in ["bagnon", "Bagnon.", "Bagnon. ", "BAGNON..", "Bagnon .."] {
        assert_eq!(folder_key(name), folder_key("Bagnon"), "{name}");
    }
    assert_ne!(folder_key("Bagnon_Options"), folder_key("Bagnon"));
}

#[test]
fn windows_unsafe_names() {
    for name in ["Bagnon.", "Bagnon ", "CON", "con", "com1.lua", "LPT9", "a:b", "a<b", "a|b", "a?b", "a*b", "a\"b", "", ".", "..", "SHAGUT~1", "shagut~12", "SH5A3F~1.TOC"] {
        assert!(!windows_safe_folder(name), "{name:?} should be unsafe");
    }
    for name in ["Bagnon", "!Libs", "Bagnon_Options", "pfUI", "Console", "COM10", "A.B", "~Libs", "Bagnon~"] {
        assert!(windows_safe_folder(name), "{name:?} should be safe");
    }
}
