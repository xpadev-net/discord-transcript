use crate::infrastructure::storage_fs::sanitize_path_component;
use serde::Serialize;
use std::collections::HashSet;
use std::fmt::{Display, Formatter};
use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};

pub const WORKSPACES_ROOT_DIR: &str = "workspaces";
pub const MASKED_TRANSCRIPT_FILENAME: &str = "transcript_masked.md";
pub const TRANSCRIPT_MANIFEST_FILENAME: &str = "manifest.json";
pub const SSRC_MAPPING_FILENAME: &str = "ssrc_mapping.json";
pub const DEBUG_DIR: &str = "debug";
pub const DEBUG_WHISPER_DIR: &str = "whisper";
pub const DEBUG_MIXDOWN_WHISPER_FILENAME: &str = "mixdown.json";
pub const DEBUG_PRE_CORRECTION_TRANSCRIPT_FILENAME: &str = "transcript_pre_correction.md";
pub const DEBUG_CORRECTION_PROMPT_FILENAME: &str = "correction_prompt.txt";
pub const DEBUG_SUMMARY_PROMPT_FILENAME: &str = "summary_prompt.txt";
pub const CONTEXT_MANIFEST_FILENAME: &str = "manifest.json";
pub const CONTEXT_SPEAKER_ROSTER_FILENAME: &str = "speaker_roster.md";
pub const CONTEXT_DOMAIN_KNOWLEDGE_FILENAME: &str = "domain_knowledge.md";
pub const CONTEXT_AI_MEMORY_FILENAME: &str = "ai_memory.md";
pub const CONTEXT_PERSON_ALIASES_FILENAME: &str = "person_aliases.md";
pub const CONTEXT_USER_FEEDBACK_FILENAME: &str = "user_feedback.md";
pub const CONTEXT_SUMMARY_TEMPLATE_FILENAME: &str = "summary_template.txt";
pub const AGENT_INPUT_DIR: &str = "input";
pub const AGENT_OUTPUT_DIR: &str = "output";
pub const AGENT_CURSOR_DIR: &str = ".cursor";
pub const AGENT_CURSOR_CONFIG_FILENAME: &str = "cli.json";
pub const AGENT_SUMMARY_OUTPUT_FILENAME: &str = "summary.md";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MeetingWorkspaceLayout {
    base_dir: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MeetingWorkspacePaths {
    root: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentWorkspace {
    root: PathBuf,
    expected_output_path: PathBuf,
    cursor_config_path: PathBuf,
    root_identity: AgentWorkspaceRootIdentity,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentWorkspaceBuilder {
    meeting_root: PathBuf,
    agent_root: PathBuf,
    expected_output_path: PathBuf,
    inputs: Vec<AgentWorkspaceInput>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct AgentWorkspaceInput {
    source: PathBuf,
    destination: PathBuf,
}

#[cfg(unix)]
#[derive(Debug, Clone, PartialEq, Eq)]
struct AgentWorkspaceRootIdentity {
    dev: u64,
    ino: u64,
}

#[cfg(not(unix))]
#[derive(Debug, Clone, PartialEq, Eq)]
struct AgentWorkspaceRootIdentity;

#[derive(Debug)]
pub enum AgentWorkspaceError {
    InvalidPath {
        path: PathBuf,
        reason: &'static str,
    },
    SourceOutsideMeetingWorkspace {
        source: PathBuf,
        meeting_root: PathBuf,
    },
    SourceNotRegularFile(PathBuf),
    DestinationAlreadyExists(PathBuf),
    Io {
        path: PathBuf,
        source: io::Error,
    },
    Serialize(serde_json::Error),
}

impl Display for AgentWorkspaceError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidPath { path, reason } => {
                write!(
                    f,
                    "invalid agent workspace path {}: {reason}",
                    path.display()
                )
            }
            Self::SourceOutsideMeetingWorkspace {
                source,
                meeting_root,
            } => write!(
                f,
                "agent workspace input {} is outside meeting workspace {}",
                source.display(),
                meeting_root.display()
            ),
            Self::SourceNotRegularFile(path) => {
                write!(
                    f,
                    "agent workspace input is not a regular file: {}",
                    path.display()
                )
            }
            Self::DestinationAlreadyExists(path) => {
                write!(f, "agent workspace already exists: {}", path.display())
            }
            Self::Io { path, source } => {
                write!(
                    f,
                    "agent workspace filesystem error at {}: {source}",
                    path.display()
                )
            }
            Self::Serialize(err) => write!(f, "failed to serialize agent workspace config: {err}"),
        }
    }
}

impl std::error::Error for AgentWorkspaceError {}

#[derive(Debug, Serialize)]
struct CursorCliConfig {
    permissions: CursorPermissions,
}

#[derive(Debug, Serialize)]
struct CursorPermissions {
    allow: Vec<String>,
    deny: Vec<String>,
}

impl MeetingWorkspaceLayout {
    pub fn new(base_dir: impl AsRef<Path>) -> Self {
        Self {
            base_dir: base_dir.as_ref().to_path_buf(),
        }
    }

    pub fn workspace_root(&self) -> PathBuf {
        self.base_dir.join(WORKSPACES_ROOT_DIR)
    }

    pub fn for_meeting(
        &self,
        guild_id: &str,
        voice_channel_id: &str,
        meeting_id: &str,
    ) -> MeetingWorkspacePaths {
        let guild = sanitize_path_component(guild_id);
        let channel = sanitize_path_component(voice_channel_id);
        let meeting = sanitize_path_component(meeting_id);
        let root = self
            .workspace_root()
            .join(guild)
            .join(channel)
            .join(meeting);
        MeetingWorkspacePaths { root }
    }

    pub fn legacy_meeting_dir(&self, meeting_id: &str) -> PathBuf {
        self.base_dir.join(sanitize_path_component(meeting_id))
    }
}

impl MeetingWorkspacePaths {
    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn audio_dir(&self) -> PathBuf {
        self.root.join("audio")
    }

    pub fn transcript_dir(&self) -> PathBuf {
        self.root.join("transcript")
    }

    pub fn context_dir(&self) -> PathBuf {
        self.root.join("context")
    }

    pub fn summary_dir(&self) -> PathBuf {
        self.root.join("summary")
    }

    pub fn mixdown_path(&self) -> PathBuf {
        self.audio_dir().join("mixdown.wav")
    }

    pub fn speakers_dir(&self) -> PathBuf {
        self.audio_dir().join("speakers")
    }

    pub fn masked_transcript_path(&self) -> PathBuf {
        self.transcript_dir().join(MASKED_TRANSCRIPT_FILENAME)
    }

    pub fn transcript_manifest_path(&self) -> PathBuf {
        self.transcript_dir().join(TRANSCRIPT_MANIFEST_FILENAME)
    }

    pub fn ssrc_mapping_path(&self) -> PathBuf {
        self.audio_dir().join(SSRC_MAPPING_FILENAME)
    }

    pub fn debug_dir(&self) -> PathBuf {
        self.root.join(DEBUG_DIR)
    }

    pub fn whisper_debug_dir(&self) -> PathBuf {
        self.debug_dir().join(DEBUG_WHISPER_DIR)
    }

    /// Per-speaker Whisper raw response path.
    ///
    /// The speaker identifier is sanitized internally via
    /// [`sanitize_path_component`] so this method is safe to call with raw
    /// (untrusted) speaker IDs and cannot escape the workspace.
    pub fn whisper_response_path(&self, speaker_id: &str) -> PathBuf {
        self.whisper_response_path_for_sanitized(&sanitize_path_component(speaker_id))
    }

    /// Lower-level path builder for callers that already hold a value
    /// produced by [`sanitize_path_component`]. Avoids a redundant
    /// idempotent re-sanitization. Prefer [`whisper_response_path`] when
    /// the input may be raw.
    pub fn whisper_response_path_for_sanitized(&self, safe_speaker_id: &str) -> PathBuf {
        self.whisper_debug_dir()
            .join(format!("{safe_speaker_id}.json"))
    }

    pub fn mixdown_whisper_response_path(&self) -> PathBuf {
        self.whisper_debug_dir()
            .join(DEBUG_MIXDOWN_WHISPER_FILENAME)
    }

    pub fn pre_correction_transcript_path(&self) -> PathBuf {
        self.debug_dir()
            .join(DEBUG_PRE_CORRECTION_TRANSCRIPT_FILENAME)
    }

    pub fn correction_prompt_path(&self) -> PathBuf {
        self.debug_dir().join(DEBUG_CORRECTION_PROMPT_FILENAME)
    }

    pub fn summary_prompt_path(&self) -> PathBuf {
        self.debug_dir().join(DEBUG_SUMMARY_PROMPT_FILENAME)
    }

    pub fn context_manifest_path(&self) -> PathBuf {
        self.context_dir().join(CONTEXT_MANIFEST_FILENAME)
    }

    pub fn context_speaker_roster_path(&self) -> PathBuf {
        self.context_dir().join(CONTEXT_SPEAKER_ROSTER_FILENAME)
    }

    pub fn context_domain_knowledge_path(&self) -> PathBuf {
        self.context_dir().join(CONTEXT_DOMAIN_KNOWLEDGE_FILENAME)
    }

    pub fn context_ai_memory_path(&self) -> PathBuf {
        self.context_dir().join(CONTEXT_AI_MEMORY_FILENAME)
    }

    pub fn context_person_aliases_path(&self) -> PathBuf {
        self.context_dir().join(CONTEXT_PERSON_ALIASES_FILENAME)
    }

    pub fn context_user_feedback_path(&self) -> PathBuf {
        self.context_dir().join(CONTEXT_USER_FEEDBACK_FILENAME)
    }

    pub fn context_summary_template_path(&self) -> PathBuf {
        self.context_dir().join(CONTEXT_SUMMARY_TEMPLATE_FILENAME)
    }

    pub fn ensure_base_dirs(&self) -> std::io::Result<()> {
        fs::create_dir_all(self.audio_dir())?;
        fs::create_dir_all(self.transcript_dir())?;
        fs::create_dir_all(self.context_dir())?;
        fs::create_dir_all(self.summary_dir())?;
        fs::create_dir_all(self.speakers_dir())?;
        fs::create_dir_all(self.whisper_debug_dir())
    }

    /// Returns a path relative to the workspace root. Returns None if the
    /// provided path is outside the workspace (avoids leaking absolute paths).
    pub fn relative_path(&self, path: &Path) -> Option<PathBuf> {
        path.strip_prefix(&self.root).ok().map(PathBuf::from)
    }
}

impl AgentWorkspace {
    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn input_dir(&self) -> PathBuf {
        self.root.join(AGENT_INPUT_DIR)
    }

    pub fn output_dir(&self) -> PathBuf {
        self.root.join(AGENT_OUTPUT_DIR)
    }

    pub fn expected_output_path(&self) -> &Path {
        &self.expected_output_path
    }

    pub fn cursor_config_path(&self) -> &Path {
        &self.cursor_config_path
    }

    pub fn cleanup(&self) -> Result<(), AgentWorkspaceError> {
        cleanup_agent_workspace_root(&self.root, &self.root_identity)
    }
}

impl AgentWorkspaceBuilder {
    pub fn new(meeting_root: impl AsRef<Path>, agent_root: impl AsRef<Path>) -> Self {
        let agent_root = agent_root.as_ref().to_path_buf();
        Self {
            meeting_root: meeting_root.as_ref().to_path_buf(),
            expected_output_path: agent_root
                .join(AGENT_OUTPUT_DIR)
                .join(AGENT_SUMMARY_OUTPUT_FILENAME),
            agent_root,
            inputs: Vec::new(),
        }
    }

    pub fn with_expected_output(
        mut self,
        relative_output_path: impl AsRef<Path>,
    ) -> Result<Self, AgentWorkspaceError> {
        let relative_output_path =
            validate_agent_relative_path(relative_output_path.as_ref(), AGENT_OUTPUT_DIR)?;
        self.expected_output_path = self.agent_root.join(relative_output_path);
        Ok(self)
    }

    pub fn add_input_file(
        mut self,
        source: impl AsRef<Path>,
        relative_input_path: impl AsRef<Path>,
    ) -> Result<Self, AgentWorkspaceError> {
        let destination =
            validate_agent_relative_path(relative_input_path.as_ref(), AGENT_INPUT_DIR)?;
        self.inputs.push(AgentWorkspaceInput {
            source: source.as_ref().to_path_buf(),
            destination,
        });
        Ok(self)
    }

    pub fn build(self) -> Result<AgentWorkspace, AgentWorkspaceError> {
        if fs::symlink_metadata(&self.agent_root).is_ok() {
            return Err(AgentWorkspaceError::DestinationAlreadyExists(
                self.agent_root,
            ));
        }

        let meeting_root = canonicalize_existing(&self.meeting_root)?;
        let expected_agent_parent =
            prepare_agent_root_parent(&self.meeting_root, &self.agent_root)?;
        fs::create_dir(&self.agent_root).map_err(|err| AgentWorkspaceError::Io {
            path: self.agent_root.clone(),
            source: err,
        })?;
        harden_agent_dir(&self.agent_root)?;
        if let Some(parent) = self.agent_root.parent() {
            validate_agent_dir(&meeting_root, parent)?;
        }
        let agent_root =
            canonicalize_agent_root(&self.agent_root, &meeting_root, &expected_agent_parent)?;
        let root_identity = root_identity(&self.agent_root)?;

        let cleanup_root = self.agent_root.clone();
        let cleanup_identity = root_identity.clone();
        let build_result = self.populate(meeting_root, agent_root, root_identity);
        if build_result.is_err() {
            let _ = cleanup_agent_workspace_root(&cleanup_root, &cleanup_identity);
        }
        build_result
    }

    fn populate(
        self,
        meeting_root: PathBuf,
        agent_root: PathBuf,
        root_identity: AgentWorkspaceRootIdentity,
    ) -> Result<AgentWorkspace, AgentWorkspaceError> {
        let input_dir = self.agent_root.join(AGENT_INPUT_DIR);
        let output_dir = self.agent_root.join(AGENT_OUTPUT_DIR);
        let cursor_dir = self.agent_root.join(AGENT_CURSOR_DIR);

        create_agent_dir(&input_dir)?;
        create_agent_dir(&output_dir)?;
        create_agent_dir(&cursor_dir)?;

        let mut allowed_reads = Vec::new();
        let mut destinations = HashSet::new();
        for input in &self.inputs {
            let source_metadata = validate_copy_source(&meeting_root, &agent_root, &input.source)?;
            if !destinations.insert(input.destination.clone()) {
                return Err(AgentWorkspaceError::InvalidPath {
                    path: input.destination.clone(),
                    reason: "duplicate agent workspace input destination",
                });
            }
            let destination = self.agent_root.join(&input.destination);
            prepare_agent_parent_dirs(&self.agent_root, &agent_root, &destination)?;
            copy_regular_file_no_symlink(
                &input.source,
                &self.agent_root,
                &input.destination,
                &source_metadata,
            )?;
            allowed_reads.push(permission_path(&input.destination));
        }

        let expected_output_relative = self
            .expected_output_path
            .strip_prefix(&self.agent_root)
            .map_err(|_| AgentWorkspaceError::InvalidPath {
                path: self.expected_output_path.clone(),
                reason: "expected output must be inside the agent workspace",
            })?;
        validate_agent_relative_path(expected_output_relative, AGENT_OUTPUT_DIR)?;
        let cursor_config_path = cursor_dir.join(AGENT_CURSOR_CONFIG_FILENAME);
        validate_agent_parent_dir(&agent_root, &cursor_config_path)?;
        write_cursor_config(
            &self.agent_root,
            Path::new(AGENT_CURSOR_DIR).join(AGENT_CURSOR_CONFIG_FILENAME),
            allowed_reads,
            permission_path(expected_output_relative),
        )?;

        Ok(AgentWorkspace {
            root: self.agent_root,
            expected_output_path: self.expected_output_path,
            cursor_config_path,
            root_identity,
        })
    }
}

fn create_agent_dir(path: &Path) -> Result<(), AgentWorkspaceError> {
    fs::create_dir(path).map_err(|err| AgentWorkspaceError::Io {
        path: path.to_path_buf(),
        source: err,
    })?;
    harden_agent_dir(path)
}

#[cfg(unix)]
fn harden_agent_dir(path: &Path) -> Result<(), AgentWorkspaceError> {
    use std::os::unix::fs::PermissionsExt;

    let metadata = fs::symlink_metadata(path).map_err(|err| AgentWorkspaceError::Io {
        path: path.to_path_buf(),
        source: err,
    })?;
    if !metadata.file_type().is_dir() || metadata.file_type().is_symlink() {
        return Err(AgentWorkspaceError::InvalidPath {
            path: path.to_path_buf(),
            reason: "agent workspace directory must not be a symlink",
        });
    }
    let mut permissions = metadata.permissions();
    permissions.set_mode(0o700);
    fs::set_permissions(path, permissions).map_err(|err| AgentWorkspaceError::Io {
        path: path.to_path_buf(),
        source: err,
    })
}

#[cfg(not(unix))]
fn harden_agent_dir(path: &Path) -> Result<(), AgentWorkspaceError> {
    let metadata = fs::symlink_metadata(path).map_err(|err| AgentWorkspaceError::Io {
        path: path.to_path_buf(),
        source: err,
    })?;
    if !metadata.file_type().is_dir() || metadata.file_type().is_symlink() {
        return Err(AgentWorkspaceError::InvalidPath {
            path: path.to_path_buf(),
            reason: "agent workspace directory must not be a symlink",
        });
    }
    Ok(())
}

fn prepare_agent_parent_dirs(
    lexical_agent_root: &Path,
    canonical_agent_root: &Path,
    destination: &Path,
) -> Result<(), AgentWorkspaceError> {
    let Some(parent) = destination.parent() else {
        return Err(AgentWorkspaceError::InvalidPath {
            path: destination.to_path_buf(),
            reason: "agent workspace destination must have a parent directory",
        });
    };
    let mut current = lexical_agent_root.to_path_buf();
    let relative_parent =
        parent
            .strip_prefix(lexical_agent_root)
            .map_err(|_| AgentWorkspaceError::InvalidPath {
                path: parent.to_path_buf(),
                reason: "agent workspace destination parent escaped the agent root",
            })?;
    for component in relative_parent.components() {
        current.push(component.as_os_str());
        match fs::create_dir(&current) {
            Ok(()) => harden_agent_dir(&current)?,
            Err(err) if err.kind() == io::ErrorKind::AlreadyExists => {}
            Err(err) => {
                return Err(AgentWorkspaceError::Io {
                    path: current.clone(),
                    source: err,
                });
            }
        }
        validate_agent_dir(canonical_agent_root, &current)?;
    }
    validate_agent_parent_dir(canonical_agent_root, destination)
}

fn validate_agent_parent_dir(
    canonical_agent_root: &Path,
    destination: &Path,
) -> Result<(), AgentWorkspaceError> {
    let Some(parent) = destination.parent() else {
        return Err(AgentWorkspaceError::InvalidPath {
            path: destination.to_path_buf(),
            reason: "agent workspace destination must have a parent directory",
        });
    };
    validate_agent_dir(canonical_agent_root, parent)
}

fn validate_agent_dir(canonical_agent_root: &Path, path: &Path) -> Result<(), AgentWorkspaceError> {
    let metadata = fs::symlink_metadata(path).map_err(|err| AgentWorkspaceError::Io {
        path: path.to_path_buf(),
        source: err,
    })?;
    if !metadata.file_type().is_dir() || metadata.file_type().is_symlink() {
        return Err(AgentWorkspaceError::InvalidPath {
            path: path.to_path_buf(),
            reason: "agent workspace destination parent must be a real directory",
        });
    }
    let canonical_path = canonicalize_existing(path)?;
    if !canonical_path.starts_with(canonical_agent_root) {
        return Err(AgentWorkspaceError::InvalidPath {
            path: path.to_path_buf(),
            reason: "agent workspace destination parent escaped the agent root",
        });
    }
    Ok(())
}

#[cfg(unix)]
fn root_identity(path: &Path) -> Result<AgentWorkspaceRootIdentity, AgentWorkspaceError> {
    use std::os::unix::fs::MetadataExt;

    let metadata = fs::symlink_metadata(path).map_err(|err| AgentWorkspaceError::Io {
        path: path.to_path_buf(),
        source: err,
    })?;
    if !metadata.file_type().is_dir() || metadata.file_type().is_symlink() {
        return Err(AgentWorkspaceError::InvalidPath {
            path: path.to_path_buf(),
            reason: "agent workspace root must be a real directory",
        });
    }
    Ok(AgentWorkspaceRootIdentity {
        dev: metadata.dev(),
        ino: metadata.ino(),
    })
}

#[cfg(not(unix))]
fn root_identity(_path: &Path) -> Result<AgentWorkspaceRootIdentity, AgentWorkspaceError> {
    Ok(AgentWorkspaceRootIdentity)
}

#[cfg(unix)]
fn validate_root_identity(
    path: &Path,
    expected: &AgentWorkspaceRootIdentity,
) -> Result<(), AgentWorkspaceError> {
    let actual = root_identity(path)?;
    if &actual != expected {
        return Err(AgentWorkspaceError::InvalidPath {
            path: path.to_path_buf(),
            reason: "agent workspace root identity changed before cleanup",
        });
    }
    Ok(())
}

#[cfg(not(unix))]
fn validate_root_identity(
    _path: &Path,
    _expected: &AgentWorkspaceRootIdentity,
) -> Result<(), AgentWorkspaceError> {
    Ok(())
}

fn cleanup_agent_workspace_root(
    root: &Path,
    expected: &AgentWorkspaceRootIdentity,
) -> Result<(), AgentWorkspaceError> {
    match fs::symlink_metadata(root) {
        Ok(_) => {}
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(err) => {
            return Err(AgentWorkspaceError::Io {
                path: root.to_path_buf(),
                source: err,
            });
        }
    }

    validate_root_identity(root, expected)?;
    match fs::remove_dir_all(root) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(AgentWorkspaceError::Io {
            path: root.to_path_buf(),
            source: err,
        }),
    }
}

fn canonicalize_existing(path: &Path) -> Result<PathBuf, AgentWorkspaceError> {
    path.canonicalize().map_err(|err| AgentWorkspaceError::Io {
        path: path.to_path_buf(),
        source: err,
    })
}

fn canonicalize_agent_root(
    agent_root: &Path,
    meeting_root: &Path,
    expected_parent: &Path,
) -> Result<PathBuf, AgentWorkspaceError> {
    let canonical_agent_root = canonicalize_existing(agent_root)?;
    if !canonical_agent_root.starts_with(meeting_root) {
        return Err(AgentWorkspaceError::InvalidPath {
            path: agent_root.to_path_buf(),
            reason: "agent workspace root escaped the meeting workspace",
        });
    }
    if canonical_agent_root.parent() != Some(expected_parent) {
        return Err(AgentWorkspaceError::InvalidPath {
            path: agent_root.to_path_buf(),
            reason: "agent workspace root parent changed during materialization",
        });
    }
    Ok(canonical_agent_root)
}

fn prepare_agent_root_parent(
    meeting_root: &Path,
    agent_root: &Path,
) -> Result<PathBuf, AgentWorkspaceError> {
    let Some(parent) = agent_root.parent() else {
        return Err(AgentWorkspaceError::InvalidPath {
            path: agent_root.to_path_buf(),
            reason: "agent workspace root must have a parent directory",
        });
    };
    let canonical_meeting_root = canonicalize_existing(meeting_root)?;
    let relative_parent =
        parent
            .strip_prefix(meeting_root)
            .map_err(|_| AgentWorkspaceError::InvalidPath {
                path: parent.to_path_buf(),
                reason: "agent workspace parent must be under the meeting workspace",
            })?;

    let mut current = canonical_meeting_root.clone();
    for component in relative_parent.components() {
        if !matches!(component, Component::Normal(_)) {
            return Err(AgentWorkspaceError::InvalidPath {
                path: parent.to_path_buf(),
                reason: "agent workspace parent must not contain traversal components",
            });
        }

        current.push(component.as_os_str());
        match fs::create_dir(&current) {
            Ok(()) => harden_agent_dir(&current)?,
            Err(err) if err.kind() == io::ErrorKind::NotFound => {
                return Err(AgentWorkspaceError::Io {
                    path: current.clone(),
                    source: err,
                });
            }
            Err(err) if err.kind() == io::ErrorKind::AlreadyExists => {
                validate_agent_dir(&canonical_meeting_root, &current)?;
            }
            Err(err) => {
                return Err(AgentWorkspaceError::Io {
                    path: current.clone(),
                    source: err,
                });
            }
        }
    }

    Ok(current)
}

fn validate_copy_source(
    meeting_root: &Path,
    agent_root: &Path,
    source: &Path,
) -> Result<fs::Metadata, AgentWorkspaceError> {
    let metadata = fs::symlink_metadata(source).map_err(|err| AgentWorkspaceError::Io {
        path: source.to_path_buf(),
        source: err,
    })?;
    if !metadata.file_type().is_file() || metadata.file_type().is_symlink() {
        return Err(AgentWorkspaceError::SourceNotRegularFile(
            source.to_path_buf(),
        ));
    }

    let canonical_source = canonicalize_existing(source)?;
    if !canonical_source.starts_with(meeting_root) {
        return Err(AgentWorkspaceError::SourceOutsideMeetingWorkspace {
            source: source.to_path_buf(),
            meeting_root: meeting_root.to_path_buf(),
        });
    }
    if canonical_source.starts_with(agent_root) {
        return Err(AgentWorkspaceError::InvalidPath {
            path: source.to_path_buf(),
            reason: "input source must not be inside the agent workspace",
        });
    }
    Ok(metadata)
}

#[cfg(unix)]
fn copy_regular_file_no_symlink(
    source: &Path,
    agent_root: &Path,
    relative_destination: &Path,
    expected_metadata: &fs::Metadata,
) -> Result<(), AgentWorkspaceError> {
    use std::fs::OpenOptions;
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt};

    let mut input = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(source)
        .map_err(|err| AgentWorkspaceError::Io {
            path: source.to_path_buf(),
            source: err,
        })?;
    let actual_metadata = input.metadata().map_err(|err| AgentWorkspaceError::Io {
        path: source.to_path_buf(),
        source: err,
    })?;
    if !actual_metadata.file_type().is_file()
        || expected_metadata.dev() != actual_metadata.dev()
        || expected_metadata.ino() != actual_metadata.ino()
    {
        return Err(AgentWorkspaceError::SourceNotRegularFile(
            source.to_path_buf(),
        ));
    }

    let mut output = create_new_file_under_agent_root(agent_root, relative_destination)?;
    io::copy(&mut input, &mut output).map_err(|err| AgentWorkspaceError::Io {
        path: agent_root.join(relative_destination),
        source: err,
    })?;
    Ok(())
}

#[cfg(not(unix))]
fn copy_regular_file_no_symlink(
    _source: &Path,
    agent_root: &Path,
    relative_destination: &Path,
    _expected_metadata: &fs::Metadata,
) -> Result<(), AgentWorkspaceError> {
    Err(AgentWorkspaceError::InvalidPath {
        path: agent_root.join(relative_destination),
        reason: "agent workspace materialization requires Unix no-follow copy support",
    })
}

#[cfg(unix)]
fn create_new_file_under_agent_root(
    agent_root: &Path,
    relative_path: &Path,
) -> Result<fs::File, AgentWorkspaceError> {
    use std::ffi::{CString, OsStr};
    use std::os::fd::{AsRawFd, FromRawFd};
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::OpenOptionsExt;

    fn component_cstring(
        component: &OsStr,
        full_path: &Path,
    ) -> Result<CString, AgentWorkspaceError> {
        CString::new(component.as_bytes()).map_err(|_| AgentWorkspaceError::InvalidPath {
            path: full_path.to_path_buf(),
            reason: "agent workspace path component must not contain NUL bytes",
        })
    }

    fn open_dir_at(
        parent_fd: libc::c_int,
        component: &OsStr,
        full_path: &Path,
    ) -> Result<fs::File, AgentWorkspaceError> {
        let component = component_cstring(component, full_path)?;
        let fd = unsafe {
            libc::openat(
                parent_fd,
                component.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
            )
        };
        if fd < 0 {
            return Err(AgentWorkspaceError::Io {
                path: full_path.to_path_buf(),
                source: io::Error::last_os_error(),
            });
        }
        Ok(unsafe { fs::File::from_raw_fd(fd) })
    }

    fn mkdir_at(
        parent_fd: libc::c_int,
        component: &OsStr,
        full_path: &Path,
    ) -> Result<(), AgentWorkspaceError> {
        let component = component_cstring(component, full_path)?;
        let result = unsafe { libc::mkdirat(parent_fd, component.as_ptr(), 0o700) };
        if result == 0 {
            return Ok(());
        }
        let err = io::Error::last_os_error();
        if err.kind() == io::ErrorKind::AlreadyExists {
            return Ok(());
        }
        Err(AgentWorkspaceError::Io {
            path: full_path.to_path_buf(),
            source: err,
        })
    }

    fn create_file_at(
        parent_fd: libc::c_int,
        component: &OsStr,
        full_path: &Path,
    ) -> Result<fs::File, AgentWorkspaceError> {
        let component = component_cstring(component, full_path)?;
        let fd = unsafe {
            libc::openat(
                parent_fd,
                component.as_ptr(),
                libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_CLOEXEC | libc::O_NOFOLLOW,
                0o600,
            )
        };
        if fd < 0 {
            return Err(AgentWorkspaceError::Io {
                path: full_path.to_path_buf(),
                source: io::Error::last_os_error(),
            });
        }
        Ok(unsafe { fs::File::from_raw_fd(fd) })
    }

    let relative_path = validate_agent_relative_path(relative_path, AGENT_INPUT_DIR)
        .or_else(|_| validate_agent_relative_path(relative_path, AGENT_OUTPUT_DIR))
        .or_else(|_| validate_agent_relative_path(relative_path, AGENT_CURSOR_DIR))?;
    let full_path = agent_root.join(&relative_path);
    let components = relative_path.components().collect::<Vec<_>>();
    let (file_component, parent_components) =
        components
            .split_last()
            .ok_or_else(|| AgentWorkspaceError::InvalidPath {
                path: full_path.clone(),
                reason: "agent workspace file path must not be empty",
            })?;
    let mut current_path = agent_root.to_path_buf();
    let mut current_dir = {
        use std::fs::OpenOptions;
        OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW)
            .open(agent_root)
            .map_err(|err| AgentWorkspaceError::Io {
                path: agent_root.to_path_buf(),
                source: err,
            })?
    };

    for component in parent_components {
        let Component::Normal(name) = component else {
            return Err(AgentWorkspaceError::InvalidPath {
                path: full_path.clone(),
                reason: "agent workspace path must contain only normal components",
            });
        };
        current_path.push(name);
        mkdir_at(current_dir.as_raw_fd(), name, &current_path)?;
        current_dir = open_dir_at(current_dir.as_raw_fd(), name, &current_path)?;
    }

    let Component::Normal(file_name) = file_component else {
        return Err(AgentWorkspaceError::InvalidPath {
            path: full_path.clone(),
            reason: "agent workspace file path must contain only normal components",
        });
    };
    create_file_at(current_dir.as_raw_fd(), file_name, &full_path)
}

fn validate_agent_relative_path(
    path: &Path,
    required_first_component: &str,
) -> Result<PathBuf, AgentWorkspaceError> {
    let components = path.components().collect::<Vec<_>>();
    if components.is_empty() {
        return Err(AgentWorkspaceError::InvalidPath {
            path: path.to_path_buf(),
            reason: "path must not be empty",
        });
    }
    if components.len() < 2 {
        return Err(AgentWorkspaceError::InvalidPath {
            path: path.to_path_buf(),
            reason: "path must include a file name below the required agent workspace directory",
        });
    }
    if components
        .iter()
        .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(AgentWorkspaceError::InvalidPath {
            path: path.to_path_buf(),
            reason: "path must be relative and must not contain traversal components",
        });
    }
    if components.first().and_then(|component| match component {
        Component::Normal(value) => value.to_str(),
        _ => None,
    }) != Some(required_first_component)
    {
        return Err(AgentWorkspaceError::InvalidPath {
            path: path.to_path_buf(),
            reason: "path is not under the required agent workspace directory",
        });
    }
    Ok(path.to_path_buf())
}

fn permission_path(path: &Path) -> String {
    path.components()
        .map(|component| component.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

fn write_cursor_config(
    agent_root: &Path,
    relative_path: impl AsRef<Path>,
    allowed_reads: Vec<String>,
    expected_output: String,
) -> Result<(), AgentWorkspaceError> {
    let mut allow = allowed_reads
        .into_iter()
        .map(|path| format!("Read({path})"))
        .collect::<Vec<_>>();
    allow.push(format!("Write({expected_output})"));

    let config = CursorCliConfig {
        permissions: CursorPermissions {
            allow,
            deny: vec![
                "Read(.env)".to_owned(),
                "Read(debug/**)".to_owned(),
                "Read(../**)".to_owned(),
                "Write(input/**)".to_owned(),
                "Shell(*)".to_owned(),
            ],
        },
    };
    let json = serde_json::to_vec_pretty(&config).map_err(AgentWorkspaceError::Serialize)?;
    write_new_file_no_symlink(agent_root, relative_path.as_ref(), &json)
}

#[cfg(unix)]
fn write_new_file_no_symlink(
    agent_root: &Path,
    relative_path: &Path,
    bytes: &[u8],
) -> Result<(), AgentWorkspaceError> {
    use std::io::Write;

    let mut file = create_new_file_under_agent_root(agent_root, relative_path)?;
    file.write_all(bytes)
        .map_err(|err| AgentWorkspaceError::Io {
            path: agent_root.join(relative_path),
            source: err,
        })
}

#[cfg(not(unix))]
fn write_new_file_no_symlink(
    agent_root: &Path,
    relative_path: &Path,
    _bytes: &[u8],
) -> Result<(), AgentWorkspaceError> {
    Err(AgentWorkspaceError::InvalidPath {
        path: agent_root.join(relative_path),
        reason: "agent workspace materialization requires Unix no-follow write support",
    })
}
