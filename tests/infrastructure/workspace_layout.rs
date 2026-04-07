use discord_transcript::infrastructure::workspace::MeetingWorkspaceLayout;

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
