use crate::infrastructure::workspace::MeetingWorkspacePaths;
use std::fmt::{Display, Formatter};
use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SavedChunk {
    pub path: PathBuf,
    pub size_bytes: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChunkStorageError {
    Io(String),
}

impl Display for ChunkStorageError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(err) => write!(f, "filesystem error: {err}"),
        }
    }
}

impl std::error::Error for ChunkStorageError {}

pub trait ChunkStorage {
    fn save_chunk(
        &self,
        meeting_id: &str,
        user_id: &str,
        sequence: u64,
        start_ms: u64,
        bytes: &[u8],
    ) -> Result<SavedChunk, ChunkStorageError>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalChunkStorage {
    pub workspace: MeetingWorkspacePaths,
    pub meeting_id: String,
}

impl LocalChunkStorage {
    pub fn new(workspace: MeetingWorkspacePaths, meeting_id: impl Into<String>) -> Self {
        Self {
            workspace,
            meeting_id: meeting_id.into(),
        }
    }

    fn chunk_file_path(&self, user_id: &str, sequence: u64, start_ms: u64) -> PathBuf {
        let safe_user_id = sanitize_path_component(user_id);
        self.workspace
            .audio_dir()
            .join(format!("{}_{}_{}.wav", safe_user_id, sequence, start_ms))
    }
}

fn temp_chunk_file_path(file_path: &Path) -> Result<PathBuf, ChunkStorageError> {
    let file_name = file_path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| ChunkStorageError::Io("chunk path has no file name".to_owned()))?;
    Ok(file_path.with_file_name(format!("{file_name}.tmp")))
}

fn write_chunk_atomically(file_path: &Path, bytes: &[u8]) -> Result<(), ChunkStorageError> {
    let temp_path = temp_chunk_file_path(file_path)?;
    let write_result = (|| {
        let mut file =
            File::create(&temp_path).map_err(|err| ChunkStorageError::Io(err.to_string()))?;
        file.write_all(bytes)
            .map_err(|err| ChunkStorageError::Io(err.to_string()))?;
        file.sync_all()
            .map_err(|err| ChunkStorageError::Io(err.to_string()))
    })();
    if let Err(err) = write_result {
        let _ = fs::remove_file(&temp_path);
        return Err(err);
    }

    fs::rename(&temp_path, file_path).map_err(|err| {
        let _ = fs::remove_file(&temp_path);
        ChunkStorageError::Io(err.to_string())
    })?;

    if let Some(dir) = file_path.parent()
        && let Ok(dir_file) = File::open(dir)
    {
        let _ = dir_file.sync_all();
    }
    Ok(())
}

pub fn sanitize_path_component(input: &str) -> String {
    let sanitized: String = input
        .replace(['/', '\\'], "_")
        .replace("..", "_")
        .chars()
        .filter(|c| c.is_alphanumeric() || *c == '-' || *c == '_' || *c == '.')
        .collect();

    // Guard against empty result or lone "." / ".." which have special filesystem meaning.
    // Append a short hash of the original input to prevent collisions between
    // different raw IDs that all sanitize to the fallback.
    if sanitized.is_empty() || sanitized == "." {
        let hash = simple_hash(input);
        return format!("unknown_{hash:016x}");
    }
    sanitized
}

/// Cheap, deterministic hash for filesystem-safe fallback names.
fn simple_hash(input: &str) -> u64 {
    // FNV-1a 64-bit
    let mut h: u64 = 0xcbf29ce484222325;
    for b in input.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

impl ChunkStorage for LocalChunkStorage {
    fn save_chunk(
        &self,
        meeting_id: &str,
        user_id: &str,
        sequence: u64,
        start_ms: u64,
        bytes: &[u8],
    ) -> Result<SavedChunk, ChunkStorageError> {
        let file_path = self.chunk_file_path(user_id, sequence, start_ms);
        if meeting_id != self.meeting_id {
            tracing::warn!(
                expected = %self.meeting_id,
                provided = %meeting_id,
                "meeting_id mismatch while saving chunk; proceeding with stored workspace"
            );
        }
        let Some(dir) = file_path.parent() else {
            return Err(ChunkStorageError::Io(
                "chunk path has no parent directory".to_owned(),
            ));
        };
        fs::create_dir_all(dir).map_err(|err| ChunkStorageError::Io(err.to_string()))?;
        write_chunk_atomically(&file_path, bytes)?;

        Ok(SavedChunk {
            path: file_path,
            size_bytes: bytes.len(),
        })
    }
}
