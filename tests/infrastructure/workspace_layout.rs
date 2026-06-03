use discord_transcript::infrastructure::workspace::MeetingWorkspaceLayout;
use std::path::{Component, Path};

fn assert_only_normal_suffix_components(path: &Path, prefix: &Path, expected_suffix_len: usize) {
    let suffix = path
        .strip_prefix(prefix)
        .expect("path should start with expected prefix");
    let components: Vec<_> = suffix.components().collect();

    assert_eq!(components.len(), expected_suffix_len);
    assert!(
        components
            .iter()
            .all(|component| matches!(component, Component::Normal(_)))
    );
}

#[test]
fn workspace_paths_do_not_collide_between_meetings() {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system time should be after epoch")
        .as_nanos();
    let base = std::env::temp_dir().join(format!("workspace_layout_test_{nanos}"));
    let layout = MeetingWorkspaceLayout::new(&base);
    let first = layout.for_meeting("guildA", "channel1", "meeting1");
    let second = layout.for_meeting("guildA", "channel2", "meeting1");
    let third = layout.for_meeting("guildB", "channel1", "meeting1");

    assert_ne!(first.audio_dir(), second.audio_dir());
    assert_ne!(first.audio_dir(), third.audio_dir());
    assert_ne!(second.audio_dir(), third.audio_dir());
}

#[test]
fn debug_paths_are_under_workspace_root() {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system time should be after epoch")
        .as_nanos();
    let base = std::env::temp_dir().join(format!("workspace_debug_paths_{nanos}"));
    let layout = MeetingWorkspaceLayout::new(&base);
    let workspace = layout.for_meeting("g", "vc", "m");
    let root = workspace.root().to_path_buf();

    assert!(workspace.debug_dir().starts_with(&root));
    assert!(workspace.whisper_debug_dir().starts_with(&root));
    assert!(
        workspace
            .whisper_response_path("alice")
            .starts_with(workspace.whisper_debug_dir())
    );
    assert!(
        workspace
            .mixdown_whisper_response_path()
            .starts_with(workspace.whisper_debug_dir())
    );
    assert!(
        workspace
            .pre_correction_transcript_path()
            .starts_with(workspace.debug_dir())
    );
    assert!(
        workspace
            .correction_prompt_path()
            .starts_with(workspace.debug_dir())
    );
    assert!(
        workspace
            .summary_prompt_path()
            .starts_with(workspace.debug_dir())
    );
}

#[test]
fn workspace_for_meeting_filtered_dotdot_stays_under_workspace_root() {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system time should be after epoch")
        .as_nanos();
    let base = std::env::temp_dir().join(format!("workspace_dotdot_regression_{nanos}"));
    let layout = MeetingWorkspaceLayout::new(&base);
    let workspace_root = layout.workspace_root();
    let workspace = layout.for_meeting(".@.", ".@.", ".@.");

    assert!(workspace.root().starts_with(&workspace_root));
    assert_only_normal_suffix_components(workspace.root(), &workspace_root, 3);
}

#[test]
fn legacy_meeting_dir_filtered_dotdot_does_not_escape_base_dir() {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system time should be after epoch")
        .as_nanos();
    let base = std::env::temp_dir().join(format!("legacy_dotdot_regression_{nanos}"));
    let layout = MeetingWorkspaceLayout::new(&base);
    let meeting_dir = layout.legacy_meeting_dir(".@.");

    assert!(meeting_dir.starts_with(&base));
    assert_only_normal_suffix_components(&meeting_dir, &base, 1);
}

#[test]
fn raw_whisper_response_path_filtered_dotdot_stays_under_debug_dir() {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system time should be after epoch")
        .as_nanos();
    let base = std::env::temp_dir().join(format!("whisper_dotdot_regression_{nanos}"));
    let layout = MeetingWorkspaceLayout::new(&base);
    let workspace = layout.for_meeting("g", "vc", "m");
    let whisper_path = workspace.whisper_response_path(".@.");

    assert!(whisper_path.starts_with(workspace.whisper_debug_dir()));
    assert_only_normal_suffix_components(&whisper_path, &workspace.whisper_debug_dir(), 1);
    assert_ne!(
        whisper_path.file_name().and_then(|name| name.to_str()),
        Some("..json")
    );
}

#[test]
fn ensure_base_dirs_creates_debug_whisper_dir() {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system time should be after epoch")
        .as_nanos();
    let base = std::env::temp_dir().join(format!("workspace_ensure_debug_{nanos}"));
    let layout = MeetingWorkspaceLayout::new(&base);
    let workspace = layout.for_meeting("g", "vc", "m");

    workspace
        .ensure_base_dirs()
        .expect("ensure_base_dirs should succeed");

    assert!(workspace.debug_dir().is_dir());
    assert!(workspace.whisper_debug_dir().is_dir());

    std::fs::remove_dir_all(&base).ok();
}
