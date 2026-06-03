use discord_transcript::infrastructure::storage_fs::sanitize_path_component;
use std::path::{Component, Path};

#[test]
fn sanitize_path_component_falls_back_for_special_dot_components() {
    for raw in [".", ".@.", ". .", ".\u{1f4a5}.", ".\0."] {
        let sanitized = sanitize_path_component(raw);

        assert!(sanitized.starts_with("unknown_"));
        assert_ne!(sanitized, ".");
        assert_ne!(sanitized, "..");
    }

    assert_eq!(sanitize_path_component(".."), "_");
}

#[test]
fn sanitize_path_component_removes_path_separators() {
    for raw in [
        "../meeting",
        r"..\meeting",
        "guild/channel",
        r"guild\channel",
        "///",
        "\\\\\\",
    ] {
        let sanitized = sanitize_path_component(raw);

        assert!(!sanitized.contains('/'));
        assert!(!sanitized.contains('\\'));
        assert_ne!(sanitized, ".");
        assert_ne!(sanitized, "..");
        assert!(Path::new(&sanitized).components().all(|component| matches!(
            component,
            Component::Normal(_)
        )));
    }
}
