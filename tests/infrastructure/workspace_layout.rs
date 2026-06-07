use discord_transcript::application::summary::materialize_summary_agent_workspace;
use discord_transcript::application::summary::{SummaryRequest, TranscriptManifest};
use discord_transcript::domain::privacy::MaskingStats;
use discord_transcript::infrastructure::workspace::{
    AGENT_CURSOR_CONFIG_FILENAME, AGENT_CURSOR_DIR, AGENT_INPUT_DIR, AGENT_OUTPUT_DIR,
    AgentWorkspaceBuilder, MeetingWorkspaceLayout,
};
use std::path::{Component, Path, PathBuf};

fn unique_temp_dir(test_name: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system time should be after epoch")
        .as_nanos();
    std::env::temp_dir().join(format!("{test_name}_{nanos}"))
}

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
    assert!(
        workspace
            .context_manifest_path()
            .starts_with(workspace.context_dir())
    );
    assert!(
        workspace
            .context_speaker_roster_path()
            .starts_with(workspace.context_dir())
    );
    assert!(
        workspace
            .context_domain_knowledge_path()
            .starts_with(workspace.context_dir())
    );
    assert!(
        workspace
            .context_ai_memory_path()
            .starts_with(workspace.context_dir())
    );
    assert!(
        workspace
            .context_person_aliases_path()
            .starts_with(workspace.context_dir())
    );
    assert!(
        workspace
            .context_user_feedback_path()
            .starts_with(workspace.context_dir())
    );
    assert!(
        workspace
            .context_summary_template_path()
            .starts_with(workspace.context_dir())
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

#[test]
fn summary_agent_workspace_materializes_only_approved_inputs_and_config() {
    let base = unique_temp_dir("summary_agent_workspace");
    let layout = MeetingWorkspaceLayout::new(&base);
    let workspace = layout.for_meeting("g", "vc", "m");
    workspace.ensure_base_dirs().expect("workspace dirs");
    std::fs::write(workspace.masked_transcript_path(), "masked transcript")
        .expect("write transcript");
    let manifest = TranscriptManifest {
        meeting_id: "m".to_owned(),
        guild_id: "g".to_owned(),
        voice_channel_id: "vc".to_owned(),
        language: Some("en".to_owned()),
        masked_transcript_path: "transcript/transcript_masked.md".to_owned(),
        generated_at: "2026-06-07T00:00:00Z".to_owned(),
        masking_stats: MaskingStats::default(),
    };
    std::fs::write(
        workspace.transcript_manifest_path(),
        serde_json::to_vec_pretty(&manifest).expect("manifest json"),
    )
    .expect("write manifest");
    std::fs::write(workspace.context_manifest_path(), "{}").expect("write context manifest");
    std::fs::write(workspace.context_speaker_roster_path(), "# Speakers")
        .expect("write speaker roster");
    std::fs::write(workspace.context_summary_template_path(), "template")
        .expect("write template");
    std::fs::write(base.join(".env"), "SECRET=1").expect("write env");
    std::fs::write(workspace.debug_dir().join("summary_prompt.txt"), "debug prompt")
        .expect("write debug");
    std::fs::write(workspace.summary_dir().join("summary.md"), "old summary")
        .expect("write old summary");

    let request = SummaryRequest {
        meeting_id: "m".to_owned(),
        guild_id: "g".to_owned(),
        voice_channel_id: "vc".to_owned(),
        title: None,
        audio_path: String::new(),
        speaker_audio: Vec::new(),
        language: Some("en".to_owned()),
        workspace: workspace.clone(),
    };
    let agent_root = workspace.root().join("agent").join("run-1");

    let agent_workspace =
        materialize_summary_agent_workspace(&request, &agent_root).expect("materialize");

    assert_eq!(agent_workspace.root(), agent_root.as_path());
    assert_eq!(
        std::fs::read_to_string(agent_root.join("input/transcript/transcript_masked.md"))
            .expect("agent transcript"),
        "masked transcript"
    );
    assert!(agent_root.join("input/transcript/manifest.json").is_file());
    assert!(agent_root.join("input/context/manifest.json").is_file());
    assert!(agent_root.join("input/context/speaker_roster.md").is_file());
    assert!(agent_root.join("input/context/summary_template.txt").is_file());
    assert!(agent_workspace.output_dir().is_dir());
    assert_eq!(
        agent_workspace.expected_output_path(),
        agent_root.join("output/summary.md").as_path()
    );
    assert!(agent_workspace.cursor_config_path().is_file());
    assert!(!agent_root.join(".env").exists());
    assert!(!agent_root.join("debug").exists());
    assert!(!agent_root.join("summary").exists());
    assert!(!agent_root.join("audio").exists());

    let top_level_entries = sorted_relative_files(&agent_root);
    assert_eq!(
        top_level_entries,
        vec![
            ".cursor/cli.json",
            "input/context/manifest.json",
            "input/context/speaker_roster.md",
            "input/context/summary_template.txt",
            "input/transcript/manifest.json",
            "input/transcript/transcript_masked.md",
        ]
    );

    let cursor_config = std::fs::read_to_string(agent_workspace.cursor_config_path())
        .expect("cursor config");
    assert!(cursor_config.contains("Read(input/transcript/transcript_masked.md)"));
    assert!(cursor_config.contains("Read(input/context/manifest.json)"));
    assert!(cursor_config.contains("Write(output/summary.md)"));
    assert!(!cursor_config.contains(workspace.root().to_string_lossy().as_ref()));

    std::fs::remove_dir_all(&base).ok();
}

#[test]
fn agent_workspace_builder_rejects_traversal_destinations() {
    let base = unique_temp_dir("agent_workspace_traversal");
    let meeting_root = base.join("meeting");
    let agent_root = base.join("agent");
    std::fs::create_dir_all(&meeting_root).expect("meeting root");
    let source = meeting_root.join("transcript.md");
    std::fs::write(&source, "transcript").expect("source");

    let err = AgentWorkspaceBuilder::new(&meeting_root, &agent_root)
        .add_input_file(&source, "input/../secret.md")
        .expect_err("traversal destination should fail");

    assert!(err.to_string().contains("traversal"));
    assert!(!agent_root.exists());
    std::fs::remove_dir_all(&base).ok();
}

#[test]
fn agent_workspace_builder_rejects_boundary_directory_destinations() {
    let base = unique_temp_dir("agent_workspace_boundary_destination");
    let meeting_root = base.join("meeting");
    let agent_root = base.join("agent");
    std::fs::create_dir_all(&meeting_root).expect("meeting root");
    let source = meeting_root.join("transcript.md");
    std::fs::write(&source, "transcript").expect("source");

    let input_err = AgentWorkspaceBuilder::new(&meeting_root, &agent_root)
        .add_input_file(&source, "input")
        .expect_err("input directory destination should fail");
    let output_err = AgentWorkspaceBuilder::new(&meeting_root, &agent_root)
        .with_expected_output("output")
        .expect_err("output directory destination should fail");

    assert!(input_err.to_string().contains("file name"));
    assert!(output_err.to_string().contains("file name"));
    assert!(!agent_root.exists());
    std::fs::remove_dir_all(&base).ok();
}

#[test]
fn agent_workspace_builder_rejects_agent_root_outside_meeting_workspace() {
    let base = unique_temp_dir("agent_workspace_root_escape");
    let meeting_root = base.join("meeting");
    let agent_root = base.join("agent").join("run-1");
    std::fs::create_dir_all(&meeting_root).expect("meeting root");
    let source = meeting_root.join("transcript.md");
    std::fs::write(&source, "transcript").expect("source");

    let err = AgentWorkspaceBuilder::new(&meeting_root, &agent_root)
        .add_input_file(&source, "input/transcript/transcript_masked.md")
        .expect("register source")
        .build()
        .expect_err("agent root outside meeting workspace should fail");

    assert!(err.to_string().contains("under the meeting workspace"));
    assert!(!agent_root.exists());
    std::fs::remove_dir_all(&base).ok();
}

#[test]
fn agent_workspace_builder_rejects_duplicate_input_destinations() {
    let base = unique_temp_dir("agent_workspace_duplicate_destination");
    let meeting_root = base.join("meeting");
    let agent_root = meeting_root.join("agent").join("run-1");
    std::fs::create_dir_all(&meeting_root).expect("meeting root");
    let first = meeting_root.join("first.md");
    let second = meeting_root.join("second.md");
    std::fs::write(&first, "first").expect("first");
    std::fs::write(&second, "second").expect("second");

    let err = AgentWorkspaceBuilder::new(&meeting_root, &agent_root)
        .add_input_file(&first, "input/transcript/transcript_masked.md")
        .expect("register first")
        .add_input_file(&second, "input/transcript/transcript_masked.md")
        .expect("register second")
        .build()
        .expect_err("duplicate destination should fail");

    assert!(err.to_string().contains("duplicate"));
    assert!(!agent_root.exists());
    std::fs::remove_dir_all(&base).ok();
}

#[test]
fn agent_workspace_builder_rejects_sources_outside_meeting_workspace() {
    let base = unique_temp_dir("agent_workspace_escape");
    let meeting_root = base.join("meeting");
    let agent_root = base.join("meeting").join("agent").join("run-1");
    let secret = base.join("secret.txt");
    std::fs::create_dir_all(&meeting_root).expect("meeting root");
    std::fs::write(&secret, "secret").expect("secret");

    let err = AgentWorkspaceBuilder::new(&meeting_root, &agent_root)
        .add_input_file(&secret, "input/transcript/transcript_masked.md")
        .expect("register source")
        .build()
        .expect_err("outside source should fail");

    assert!(err.to_string().contains("outside meeting workspace"));
    assert!(!agent_root.exists());
    std::fs::remove_dir_all(&base).ok();
}

#[cfg(unix)]
#[test]
fn agent_workspace_builder_rejects_symlink_sources() {
    use std::os::unix::fs::symlink;

    let base = unique_temp_dir("agent_workspace_symlink");
    let meeting_root = base.join("meeting");
    let agent_root = meeting_root.join("agent").join("run-1");
    std::fs::create_dir_all(&meeting_root).expect("meeting root");
    let target = meeting_root.join("target.txt");
    let link = meeting_root.join("transcript_masked.md");
    std::fs::write(&target, "transcript").expect("target");
    symlink(&target, &link).expect("symlink");

    let err = AgentWorkspaceBuilder::new(&meeting_root, &agent_root)
        .add_input_file(&link, "input/transcript/transcript_masked.md")
        .expect("register source")
        .build()
        .expect_err("symlink source should fail");

    assert!(err.to_string().contains("not a regular file"));
    assert!(!agent_root.exists());
    std::fs::remove_dir_all(&base).ok();
}

#[cfg(unix)]
#[test]
fn agent_workspace_builder_rejects_symlinked_agent_root_ancestor() {
    use std::os::unix::fs::symlink;

    let base = unique_temp_dir("agent_workspace_symlink_ancestor");
    let meeting_root = base.join("meeting");
    let symlink_target = base.join("redirected");
    let agent_parent = meeting_root.join("agent");
    let agent_root = agent_parent.join("run-1");
    std::fs::create_dir_all(&meeting_root).expect("meeting root");
    std::fs::create_dir_all(&symlink_target).expect("symlink target");
    let source = meeting_root.join("transcript.md");
    std::fs::write(&source, "transcript").expect("source");
    symlink(&symlink_target, &agent_parent).expect("agent parent symlink");

    let err = AgentWorkspaceBuilder::new(&meeting_root, &agent_root)
        .add_input_file(&source, "input/transcript/transcript_masked.md")
        .expect("register source")
        .build()
        .expect_err("symlinked agent root ancestor should fail");

    assert!(err.to_string().contains("real directory"));
    assert!(!symlink_target.join("run-1").exists());
    std::fs::remove_dir_all(&base).ok();
}

#[test]
fn agent_workspace_cleanup_removes_only_agent_root() {
    let base = unique_temp_dir("agent_workspace_cleanup");
    let meeting_root = base.join("meeting");
    let agent_root = meeting_root.join("agent").join("run-1");
    std::fs::create_dir_all(&meeting_root).expect("meeting root");
    let source = meeting_root.join("transcript.md");
    std::fs::write(&source, "transcript").expect("source");

    let agent_workspace = AgentWorkspaceBuilder::new(&meeting_root, &agent_root)
        .add_input_file(&source, "input/transcript/transcript_masked.md")
        .expect("register source")
        .build()
        .expect("materialize");

    assert!(agent_root.exists());
    agent_workspace.cleanup().expect("cleanup");

    assert!(!agent_root.exists());
    assert!(meeting_root.exists());
    assert_eq!(
        std::fs::read_to_string(source).expect("source remains"),
        "transcript"
    );
    std::fs::remove_dir_all(&base).ok();
}

#[cfg(unix)]
#[test]
fn agent_workspace_cleanup_refuses_replaced_root() {
    let base = unique_temp_dir("agent_workspace_cleanup_replaced");
    let meeting_root = base.join("meeting");
    let agent_root = meeting_root.join("agent").join("run-1");
    std::fs::create_dir_all(&meeting_root).expect("meeting root");
    let source = meeting_root.join("transcript.md");
    std::fs::write(&source, "transcript").expect("source");

    let agent_workspace = AgentWorkspaceBuilder::new(&meeting_root, &agent_root)
        .add_input_file(&source, "input/transcript/transcript_masked.md")
        .expect("register source")
        .build()
        .expect("materialize");
    std::fs::remove_dir_all(&agent_root).expect("remove original agent root");
    std::fs::create_dir_all(&agent_root).expect("replace agent root");
    std::fs::write(agent_root.join("unrelated.txt"), "do not delete").expect("replacement file");

    let err = agent_workspace
        .cleanup()
        .expect_err("cleanup should reject replaced root");

    assert!(err.to_string().contains("identity changed"));
    assert!(agent_root.join("unrelated.txt").is_file());
    std::fs::remove_dir_all(&base).ok();
}

fn sorted_relative_files(root: &Path) -> Vec<String> {
    let mut files = Vec::new();
    collect_relative_files(root, root, &mut files);
    files.sort();
    files
}

fn collect_relative_files(root: &Path, dir: &Path, files: &mut Vec<String>) {
    for entry in std::fs::read_dir(dir).expect("read dir") {
        let entry = entry.expect("dir entry");
        let path = entry.path();
        if path.is_dir() {
            collect_relative_files(root, &path, files);
        } else {
            files.push(
                path.strip_prefix(root)
                    .expect("relative")
                    .components()
                    .map(|component| component.as_os_str().to_string_lossy())
                    .collect::<Vec<_>>()
                    .join("/"),
            );
        }
    }
}

#[test]
fn agent_workspace_layout_constants_match_expected_boundary() {
    assert_eq!(AGENT_INPUT_DIR, "input");
    assert_eq!(AGENT_OUTPUT_DIR, "output");
    assert_eq!(AGENT_CURSOR_DIR, ".cursor");
    assert_eq!(AGENT_CURSOR_CONFIG_FILENAME, "cli.json");
}
