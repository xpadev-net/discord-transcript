use discord_transcript::infrastructure::storage_fs::{
    ChunkStorage, LocalChunkStorage, decode_sanitized_path_component, sanitize_path_component,
};
use discord_transcript::infrastructure::workspace::MeetingWorkspaceLayout;
use std::path::{Component, Path};

#[test]
fn sanitize_path_component_falls_back_for_special_dot_components() {
    assert_eq!(sanitize_path_component(""), "%EMPTY");
    assert_eq!(sanitize_path_component("."), "%2E");
    assert_eq!(sanitize_path_component(".."), "%2E%2E");

    for raw in [".@.", ". .", ".\u{1f4a5}.", ".\0."] {
        let sanitized = sanitize_path_component(raw);

        assert_ne!(sanitized, ".");
        assert_ne!(sanitized, "..");
    }
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

#[test]
fn sanitize_path_component_encodes_without_collapsing_distinct_ids() {
    assert_eq!(sanitize_path_component("alice-1_2.3"), "alice-1_2.3");
    assert_eq!(sanitize_path_component("a/b"), "a%2Fb");
    assert_eq!(sanitize_path_component(r"a\b"), "a%5Cb");
    assert_eq!(sanitize_path_component("ssrc:100"), "ssrc%3A100");
    assert_eq!(sanitize_path_component("%2F"), "%252F");

    for (left, right) in [
        ("a/b", "a_b"),
        (r"a\b", "a_b"),
        ("a..b", "a_b"),
        ("ssrc:100", "ssrc100"),
        (".", "%2E"),
        ("", "%EMPTY"),
    ] {
        assert_ne!(
            sanitize_path_component(left),
            sanitize_path_component(right),
            "{left:?} and {right:?} should not share a filesystem component"
        );
    }
}

#[test]
fn decode_sanitized_path_component_round_trips_canonical_encoding() {
    for raw in ["", ".", "..", "a/b", r"a\b", "ssrc:100", "%2F", "ユーザー"] {
        let encoded = sanitize_path_component(raw);
        assert_eq!(
            decode_sanitized_path_component(&encoded).as_deref(),
            Some(raw)
        );
    }

    assert_eq!(decode_sanitized_path_component("a%2fb"), None);
    assert_eq!(decode_sanitized_path_component("a%2"), None);
}

#[test]
fn local_chunk_storage_keeps_colliding_user_ids_in_distinct_files() {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system time should be after epoch")
        .as_nanos();
    let base = std::env::temp_dir().join(format!("storage_fs_collision_{nanos}"));
    let layout = MeetingWorkspaceLayout::new(&base);
    let workspace = layout.for_meeting("g1", "vc1", "m1");
    let storage = LocalChunkStorage::new(workspace, "m1");

    let first = storage
        .save_chunk("m1", "a/b", 1, 0, b"first")
        .expect("first chunk should save");
    let second = storage
        .save_chunk("m1", "a_b", 1, 0, b"second")
        .expect("second chunk should save");

    assert_ne!(first.path, second.path);
    assert_eq!(
        first.path.file_name().and_then(|name| name.to_str()),
        Some("a%2Fb_1_0.wav")
    );
    assert_eq!(
        second.path.file_name().and_then(|name| name.to_str()),
        Some("a_b_1_0.wav")
    );
    assert_eq!(std::fs::read(first.path).expect("first chunk"), b"first");
    assert_eq!(std::fs::read(second.path).expect("second chunk"), b"second");

    let _ = std::fs::remove_dir_all(base);
}
