use crate::infrastructure::s3::{ObjectStore, S3Error, S3Settings};
use crate::infrastructure::storage_fs::{
    ChunkStorage, ChunkStorageError, LocalChunkStorage, SavedChunk,
};
use crate::infrastructure::workspace::MeetingWorkspacePaths;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Shared S3 object-store handle plus the layout rules that mirror local
/// `CHUNK_STORAGE_DIR` paths into object keys: a file at
/// `<storage_dir>/<rel>` is stored under `{key_prefix}<rel>` with `/`
/// separators. One instance is shared by the recording writer, the summary
/// worker's artifact uploads, the web playback endpoints, and retention
/// deletion so every consumer derives identical keys.
#[derive(Debug, Clone)]
pub struct RecordingObjectStore {
    objects: Arc<dyn ObjectStore>,
    /// `CHUNK_STORAGE_S3_KEY_PREFIX`, normalized to end with `/` or empty.
    key_prefix: String,
    /// `CHUNK_STORAGE_S3_PRESIGN_TTL_SECONDS`, already clamped by config.
    presign_ttl_seconds: u64,
    /// `CHUNK_STORAGE_DIR`: local workspace root mirrored into object keys.
    storage_dir: PathBuf,
}

impl RecordingObjectStore {
    /// Builds the shared store from validated config. S3-compatible logging
    /// happens at call sites (they know whether this is bot/worker/web).
    pub fn from_settings(
        settings: &S3Settings,
        storage_dir: impl Into<PathBuf>,
    ) -> Result<Self, S3Error> {
        let objects = crate::infrastructure::s3::S3Client::new(settings.clone())?;
        Ok(Self::new(
            Arc::new(objects),
            settings.key_prefix.clone(),
            settings.presign_ttl_seconds,
            storage_dir,
        ))
    }

    pub fn new(
        objects: Arc<dyn ObjectStore>,
        key_prefix: String,
        presign_ttl_seconds: u64,
        storage_dir: impl Into<PathBuf>,
    ) -> Self {
        Self {
            objects,
            key_prefix,
            presign_ttl_seconds,
            storage_dir: storage_dir.into(),
        }
    }

    pub fn store(&self) -> &Arc<dyn ObjectStore> {
        &self.objects
    }

    /// Object key mirroring `path` relative to `storage_dir`. `None` when the
    /// path lives outside the storage tree (e.g. the legacy flat layout).
    pub fn object_key(&self, path: &Path) -> Option<String> {
        let rel = path.strip_prefix(&self.storage_dir).ok()?;
        let rel = rel
            .components()
            .map(|component| component.as_os_str().to_string_lossy())
            .collect::<Vec<_>>()
            .join("/");
        if rel.is_empty() {
            return None;
        }
        Some(format!("{}{}", self.key_prefix, rel))
    }

    /// Uploads `bytes` under the key derived from `path`.
    pub fn put_bytes(&self, path: &Path, bytes: &[u8], content_type: &str) -> Result<(), String> {
        let Some(key) = self.object_key(path) else {
            return Err(format!(
                "path {} is outside the chunk storage dir",
                path.display()
            ));
        };
        self.objects
            .put_object(&key, bytes, content_type)
            .map_err(|err| err.to_string())
    }

    /// Reads `path` and uploads it under its mirrored key. Missing files are
    /// reported as `Ok(false)` so callers can treat them as already deleted.
    pub fn upload_file(&self, path: &Path, content_type: &str) -> Result<bool, String> {
        let bytes = match std::fs::read(path) {
            Ok(bytes) => bytes,
            Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(false),
            Err(err) => {
                return Err(format!("failed to read {}: {err}", path.display()));
            }
        };
        self.put_bytes(path, &bytes, content_type)?;
        Ok(true)
    }

    /// Deletes every object under `prefix` (a meeting's whole object tree).
    pub fn delete_prefix(&self, prefix: &str) -> Result<(), S3Error> {
        let keys = self.objects.list_keys(prefix)?;
        self.objects.delete_keys(&keys)
    }

    /// Presigned GET URL for `key` using the configured TTL.
    pub fn presigned_get(&self, key: &str) -> String {
        self.objects.presigned_get_url(
            key,
            std::time::Duration::from_secs(self.presign_ttl_seconds),
        )
    }
}

/// `ChunkStorage` for `CHUNK_STORAGE_BACKEND=s3`: every chunk is still
/// written to the local workspace as the recording-time staging copy (live
/// transcription and assembly read local paths) and is PUT to the object
/// store, which is the canonical durable copy. A failed upload fails the
/// whole save so the session's pending-chunk retry keeps re-attempting it.
#[derive(Debug)]
pub struct S3ChunkStorage {
    local: LocalChunkStorage,
    objects: RecordingObjectStore,
}

impl ChunkStorage for S3ChunkStorage {
    fn save_chunk(
        &self,
        meeting_id: &str,
        user_id: &str,
        sequence: u64,
        start_ms: u64,
        bytes: &[u8],
    ) -> Result<SavedChunk, ChunkStorageError> {
        let saved = self
            .local
            .save_chunk(meeting_id, user_id, sequence, start_ms, bytes)?;
        self.objects
            .put_bytes(&saved.path, bytes, "audio/wav")
            .map_err(ChunkStorageError::Remote)?;
        Ok(saved)
    }
}

/// Runtime-selected chunk storage. Both variants keep a local workspace so
/// downstream readers stay path-based; only the durable destination differs.
#[derive(Debug)]
pub enum MeetingChunkStorage {
    Local(LocalChunkStorage),
    S3(S3ChunkStorage),
}

impl MeetingChunkStorage {
    /// `objects == None` selects `Local`; `Some` selects `S3`.
    pub fn new(
        workspace: MeetingWorkspacePaths,
        meeting_id: impl Into<String>,
        objects: Option<RecordingObjectStore>,
    ) -> Self {
        let local = LocalChunkStorage::new(workspace, meeting_id);
        match objects {
            Some(objects) => Self::S3(S3ChunkStorage { local, objects }),
            None => Self::Local(local),
        }
    }

    /// Local workspace shared by both backends (staging files live here).
    pub fn workspace(&self) -> &MeetingWorkspacePaths {
        match self {
            Self::Local(local) => &local.workspace,
            Self::S3(s3) => &s3.local.workspace,
        }
    }

    /// For the S3 backend, uploads `bytes` under the key mirrored from
    /// `path`. No-op for `Local` so call sites don't branch.
    pub fn put_object_bytes(
        &self,
        path: &Path,
        bytes: &[u8],
        content_type: &str,
    ) -> Result<(), ChunkStorageError> {
        match self {
            Self::Local(_) => Ok(()),
            Self::S3(s3) => s3
                .objects
                .put_bytes(path, bytes, content_type)
                .map_err(ChunkStorageError::Remote),
        }
    }
}

impl ChunkStorage for MeetingChunkStorage {
    fn save_chunk(
        &self,
        meeting_id: &str,
        user_id: &str,
        sequence: u64,
        start_ms: u64,
        bytes: &[u8],
    ) -> Result<SavedChunk, ChunkStorageError> {
        match self {
            Self::Local(local) => local.save_chunk(meeting_id, user_id, sequence, start_ms, bytes),
            Self::S3(s3) => s3.save_chunk(meeting_id, user_id, sequence, start_ms, bytes),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::infrastructure::workspace::MeetingWorkspaceLayout;
    use std::sync::Mutex;
    use std::time::Duration;

    #[derive(Debug, Default)]
    struct FakeObjectStore {
        puts: Mutex<Vec<(String, Vec<u8>, String)>>,
        fail_puts: Mutex<bool>,
        lists: Mutex<Vec<String>>,
        deletes: Mutex<Vec<Vec<String>>>,
        list_result: Mutex<Vec<String>>,
    }

    impl ObjectStore for FakeObjectStore {
        fn put_object(&self, key: &str, body: &[u8], content_type: &str) -> Result<(), S3Error> {
            if *self.fail_puts.lock().unwrap() {
                return Err(S3Error::Status {
                    status: 500,
                    detail: "injected put failure".to_owned(),
                });
            }
            self.puts.lock().unwrap().push((
                key.to_owned(),
                body.to_vec(),
                content_type.to_owned(),
            ));
            Ok(())
        }

        fn head_object(&self, _key: &str) -> Result<bool, S3Error> {
            Ok(true)
        }

        fn list_keys(&self, prefix: &str) -> Result<Vec<String>, S3Error> {
            self.lists.lock().unwrap().push(prefix.to_owned());
            Ok(self.list_result.lock().unwrap().clone())
        }

        fn delete_keys(&self, keys: &[String]) -> Result<(), S3Error> {
            self.deletes.lock().unwrap().push(keys.to_vec());
            Ok(())
        }

        fn presigned_get_url(&self, key: &str, ttl: Duration) -> String {
            format!("https://example.test/{key}?ttl={}", ttl.as_secs())
        }

        fn endpoint_label(&self) -> String {
            "fake".to_owned()
        }
    }

    fn temp_dir(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "discord_transcript_storage_s3_{name}_{}",
            std::process::id()
        ))
    }

    fn layout_meeting(base: &std::path::Path) -> MeetingWorkspacePaths {
        MeetingWorkspaceLayout::new(base).for_meeting("g1", "vc1", "m1")
    }

    fn fake_store() -> (Arc<FakeObjectStore>, Arc<dyn ObjectStore>) {
        let fake = Arc::new(FakeObjectStore::default());
        let dyn_store: Arc<dyn ObjectStore> = fake.clone();
        (fake, dyn_store)
    }

    #[test]
    fn object_key_mirrors_storage_relative_paths() {
        let (_, dyn_store) = fake_store();
        let store = RecordingObjectStore::new(dyn_store, "pfx/".to_owned(), 900, "/data");
        assert_eq!(
            store.object_key(std::path::Path::new("/data/workspaces/g/vc/m/audio/x.wav")),
            Some("pfx/workspaces/g/vc/m/audio/x.wav".to_owned())
        );
    }

    #[test]
    fn object_key_rejects_paths_outside_storage_dir() {
        let (_, dyn_store) = fake_store();
        let store = RecordingObjectStore::new(dyn_store, String::new(), 900, "/data");
        assert_eq!(
            store.object_key(std::path::Path::new("/elsewhere/x.wav")),
            None
        );
        assert_eq!(store.object_key(std::path::Path::new("/data")), None);
    }

    #[test]
    fn s3_chunk_storage_stages_locally_and_puts_object() {
        let base = temp_dir("put_ok");
        let _ = std::fs::remove_dir_all(&base);
        let (fake, dyn_store) = fake_store();
        let objects = RecordingObjectStore::new(dyn_store, String::new(), 900, &base);
        let storage = MeetingChunkStorage::new(layout_meeting(&base), "m1", Some(objects));

        let saved = storage
            .save_chunk("m1", "u1", 1, 0, b"wav-data")
            .expect("save should succeed");

        assert!(saved.path.exists());
        let rel = saved.path.strip_prefix(&base).unwrap().to_path_buf();
        let expected_key = rel
            .components()
            .map(|c| c.as_os_str().to_string_lossy())
            .collect::<Vec<_>>()
            .join("/");
        let puts = fake.puts.lock().unwrap();
        assert_eq!(puts.len(), 1);
        assert_eq!(puts[0].0, expected_key);
        assert_eq!(puts[0].1, b"wav-data");
        assert_eq!(puts[0].2, "audio/wav");
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn s3_chunk_storage_reports_remote_error_but_keeps_staging_file() {
        let base = temp_dir("put_fail");
        let _ = std::fs::remove_dir_all(&base);
        let (fake, dyn_store) = fake_store();
        *fake.fail_puts.lock().unwrap() = true;
        let objects = RecordingObjectStore::new(dyn_store, String::new(), 900, &base);
        let storage = MeetingChunkStorage::new(layout_meeting(&base), "m1", Some(objects));

        let err = storage
            .save_chunk("m1", "u1", 1, 0, b"wav-data")
            .expect_err("put failure must surface as Remote");
        match err {
            ChunkStorageError::Remote(detail) => {
                assert!(detail.contains("injected put failure"));
            }
            other => panic!("expected Remote error, got {other:?}"),
        }
        // The staging copy was still written so a retry can re-upload it.
        let staged = layout_meeting(&base).audio_dir().join("u1_1_0.wav");
        assert!(staged.exists());
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn local_backend_put_object_bytes_is_noop() {
        let base = temp_dir("local_noop");
        let _ = std::fs::remove_dir_all(&base);
        let storage = MeetingChunkStorage::new(layout_meeting(&base), "m1", None);
        storage
            .put_object_bytes(std::path::Path::new("/anywhere"), b"x", "audio/wav")
            .expect("local backend put is a no-op");
    }

    #[test]
    fn upload_file_reports_missing_source() {
        let base = temp_dir("missing");
        let _ = std::fs::remove_dir_all(&base);
        let (_, dyn_store) = fake_store();
        let store = RecordingObjectStore::new(dyn_store, String::new(), 900, &base);
        let missing = base.join("workspaces/g/vc/m/audio/none.wav");
        assert!(!store.upload_file(&missing, "audio/wav").unwrap());
    }

    #[test]
    fn delete_prefix_lists_then_deletes() {
        let (fake, dyn_store) = fake_store();
        *fake.list_result.lock().unwrap() = vec!["a".to_owned(), "b".to_owned()];
        let store = RecordingObjectStore::new(dyn_store, String::new(), 900, "/data");
        store.delete_prefix("workspaces/g/vc/m/").unwrap();
        assert_eq!(fake.lists.lock().unwrap()[0], "workspaces/g/vc/m/");
        assert_eq!(
            fake.deletes.lock().unwrap()[0],
            vec!["a".to_owned(), "b".to_owned()]
        );
    }

    #[test]
    fn presigned_get_uses_configured_ttl() {
        let (_, dyn_store) = fake_store();
        let store = RecordingObjectStore::new(dyn_store, String::new(), 42, "/data");
        assert_eq!(store.presigned_get("k"), "https://example.test/k?ttl=42");
    }
}
