use crate::storage_fs::sanitize_path_component;
use std::fs;
use std::path::{Path, PathBuf};

pub const WORKSPACES_ROOT_DIR: &str = "workspaces";
pub const MASKED_TRANSCRIPT_FILENAME: &str = "transcript_masked.md";
pub const TRANSCRIPT_MANIFEST_FILENAME: &str = "manifest.json";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MeetingWorkspaceLayout {
    base_dir: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MeetingWorkspacePaths {
    root: PathBuf,
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

    pub fn masked_transcript_path(&self) -> PathBuf {
        self.transcript_dir().join(MASKED_TRANSCRIPT_FILENAME)
    }

    pub fn transcript_manifest_path(&self) -> PathBuf {
        self.transcript_dir().join(TRANSCRIPT_MANIFEST_FILENAME)
    }

    pub fn ensure_base_dirs(&self) -> std::io::Result<()> {
        fs::create_dir_all(self.audio_dir())?;
        fs::create_dir_all(self.transcript_dir())?;
        fs::create_dir_all(self.context_dir())?;
        fs::create_dir_all(self.summary_dir())
    }

    /// Returns a path relative to the workspace root. Returns None if the
    /// provided path is outside the workspace (avoids leaking absolute paths).
    pub fn relative_path(&self, path: &Path) -> Option<PathBuf> {
        path.strip_prefix(&self.root).ok().map(PathBuf::from)
    }
}
