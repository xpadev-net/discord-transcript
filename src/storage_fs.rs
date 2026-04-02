use std::fmt::{Display, Formatter};
use std::fs;
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
        bytes: &[u8],
    ) -> Result<SavedChunk, ChunkStorageError>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalChunkStorage {
    pub base_dir: PathBuf,
}

impl LocalChunkStorage {
    pub fn new(base_dir: impl AsRef<Path>) -> Self {
        Self {
            base_dir: base_dir.as_ref().to_path_buf(),
        }
    }

    fn chunk_file_path(&self, meeting_id: &str, user_id: &str, sequence: u64) -> PathBuf {
        self.base_dir
            .join(meeting_id)
            .join(format!("{}_{}.wav", user_id, sequence))
    }
}

impl ChunkStorage for LocalChunkStorage {
    fn save_chunk(
        &self,
        meeting_id: &str,
        user_id: &str,
        sequence: u64,
        bytes: &[u8],
    ) -> Result<SavedChunk, ChunkStorageError> {
        let file_path = self.chunk_file_path(meeting_id, user_id, sequence);
        let dir = file_path
            .parent()
            .expect("chunk path should always have parent directory");
        fs::create_dir_all(dir).map_err(|err| ChunkStorageError::Io(err.to_string()))?;
        fs::write(&file_path, bytes).map_err(|err| ChunkStorageError::Io(err.to_string()))?;

        Ok(SavedChunk {
            path: file_path,
            size_bytes: bytes.len(),
        })
    }
}
