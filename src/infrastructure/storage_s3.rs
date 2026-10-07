use crate::infrastructure::s3::{ObjectStore, S3Error, S3Settings};
use crate::infrastructure::storage_fs::{
    ChunkStorage, ChunkStorageError, LocalChunkStorage, SavedChunk,
};
use crate::infrastructure::workspace::MeetingWorkspacePaths;
use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, sync_channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tracing::{info, warn};

/// Shared S3 object-store handle plus the layout rules that mirror local
/// `CHUNK_STORAGE_DIR` paths into object keys: a file at
/// `<storage_dir>/<rel>` is stored under `{key_prefix}<rel>` with `/`
/// separators. One instance is shared by the recording writer, the summary
/// worker's artifact uploads, the web playback endpoints, and retention
/// deletion so every consumer derives identical keys.
/// How many upload tasks may wait in the background queue before producers
/// get backpressure (`save_chunk` then reports `Remote` so the session's
/// pending-chunk retry kicks in).
const UPLOAD_QUEUE_BOUND: usize = 64;
/// Total inline bytes the queue (channel + delayed retries) may hold.
/// Recording chunks carry their payload in the task so `save_chunk` keeps
/// its Remote-on-saturation semantics; file-backed tasks only hold a path
/// and are read by the worker at PUT time, so arbitrarily large playback
/// files never count against — or get rejected by — this cap.
const UPLOAD_MAX_PENDING_BYTES: usize = 256 * 1024 * 1024;
/// Total tasks the queue (channel + delayed retries) may hold. File-backed
/// tasks hold no inline bytes, so only a task count stops an upload backlog
/// from growing without limit during a long outage.
const UPLOAD_MAX_PENDING_TASKS: usize = 1024;
/// How long `cancel_prefix` waits for an in-flight PUT under the prefix
/// before giving up (the S3 request timeout bounds a single PUT at 60s).
/// On expiry the delete is aborted with an error instead of racing a
/// still-running upload.
const CANCEL_INFLIGHT_WAIT: Duration = Duration::from_secs(65);
/// How often the upload worker re-fetches remote tombstone objects so a
/// delete issued by another process stops pending uploads here as well.
const TOMBSTONE_REFRESH_INTERVAL: Duration = Duration::from_secs(10);

/// Task payload: chunk/audio bytes already in memory, or a file the worker
/// reads lazily (so large artifacts never sit in queue memory).
#[derive(Debug)]
enum UploadSource {
    Inline(Vec<u8>),
    File(PathBuf),
}

#[derive(Debug)]
struct UploadTask {
    key: String,
    source: UploadSource,
    content_type: String,
    path: PathBuf,
    /// Monotonic enqueue order — a task is skipped when a newer write for the
    /// same key exists (e.g. a fresher `ssrc_mapping.json` snapshot).
    generation: u64,
    attempts: u32,
    not_before: Instant,
}

impl UploadTask {
    /// Bytes counted against `UPLOAD_MAX_PENDING_BYTES` — only inline
    /// payloads occupy queue memory.
    fn counted_bytes(&self) -> usize {
        match &self.source {
            UploadSource::Inline(bytes) => bytes.len(),
            UploadSource::File(_) => 0,
        }
    }
}

/// Generation tracking for one key: `latest` supersedes older queued
/// writes, while `outstanding` holds every generation that is queued,
/// delayed, or in-flight. `latest` may only be garbage-collected once
/// `outstanding` is empty — removing it earlier would let an older retried
/// task overwrite a newer snapshot (e.g. `ssrc_mapping.json`).
#[derive(Debug, Default)]
struct GenerationState {
    latest: HashMap<String, u64>,
    outstanding: HashMap<String, BTreeSet<u64>>,
}

impl GenerationState {
    /// Records a fresh enqueue and returns whether any older write for the
    /// key is still outstanding.
    fn track(&mut self, key: &str, generation: u64) {
        self.latest.insert(key.to_owned(), generation);
        self.outstanding
            .entry(key.to_owned())
            .or_default()
            .insert(generation);
    }

    /// A task is superseded when a newer generation was enqueued for its key.
    fn is_superseded(&self, key: &str, generation: u64) -> bool {
        self.latest
            .get(key)
            .is_some_and(|latest| *latest > generation)
    }

    /// Releases one generation; drops the whole marker once no queued or
    /// in-flight write for the key remains.
    fn settle(&mut self, key: &str, generation: u64) {
        if let Some(set) = self.outstanding.get_mut(key) {
            set.remove(&generation);
            if !set.is_empty() {
                return;
            }
        }
        self.outstanding.remove(key);
        self.latest.remove(key);
    }

    /// Rolls back a generation that was enqueued but never queued (rejected
    /// enqueue): restores `latest` to the newest still-outstanding write so
    /// the rejected write cannot supersede accepted ones.
    fn untrack(&mut self, key: &str, generation: u64) {
        if let Some(set) = self.outstanding.get_mut(key) {
            set.remove(&generation);
            if !set.is_empty() {
                if let Some(newest) = set.iter().next_back() {
                    self.latest.insert(key.to_owned(), *newest);
                }
                return;
            }
        }
        self.outstanding.remove(key);
        self.latest.remove(key);
    }
}

/// Delete/cancel bookkeeping shared by producers, the worker, and
/// `cancel_prefix`. `prefixes` and `inflight` live under one lock so the
/// worker's "check cancellation, then register the PUT" sequence is atomic:
/// a prefix pushed here can no longer gain a new in-flight upload.
#[derive(Debug, Default)]
struct CancelState {
    /// Cancelled prefixes — local `delete_prefix` pushes plus remote
    /// tombstones fetched from the object store by the worker. Matching
    /// queued tasks are skipped and new enqueues rejected, so a retried
    /// upload can never recreate a deleted recording.
    prefixes: Vec<String>,
    /// Keys currently mid-PUT — `cancel_prefix` waits for these to finish
    /// before the caller deletes, keeping delete-after-upload ordering.
    inflight: HashSet<String>,
}

impl CancelState {
    fn is_cancelled(&self, key: &str) -> bool {
        self.prefixes
            .iter()
            .any(|prefix| key.starts_with(prefix.as_str()))
    }

    /// Cancels `key` or registers its PUT as in-flight, atomically.
    /// Returns false when the key is under an already-cancelled prefix.
    fn begin_upload(&mut self, key: &str) -> bool {
        if self.is_cancelled(key) {
            return false;
        }
        self.inflight.insert(key.to_owned());
        true
    }
}

/// Single worker thread draining queued PUTs so object writes never run on
/// the shared voice-ingest path. Failed uploads retry with exponential
/// backoff on a delayed in-worker queue — without an attempt cap, so an S3
/// outage resumes by itself once the service recovers instead of stranding
/// chunks that only exist as local staging files. Memory is bounded by
/// `UPLOAD_MAX_PENDING_BYTES` and task count by `UPLOAD_MAX_PENDING_TASKS`;
/// saturation surfaces as `Remote` so session-level pending retries and
/// `audio_loss` metrics still cover a prolonged outage.
///
/// Deletes write a tombstone object under `{key_prefix}.tombstones/`; the
/// worker refreshes that list every `TOMBSTONE_REFRESH_INTERVAL` while
/// tasks are pending, so deletes issued by a different process (web vs.
/// standalone worker) cancel uploads here too.
///
/// Drop disconnects the channel and joins the worker: during drain each
/// queued task is attempted exactly once (no retries), so shutdown waits for
/// in-flight uploads but stays bounded.
#[derive(Debug)]
struct UploadQueue {
    /// `Option` so `Drop` can close the channel before joining the worker.
    sender: Option<SyncSender<UploadTask>>,
    worker: Option<std::thread::JoinHandle<()>>,
    /// Tasks in flight or waiting; test-only drain signal.
    pending: Arc<AtomicUsize>,
    /// Inline bytes held by queued + delayed tasks; bounds total memory.
    pending_bytes: Arc<AtomicUsize>,
    /// Per-key write ordering state (latest generation + outstanding set).
    generations: Arc<Mutex<GenerationState>>,
    next_generation: AtomicU64,
    /// Cancellation state shared with the worker (see `CancelState`).
    cancel: Arc<Mutex<CancelState>>,
    /// Object store the worker PUTs through; also used by `cancel_prefix`
    /// to write tombstones visible to other processes' queues.
    objects: Arc<dyn ObjectStore>,
    /// `{key_prefix}.tombstones/` — one object per deleted prefix.
    tombstone_prefix: String,
}

impl UploadQueue {
    fn start(
        objects: Arc<dyn ObjectStore>,
        retry_base: Duration,
        tombstone_prefix: String,
    ) -> Self {
        let (sender, receiver) = sync_channel::<UploadTask>(UPLOAD_QUEUE_BOUND);
        let pending = Arc::new(AtomicUsize::new(0));
        let pending_bytes = Arc::new(AtomicUsize::new(0));
        let generations = Arc::new(Mutex::new(GenerationState::default()));
        let cancel = Arc::new(Mutex::new(CancelState::default()));
        let worker_pending = Arc::clone(&pending);
        let worker_bytes = Arc::clone(&pending_bytes);
        let worker_generations = Arc::clone(&generations);
        let worker_cancel = Arc::clone(&cancel);
        let worker_objects = Arc::clone(&objects);
        let worker_tombstone_prefix = tombstone_prefix.clone();
        let worker = std::thread::Builder::new()
            .name("s3-upload".to_owned())
            .spawn(move || {
                Self::worker(
                    receiver,
                    worker_objects,
                    worker_pending,
                    worker_bytes,
                    worker_generations,
                    worker_cancel,
                    worker_tombstone_prefix,
                    retry_base,
                )
            })
            .expect("s3 upload worker must spawn");
        Self {
            sender: Some(sender),
            worker: Some(worker),
            pending,
            pending_bytes,
            generations,
            next_generation: AtomicU64::new(0),
            cancel,
            objects,
            tombstone_prefix,
        }
    }

    /// Refresh remote tombstones: deletes issued in another process write
    /// tombstone objects, and listing them here cancels our pending uploads
    /// under the same prefixes.
    fn refresh_remote_tombstones(
        objects: &Arc<dyn ObjectStore>,
        tombstone_prefix: &str,
        cancel: &Arc<Mutex<CancelState>>,
    ) {
        match objects.list_keys(tombstone_prefix) {
            Ok(keys) => {
                let mut state = cancel.lock().unwrap();
                for prefix in keys
                    .iter()
                    .filter_map(|key| key.strip_prefix(tombstone_prefix))
                {
                    if !state.prefixes.iter().any(|known| known == prefix) {
                        state.prefixes.push(prefix.to_owned());
                    }
                }
            }
            Err(err) => warn!(
                error = %err,
                "failed to refresh remote upload tombstones; keeping the stale set"
            ),
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn worker(
        receiver: Receiver<UploadTask>,
        objects: Arc<dyn ObjectStore>,
        pending: Arc<AtomicUsize>,
        pending_bytes: Arc<AtomicUsize>,
        generations: Arc<Mutex<GenerationState>>,
        cancel: Arc<Mutex<CancelState>>,
        tombstone_prefix: String,
        retry_base: Duration,
    ) {
        // A task counts down pending/pending_bytes exactly once, whichever way
        // it leaves: delivered, superseded, cancelled, or abandoned in drain.
        let settle = |task: &UploadTask| {
            pending.fetch_sub(1, Ordering::SeqCst);
            pending_bytes.fetch_sub(task.counted_bytes(), Ordering::SeqCst);
            generations
                .lock()
                .unwrap()
                .settle(&task.key, task.generation);
        };
        let mut delayed: Vec<UploadTask> = Vec::new();
        let mut disconnected = false;
        // `None` until the first task triggers a fetch, so remote tombstones
        // apply from the very first upload attempt after process start.
        let mut tombstones_fetched_at: Option<Instant> = None;
        loop {
            let now = Instant::now();
            let next_due = delayed
                .iter()
                .map(|task| task.not_before)
                .min()
                .unwrap_or(now + Duration::from_secs(3600));
            let wait = next_due.saturating_duration_since(now);
            if disconnected {
                if delayed.is_empty() {
                    return;
                }
                // Drain attempts everything once without waiting for backoff.
                for task in &mut delayed {
                    task.not_before = now;
                }
            } else {
                match receiver.recv_timeout(wait.max(Duration::from_millis(1))) {
                    Ok(task) => delayed.push(task),
                    Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                    Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                        disconnected = true;
                    }
                }
            }
            let now = Instant::now();
            let mut i = 0;
            while i < delayed.len() {
                if delayed[i].not_before > now {
                    i += 1;
                    continue;
                }
                let mut task = delayed.remove(i);
                if tombstones_fetched_at.is_none_or(|t| t.elapsed() >= TOMBSTONE_REFRESH_INTERVAL) {
                    Self::refresh_remote_tombstones(&objects, &tombstone_prefix, &cancel);
                    tombstones_fetched_at = Some(Instant::now());
                }
                if generations
                    .lock()
                    .unwrap()
                    .is_superseded(&task.key, task.generation)
                {
                    settle(&task);
                    continue;
                }
                // File-backed tasks stream from disk at upload time so large
                // artifacts never sit in queue memory. A vanished source
                // (e.g. staging deleted by retention) is permanent.
                if let UploadSource::File(path) = &task.source
                    && !path.exists()
                {
                    warn!(
                        key = %task.key,
                        path = %task.path.display(),
                        "upload source file is gone; dropping task"
                    );
                    settle(&task);
                    continue;
                }
                // Check cancellation and mark the key in-flight atomically —
                // `cancel_prefix` cannot slip a delete between the two.
                if !cancel.lock().unwrap().begin_upload(&task.key) {
                    // A tombstoned task may still have PUT an object in a
                    // racing earlier attempt; delete it so a cancelled
                    // upload cannot leave a deleted recording behind.
                    if let Err(err) = objects.delete_keys(std::slice::from_ref(&task.key)) {
                        warn!(
                            key = %task.key,
                            error = %err,
                            "failed to clean up object under deleted prefix"
                        );
                    }
                    settle(&task);
                    continue;
                }
                let result = match &task.source {
                    UploadSource::Inline(bytes) => {
                        objects.put_object(&task.key, bytes, &task.content_type)
                    }
                    UploadSource::File(path) => {
                        objects.put_file(&task.key, path, &task.content_type)
                    }
                };
                cancel.lock().unwrap().inflight.remove(&task.key);
                match result {
                    Ok(()) => settle(&task),
                    Err(err) => {
                        task.attempts += 1;
                        if disconnected {
                            // Shutdown drain: one attempt per task, then drop.
                            warn!(
                                key = %task.key,
                                path = %task.path.display(),
                                error = %err,
                                "recording upload failed during shutdown drain; only the local staging copy remains"
                            );
                            settle(&task);
                        } else {
                            let backoff = retry_base * (1 << task.attempts.min(5));
                            warn!(
                                key = %task.key,
                                attempts = task.attempts,
                                error = %err,
                                "recording upload failed; retrying"
                            );
                            task.not_before = Instant::now() + backoff;
                            delayed.push(task);
                        }
                    }
                }
            }
        }
    }

    /// Tombstones `prefix`: queued tasks under it are skipped at process
    /// time, future enqueues are rejected, and a tombstone object is written
    /// so upload workers in OTHER processes learn the delete on their next
    /// `TOMBSTONE_REFRESH_INTERVAL` refresh. This call then waits for any
    /// currently-executing PUT under the prefix to finish so a subsequent
    /// object delete cannot be undone by an upload already in flight.
    ///
    /// Errors when an in-flight upload does not finish within
    /// `CANCEL_INFLIGHT_WAIT` (a multipart artifact upload can legitimately
    /// outlive the wait). The delete is aborted and must be retried by the
    /// caller; the tombstone stays in place so the straggler is skipped and
    /// cleaned up by its own next attempt.
    fn cancel_prefix(&self, prefix: &str) -> Result<(), S3Error> {
        self.cancel.lock().unwrap().prefixes.push(prefix.to_owned());
        // The tombstone goes up before the drain wait so foreign workers
        // observe the delete as early as possible.
        self.objects.put_object(
            &format!("{}{}", self.tombstone_prefix, prefix),
            prefix.as_bytes(),
            "text/plain",
        )?;
        let deadline = Instant::now() + CANCEL_INFLIGHT_WAIT;
        while Instant::now() < deadline {
            let active = self
                .cancel
                .lock()
                .unwrap()
                .inflight
                .iter()
                .any(|key| key.starts_with(prefix));
            if !active {
                return Ok(());
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        Err(S3Error::Http(format!(
            "in-flight uploads under {prefix} did not finish within {}s; delete aborted",
            CANCEL_INFLIGHT_WAIT.as_secs()
        )))
    }

    /// Undoes the accounting and the generation marker for a task that never
    /// reached the channel, so it cannot supersede an earlier queued write
    /// for the same key.
    fn revert_enqueue(&self, task: &UploadTask, size: usize) {
        self.pending.fetch_sub(1, Ordering::SeqCst);
        self.pending_bytes.fetch_sub(size, Ordering::SeqCst);
        self.generations
            .lock()
            .unwrap()
            .untrack(&task.key, task.generation);
    }

    /// Enqueues a PUT without blocking the caller.
    fn enqueue(&self, mut task: UploadTask) -> Result<(), String> {
        if self.cancel.lock().unwrap().is_cancelled(&task.key) {
            return Err("recording upload cancelled for deleted prefix".to_owned());
        }
        let size = task.counted_bytes();
        let used_bytes = self.pending_bytes.fetch_add(size, Ordering::SeqCst) + size;
        if used_bytes > UPLOAD_MAX_PENDING_BYTES {
            self.pending_bytes.fetch_sub(size, Ordering::SeqCst);
            return Err("recording upload queue byte limit exceeded".to_owned());
        }
        // The task cap covers file-backed uploads too — they hold no inline
        // bytes, so without it a long outage grows the delayed-retry backlog
        // without limit.
        let used_tasks = self.pending.fetch_add(1, Ordering::SeqCst) + 1;
        if used_tasks > UPLOAD_MAX_PENDING_TASKS {
            self.pending.fetch_sub(1, Ordering::SeqCst);
            self.pending_bytes.fetch_sub(size, Ordering::SeqCst);
            return Err("recording upload queue task limit exceeded".to_owned());
        }
        task.generation = self.next_generation.fetch_add(1, Ordering::SeqCst);
        self.generations
            .lock()
            .unwrap()
            .track(&task.key, task.generation);
        let Some(sender) = &self.sender else {
            self.revert_enqueue(&task, size);
            return Err("recording upload worker is gone".to_owned());
        };
        match sender.try_send(task) {
            Ok(()) => Ok(()),
            Err(std::sync::mpsc::TrySendError::Full(task)) => {
                self.revert_enqueue(&task, size);
                Err("recording upload queue is full".to_owned())
            }
            Err(std::sync::mpsc::TrySendError::Disconnected(task)) => {
                self.revert_enqueue(&task, size);
                Err("recording upload worker is gone".to_owned())
            }
        }
    }

    #[cfg(test)]
    fn wait_idle(&self) {
        for _ in 0..500 {
            if self.pending.load(Ordering::SeqCst) == 0 {
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!("s3 upload queue did not drain in time");
    }
}

impl Drop for UploadQueue {
    fn drop(&mut self) {
        // Close the channel so the worker drains what's left, then wait for
        // it. Each remaining task is attempted once, so this is bounded by
        // task count × request timeout.
        self.sender.take();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

#[derive(Debug, Clone)]
pub struct RecordingObjectStore {
    objects: Arc<dyn ObjectStore>,
    /// `CHUNK_STORAGE_S3_KEY_PREFIX`, normalized to end with `/` or empty.
    key_prefix: String,
    /// `CHUNK_STORAGE_S3_PRESIGN_TTL_SECONDS`, already clamped by config.
    presign_ttl_seconds: u64,
    /// `CHUNK_STORAGE_DIR`: local workspace root mirrored into object keys.
    storage_dir: PathBuf,
    uploads: Arc<UploadQueue>,
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
        Self::with_retry_base(
            objects,
            key_prefix,
            presign_ttl_seconds,
            storage_dir,
            Duration::from_secs(1),
        )
    }

    fn with_retry_base(
        objects: Arc<dyn ObjectStore>,
        key_prefix: String,
        presign_ttl_seconds: u64,
        storage_dir: impl Into<PathBuf>,
        retry_base: Duration,
    ) -> Self {
        let tombstone_prefix = format!("{}.tombstones/", key_prefix);
        Self {
            uploads: Arc::new(UploadQueue::start(
                objects.clone(),
                retry_base,
                tombstone_prefix,
            )),
            objects,
            key_prefix,
            presign_ttl_seconds,
            storage_dir: storage_dir.into(),
        }
    }

    pub fn store(&self) -> &Arc<dyn ObjectStore> {
        &self.objects
    }

    /// Blocks until every queued upload finished (success or final failure).
    #[cfg(test)]
    pub fn wait_uploads_idle(&self) {
        self.uploads.wait_idle();
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

    /// Enqueues `bytes` for upload under the key derived from `path` and
    /// returns immediately. Only queue-level failures surface; the worker
    /// retries object-store failures itself.
    pub fn put_bytes(&self, path: &Path, bytes: &[u8], content_type: &str) -> Result<(), String> {
        let Some(key) = self.object_key(path) else {
            return Err(format!(
                "path {} is outside the chunk storage dir",
                path.display()
            ));
        };
        self.uploads.enqueue(UploadTask {
            key,
            source: UploadSource::Inline(bytes.to_vec()),
            content_type: content_type.to_owned(),
            path: path.to_path_buf(),
            generation: 0, // assigned by `enqueue`
            attempts: 0,
            not_before: Instant::now(),
        })
    }

    /// Queues `path` for upload under its mirrored key; the worker reads the
    /// file at PUT time so arbitrarily large playback files never occupy
    /// queue memory or trip the inline byte cap. Missing files are reported
    /// as `Ok(false)` so callers can treat them as already deleted.
    pub fn upload_file(&self, path: &Path, content_type: &str) -> Result<bool, String> {
        let Some(key) = self.object_key(path) else {
            return Ok(false);
        };
        if !path.is_file() {
            return Ok(false);
        }
        self.uploads.enqueue(UploadTask {
            key,
            source: UploadSource::File(path.to_path_buf()),
            content_type: content_type.to_owned(),
            path: path.to_path_buf(),
            generation: 0, // assigned by `enqueue`
            attempts: 0,
            not_before: Instant::now(),
        })?;
        Ok(true)
    }

    /// Lists all object keys under `prefix` (paginates as needed).
    pub fn list_keys(&self, prefix: &str) -> Result<Vec<String>, S3Error> {
        self.objects.list_keys(prefix)
    }

    /// Object key prefix mirroring a directory under `storage_dir`.
    pub fn object_prefix(&self, dir: &Path) -> Option<String> {
        self.object_key(dir).map(|key| format!("{key}/"))
    }

    /// Whether an object exists at the key mirrored from `path`. `Ok(false)`
    /// for paths outside the storage root, which have no mirror.
    pub fn object_exists(&self, path: &Path) -> Result<bool, S3Error> {
        let Some(key) = self.object_key(path) else {
            return Ok(false);
        };
        self.objects.head_object(&key)
    }

    /// Presigned GET URL for the object mirrored from `path`. `None` when the
    /// path is outside the storage root.
    pub fn presigned_get_for_path(&self, path: &Path) -> Option<String> {
        self.object_key(path).map(|key| self.presigned_get(&key))
    }

    /// Deletes every object under `prefix` (a meeting's whole object tree).
    /// Pending and in-flight uploads under the prefix are cancelled first —
    /// in this process AND, via a tombstone object, in any other process —
    /// so a retried PUT cannot recreate deleted recordings.
    /// Returns how many keys were deleted.
    pub fn delete_prefix(&self, prefix: &str) -> Result<usize, S3Error> {
        self.uploads.cancel_prefix(prefix)?;
        let keys = self.objects.list_keys(prefix)?;
        let deleted = keys.len();
        self.objects.delete_keys(&keys)?;
        Ok(deleted)
    }

    /// Deletes only recording objects under a legacy flat meeting prefix:
    /// top-level `*.wav` files and the `speakers/` subtree. Other objects
    /// under the prefix (transcripts, context, ...) are left in place.
    /// Pending and in-flight uploads under the prefix are cancelled first.
    /// Returns how many keys were deleted.
    pub fn delete_legacy_recording_prefix(&self, prefix: &str) -> Result<usize, S3Error> {
        self.uploads.cancel_prefix(prefix)?;
        let keys = self
            .objects
            .list_keys(prefix)?
            .into_iter()
            .filter_map(|key| {
                let rel = key.strip_prefix(prefix)?;
                ((rel.ends_with(".wav") && !rel.contains('/')) || rel.starts_with("speakers/"))
                    .then_some(key)
            })
            .collect::<Vec<_>>();
        let deleted = keys.len();
        if !keys.is_empty() {
            self.objects.delete_keys(&keys)?;
        }
        Ok(deleted)
    }

    /// Presigned GET URL for `key` using the configured TTL.
    pub fn presigned_get(&self, key: &str) -> String {
        self.objects.presigned_get_url(
            key,
            std::time::Duration::from_secs(self.presign_ttl_seconds),
        )
    }

    /// Object prefix holding one tombstone per deleted recording prefix.
    /// Tombstones live outside every deleted prefix so `delete_prefix` never
    /// removes its own marker.
    fn tombstone_prefix(&self) -> String {
        format!("{}.tombstones/", self.key_prefix)
    }

    /// Re-enqueues staged files whose remote object is missing, on a
    /// background thread. Pending uploads exist only in memory, so a restart
    /// during an S3 outage would otherwise leave finished meetings' remote
    /// copies missing until retention deleted the staging files. Call once
    /// at process start; the scan costs one remote list plus a local walk.
    pub fn reconcile_staged_uploads(&self) {
        let this = self.clone();
        if let Err(err) = std::thread::Builder::new()
            .name("s3-reconcile".to_owned())
            .spawn(move || this.reconcile_staged_uploads_now())
        {
            warn!(error = %err, "failed to spawn s3 reconcile thread");
        }
    }

    /// Synchronous body of `reconcile_staged_uploads`, split out for tests.
    fn reconcile_staged_uploads_now(&self) {
        let remote: HashSet<String> = match self.objects.list_keys(&self.key_prefix) {
            Ok(keys) => keys.into_iter().collect(),
            Err(err) => {
                warn!(
                    error = %err,
                    "s3 reconcile: cannot list remote objects; skipping"
                );
                return;
            }
        };
        let tombstone_prefix = self.tombstone_prefix();
        let tombstoned: Vec<String> = self
            .objects
            .list_keys(&tombstone_prefix)
            .unwrap_or_default()
            .into_iter()
            .filter_map(|key| {
                key.strip_prefix(tombstone_prefix.as_str())
                    .map(str::to_owned)
            })
            .collect();
        let mut files = Vec::new();
        collect_staged_files(&self.storage_dir, &mut files);
        let mut scanned = 0usize;
        let mut queued = 0usize;
        for path in files {
            let Ok(rel) = path.strip_prefix(&self.storage_dir) else {
                continue;
            };
            if !is_upload_candidate(rel) {
                continue;
            }
            scanned += 1;
            let Some(key) = self.object_key(&path) else {
                continue;
            };
            if remote.contains(&key)
                || tombstoned
                    .iter()
                    .any(|prefix| key.starts_with(prefix.as_str()))
            {
                continue;
            }
            match self.upload_file(&path, upload_content_type(rel)) {
                Ok(true) => queued += 1,
                Ok(false) => {}
                Err(err) => warn!(
                    path = %path.display(),
                    error = %err,
                    "s3 reconcile: failed to enqueue staged file"
                ),
            }
        }
        info!(
            scanned,
            queued, "s3 reconcile: re-enqueued staged files missing remotely"
        );
    }
}

/// Recursive file walk under `dir` (best effort; unreadable dirs are
/// skipped — the next startup scan retries them).
fn collect_staged_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_staged_files(&path, out);
        } else if path.is_file() {
            out.push(path);
        }
    }
}

/// Whether a staged file (relative to `storage_dir`) is part of the uploaded
/// surface: workspace `audio/` trees except the transcription-only
/// `transcription_speakers/`, or the legacy flat layout's `<meeting>/*.wav`
/// and `<meeting>/speakers/**` (mirroring `delete_legacy_recording_prefix`).
fn is_upload_candidate(rel: &Path) -> bool {
    let components: Vec<String> = rel
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect();
    // Hidden entries are build scaffolding (e.g. `.speaker-build-tmp/`), and
    // `.tmp`/`.part` names are staged writes that were never promoted — a
    // crash leftovers must not become permanent objects.
    if components.iter().any(|c| c.starts_with('.'))
        || components
            .last()
            .is_some_and(|name| name.ends_with(".tmp") || name.ends_with(".part"))
    {
        return false;
    }
    if components.first().map(String::as_str)
        == Some(crate::infrastructure::workspace::WORKSPACES_ROOT_DIR)
    {
        let Some(audio_pos) = components.iter().position(|c| c == "audio") else {
            return false;
        };
        // Intermediate transcription inputs are never uploaded.
        return components.get(audio_pos + 1).map(String::as_str) != Some("transcription_speakers");
    }
    match components.as_slice() {
        [_, file] => file.ends_with(".wav"),
        [_, dir, ..] => dir == "speakers",
        _ => false,
    }
}

fn upload_content_type(rel: &Path) -> &'static str {
    match rel.extension().and_then(|ext| ext.to_str()) {
        Some("wav") => "audio/wav",
        Some("json") => "application/json",
        _ => "application/octet-stream",
    }
}

/// `ChunkStorage` for `CHUNK_STORAGE_BACKEND=s3`: every chunk is still
/// written to the local workspace as the recording-time staging copy (live
/// transcription and assembly read local paths) and is queued for upload to
/// the object store, which is the canonical durable copy. Upload runs on the
/// shared background queue so the voice-ingest path never blocks on object
/// I/O; only a saturated queue surfaces `Remote` (the session's
/// pending-chunk retry then keeps re-attempting the save).
#[derive(Debug)]
pub struct S3ChunkStorage {
    local: LocalChunkStorage,
    objects: RecordingObjectStore,
    /// Staged files whose upload never reached the queue (`start_ms` →
    /// (path, byte length)). A retried save may stage under a different
    /// filename (sequence re-assigned, or the user re-keyed), so superseded
    /// duplicates are removed before they would double-count in assembly.
    failed_staged: Mutex<HashMap<u64, Vec<(PathBuf, usize)>>>,
}

impl S3ChunkStorage {
    /// Removes earlier staged files for `start_ms` whose bytes equal the
    /// chunk being saved — they are the same audio restaged by a retry.
    fn drop_superseded_staging(&self, start_ms: u64, bytes: &[u8]) {
        let mut pending = self.failed_staged.lock().unwrap();
        let Some(candidates) = pending.get_mut(&start_ms) else {
            return;
        };
        candidates.retain(|(path, len)| {
            if *len != bytes.len() {
                return true;
            }
            let same = std::fs::read(path).is_ok_and(|content| content == bytes);
            if same && std::fs::remove_file(path).is_err() {
                warn!(
                    path = %path.display(),
                    "failed to remove superseded staging file; duplicate may persist"
                );
            }
            !same
        });
        if candidates.is_empty() {
            pending.remove(&start_ms);
        }
    }
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
        self.drop_superseded_staging(start_ms, bytes);
        let saved = self
            .local
            .save_chunk(meeting_id, user_id, sequence, start_ms, bytes)?;
        if let Err(err) = self.objects.put_bytes(&saved.path, bytes, "audio/wav") {
            self.failed_staged
                .lock()
                .unwrap()
                .entry(start_ms)
                .or_default()
                .push((saved.path.clone(), bytes.len()));
            return Err(ChunkStorageError::Remote(err));
        }
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
            Some(objects) => Self::S3(S3ChunkStorage {
                local,
                objects,
                failed_staged: Mutex::new(HashMap::new()),
            }),
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
        /// Every put_object call, success or failure.
        attempts: Mutex<usize>,
        fail_puts: Mutex<bool>,
        /// Kills the upload worker (panic) to exercise queue-disconnect paths.
        panic_puts: Mutex<bool>,
        lists: Mutex<Vec<String>>,
        deletes: Mutex<Vec<Vec<String>>>,
        list_result: Mutex<Vec<String>>,
    }

    impl ObjectStore for FakeObjectStore {
        fn put_object(&self, key: &str, body: &[u8], content_type: &str) -> Result<(), S3Error> {
            *self.attempts.lock().unwrap() += 1;
            if *self.panic_puts.lock().unwrap() {
                panic!("injected put panic");
            }
            // Tombstone writes stay immune to `fail_puts` so a simulated
            // outage fails recording uploads without breaking deletes.
            if *self.fail_puts.lock().unwrap() && !key.starts_with(".tombstones/") {
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
            Ok(self
                .list_result
                .lock()
                .unwrap()
                .iter()
                .filter(|key| key.starts_with(prefix))
                .cloned()
                .collect())
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
        let storage = MeetingChunkStorage::new(layout_meeting(&base), "m1", Some(objects.clone()));

        let saved = storage
            .save_chunk("m1", "u1", 1, 0, b"wav-data")
            .expect("save should succeed");
        objects.wait_uploads_idle();

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
    fn s3_chunk_storage_retries_failed_upload_and_keeps_staging_file() {
        let base = temp_dir("put_fail");
        let _ = std::fs::remove_dir_all(&base);
        let (fake, dyn_store) = fake_store();
        *fake.fail_puts.lock().unwrap() = true;
        let objects = RecordingObjectStore::with_retry_base(
            dyn_store,
            String::new(),
            900,
            &base,
            Duration::from_millis(1),
        );
        let storage = MeetingChunkStorage::new(layout_meeting(&base), "m1", Some(objects.clone()));

        let saved = storage
            .save_chunk("m1", "u1", 1, 0, b"wav-data")
            .expect("enqueue should succeed even when the store fails");
        // Uploads retry indefinitely; let a few attempts fail, then recover.
        for _ in 0..200 {
            if *fake.attempts.lock().unwrap() >= 3 {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        *fake.fail_puts.lock().unwrap() = false;
        objects.wait_uploads_idle();

        let puts = fake.puts.lock().unwrap();
        assert_eq!(puts.len(), 1);
        assert_eq!(puts[0].1, b"wav-data");
        assert!(saved.path.exists());
        let _ = std::fs::remove_dir_all(&base);
    }

    /// `delete_prefix` must stop queued retries from recreating the deleted
    /// recording, and reject uploads enqueued afterwards for that prefix.
    #[test]
    fn delete_prefix_cancels_queued_uploads_and_rejects_new_ones() {
        let base = temp_dir("cancel");
        let _ = std::fs::remove_dir_all(&base);
        let (fake, dyn_store) = fake_store();
        *fake.fail_puts.lock().unwrap() = true;
        let objects = RecordingObjectStore::with_retry_base(
            dyn_store,
            String::new(),
            900,
            &base,
            Duration::from_millis(1),
        );
        let storage = MeetingChunkStorage::new(layout_meeting(&base), "m1", Some(objects.clone()));

        storage
            .save_chunk("m1", "u1", 1, 0, b"wav-data")
            .expect("save should enqueue");
        // Let the task attempt once so it is parked in the delayed queue.
        for _ in 0..200 {
            if *fake.attempts.lock().unwrap() >= 1 {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        objects
            .delete_prefix("workspaces/")
            .expect("prefix delete should succeed");
        objects.wait_uploads_idle();

        assert!(
            !fake
                .puts
                .lock()
                .unwrap()
                .iter()
                .any(|(key, _, _)| key.starts_with("workspaces/")),
            "a cancelled task must never be uploaded"
        );
        let later = base.join("workspaces/g/vc/m/later.json");
        std::fs::create_dir_all(later.parent().unwrap()).unwrap();
        assert!(
            objects
                .put_bytes(&later, b"{}", "application/json")
                .is_err(),
            "enqueues under a deleted prefix are rejected"
        );
        let _ = std::fs::remove_dir_all(&base);
    }

    /// `upload_file` defers the read to the worker, so large artifacts queue
    /// without occupying memory and are uploaded with their current bytes.
    #[test]
    fn upload_file_reads_payload_at_upload_time() {
        let base = temp_dir("upload_file");
        let _ = std::fs::remove_dir_all(&base);
        let (fake, dyn_store) = fake_store();
        let objects = RecordingObjectStore::new(dyn_store, String::new(), 900, &base);
        let wav = base.join("workspaces/g/vc/m/audio/mixdown.wav");
        std::fs::create_dir_all(wav.parent().unwrap()).unwrap();
        std::fs::write(&wav, vec![7u8; 4096]).unwrap();

        assert!(objects.upload_file(&wav, "audio/wav").unwrap());
        objects.wait_uploads_idle();

        let puts = fake.puts.lock().unwrap();
        assert_eq!(puts.len(), 1);
        assert_eq!(puts[0].0, "workspaces/g/vc/m/audio/mixdown.wav");
        assert_eq!(puts[0].1, vec![7u8; 4096]);
        assert!(
            !objects
                .upload_file(&base.join("workspaces/g/vc/m/audio/none.wav"), "audio/wav")
                .unwrap()
        );
        let _ = std::fs::remove_dir_all(&base);
    }

    /// A second write to the same key (e.g. a fresher `ssrc_mapping.json`)
    /// must win over an older queued task whose first attempts failed.
    #[test]
    fn newer_write_supersedes_queued_retry_for_same_key() {
        let base = temp_dir("supersede");
        let _ = std::fs::remove_dir_all(&base);
        let (fake, dyn_store) = fake_store();
        *fake.fail_puts.lock().unwrap() = true;
        let objects = RecordingObjectStore::with_retry_base(
            dyn_store,
            String::new(),
            900,
            &base,
            Duration::from_millis(1),
        );
        let path = base.join("workspaces/g/vc/m/ssrc_mapping.json");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();

        objects
            .put_bytes(&path, b"{\"v\":1}", "application/json")
            .unwrap();
        objects
            .put_bytes(&path, b"{\"v\":2}", "application/json")
            .unwrap();
        // Let both tasks attempt at least once so the older one is parked in
        // the delayed queue before it can be superseded.
        for _ in 0..200 {
            if *fake.attempts.lock().unwrap() >= 2 {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        *fake.fail_puts.lock().unwrap() = false;
        objects.wait_uploads_idle();

        let puts = fake.puts.lock().unwrap();
        assert_eq!(puts.len(), 1);
        assert_eq!(puts[0].1, b"{\"v\":2}");
        let _ = std::fs::remove_dir_all(&base);
    }

    /// A dead upload worker surfaces `Remote`, and a retry of the same audio
    /// under a different filename removes the superseded staging file so
    /// assembly never sees it twice.
    #[test]
    fn s3_chunk_storage_remote_error_dedupes_restaged_chunk() {
        let base = temp_dir("put_dead");
        let _ = std::fs::remove_dir_all(&base);
        let (fake, dyn_store) = fake_store();
        let objects = RecordingObjectStore::with_retry_base(
            dyn_store,
            String::new(),
            900,
            &base,
            Duration::from_millis(1),
        );
        let storage = MeetingChunkStorage::new(layout_meeting(&base), "m1", Some(objects.clone()));

        // Kill the worker, then keep probing until the disconnect is visible.
        *fake.panic_puts.lock().unwrap() = true;
        let probe = base.join("workspaces/g/vc/m/probe.json");
        std::fs::create_dir_all(probe.parent().unwrap()).unwrap();
        let mut gone = false;
        for _ in 0..200 {
            if objects
                .put_bytes(&probe, b"{}", "application/json")
                .is_err()
            {
                gone = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(gone, "upload worker never reported disconnection");

        let err = storage
            .save_chunk("m1", "u1", 1, 0, b"wav-data")
            .expect_err("save must surface Remote once the worker is gone");
        match err {
            ChunkStorageError::Remote(detail) => {
                assert!(detail.contains("worker is gone"));
            }
            other => panic!("expected Remote error, got {other:?}"),
        }
        let first_path = layout_meeting(&base).audio_dir().join("u1_1_0.wav");
        assert!(first_path.exists());

        let err = storage
            .save_chunk("m1", "u2", 7, 0, b"wav-data")
            .expect_err("retry still fails while the worker is dead");
        assert!(matches!(err, ChunkStorageError::Remote(_)));
        assert!(!first_path.exists(), "superseded staging file was removed");
        let staged = layout_meeting(&base).audio_dir().join("u2_7_0.wav");
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
        *fake.list_result.lock().unwrap() = vec![
            "workspaces/g/vc/m/a".to_owned(),
            "workspaces/g/vc/m/b".to_owned(),
        ];
        let store = RecordingObjectStore::new(dyn_store, String::new(), 900, "/data");
        assert_eq!(store.delete_prefix("workspaces/g/vc/m/").unwrap(), 2);
        assert!(
            fake.lists
                .lock()
                .unwrap()
                .contains(&"workspaces/g/vc/m/".to_owned())
        );
        assert_eq!(
            fake.deletes.lock().unwrap()[0],
            vec![
                "workspaces/g/vc/m/a".to_owned(),
                "workspaces/g/vc/m/b".to_owned()
            ]
        );
        // The delete also leaves a tombstone object so upload queues in
        // other processes learn the cancellation on their next refresh.
        assert!(fake.puts.lock().unwrap().iter().any(|(key, body, _)| {
            key == ".tombstones/workspaces/g/vc/m/" && body == b"workspaces/g/vc/m/"
        }));
    }

    /// A remote tombstone written by another process's `delete_prefix` must
    /// cancel our queued upload for the prefix and clean up the object an
    /// earlier racing attempt may have created.
    #[test]
    fn remote_tombstone_cancels_pending_upload() {
        let base = temp_dir("tombstone");
        let _ = std::fs::remove_dir_all(&base);
        let (fake, dyn_store) = fake_store();
        // Another process deleted `workspaces/` — its tombstone is visible.
        *fake.list_result.lock().unwrap() = vec![".tombstones/workspaces/".to_owned()];
        let objects = RecordingObjectStore::with_retry_base(
            dyn_store,
            String::new(),
            900,
            &base,
            Duration::from_millis(1),
        );
        let path = base.join("workspaces/g/vc/m/audio/u_1_0.wav");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, b"wav-data").unwrap();

        objects.upload_file(&path, "audio/wav").unwrap();
        objects.wait_uploads_idle();

        assert!(
            !fake
                .puts
                .lock()
                .unwrap()
                .iter()
                .any(|(key, _, _)| key.starts_with("workspaces/")),
            "upload under a remotely-deleted prefix must be skipped"
        );
        assert!(
            fake.deletes
                .lock()
                .unwrap()
                .iter()
                .any(|keys| keys == &vec!["workspaces/g/vc/m/audio/u_1_0.wav".to_owned()]),
            "the cancelled task self-cleans an object a racing PUT created"
        );
        let _ = std::fs::remove_dir_all(&base);
    }

    /// The newest write settling must not free the generation marker while
    /// an older retry for the same key is still queued — otherwise the stale
    /// retry overwrites the newer snapshot.
    #[test]
    fn settled_newer_write_still_supersedes_delayed_retry() {
        let base = temp_dir("settled_supersede");
        let _ = std::fs::remove_dir_all(&base);
        let (fake, dyn_store) = fake_store();
        *fake.fail_puts.lock().unwrap() = true;
        let objects = RecordingObjectStore::with_retry_base(
            dyn_store,
            String::new(),
            900,
            &base,
            Duration::from_millis(1),
        );
        let path = base.join("workspaces/g/vc/m/ssrc_mapping.json");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();

        // Old snapshot fails once and parks in the delayed queue.
        objects
            .put_bytes(&path, b"{\"v\":1}", "application/json")
            .unwrap();
        for _ in 0..200 {
            if *fake.attempts.lock().unwrap() >= 1 {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        // New snapshot lands and succeeds while the old one still waits.
        *fake.fail_puts.lock().unwrap() = false;
        objects
            .put_bytes(&path, b"{\"v\":2}", "application/json")
            .unwrap();
        objects.wait_uploads_idle();

        let puts: Vec<Vec<u8>> = fake
            .puts
            .lock()
            .unwrap()
            .iter()
            .map(|(_, body, _)| body.clone())
            .collect();
        assert_eq!(puts, vec![b"{\"v\":2}".to_vec()]);
        let _ = std::fs::remove_dir_all(&base);
    }

    /// File-backed tasks hold no inline bytes, so only the task-count cap
    /// stops the delayed-retry backlog from growing without bound.
    #[test]
    fn upload_queue_is_bounded_by_task_count() {
        let base = temp_dir("task_cap");
        let _ = std::fs::remove_dir_all(&base);
        let (fake, dyn_store) = fake_store();
        *fake.fail_puts.lock().unwrap() = true;
        let objects = RecordingObjectStore::with_retry_base(
            dyn_store,
            String::new(),
            900,
            &base,
            Duration::from_millis(1),
        );
        // Unique keys: same-key enqueues would supersede each other and
        // never accumulate in the backlog. Transient channel-full rejections
        // are retried; the loop only stops at the hard task cap.
        let mut rejected = None;
        let mut accepted = 0usize;
        for _ in 0..(UPLOAD_MAX_PENDING_TASKS * 4) {
            let path = base.join(format!("workspaces/g/vc/m/audio/chunk_{accepted}.wav"));
            match objects.put_bytes(&path, b"x", "audio/wav") {
                Ok(()) => accepted += 1,
                Err(err) if err.contains("queue is full") => {
                    std::thread::sleep(Duration::from_millis(2));
                }
                Err(err) => {
                    rejected = Some(err);
                    break;
                }
            }
        }
        assert_eq!(
            rejected.as_deref(),
            Some("recording upload queue task limit exceeded")
        );
        let _ = std::fs::remove_dir_all(&base);
    }

    /// Startup reconcile re-enqueues staged files whose remote object is
    /// missing and skips ones already uploaded or tombstoned.
    #[test]
    fn reconcile_staged_uploads_enqueues_only_missing() {
        let base = temp_dir("reconcile");
        let _ = std::fs::remove_dir_all(&base);
        let (fake, dyn_store) = fake_store();
        let objects = RecordingObjectStore::new(dyn_store, String::new(), 900, &base);
        let audio = base.join("workspaces/g/vc/m/audio");
        std::fs::create_dir_all(audio.join("speakers")).unwrap();
        std::fs::create_dir_all(audio.join("transcription_speakers")).unwrap();
        std::fs::write(audio.join("u_1_0.wav"), b"chunk").unwrap();
        std::fs::write(audio.join("mixdown.wav"), b"mix").unwrap();
        std::fs::write(audio.join("speakers/u.wav"), b"speaker").unwrap();
        std::fs::write(audio.join("ssrc_mapping.json"), b"{}").unwrap();
        // Transcription intermediates and non-audio files are not uploaded.
        std::fs::write(audio.join("transcription_speakers/u.wav"), b"part").unwrap();
        std::fs::create_dir_all(base.join("workspaces/g/vc/m/transcript")).unwrap();
        std::fs::write(base.join("workspaces/g/vc/m/transcript/t.md"), b"doc").unwrap();
        // Crash leftovers (staging tmp, unfinished mixdown, speaker build
        // scaffolding) are never uploaded either.
        std::fs::write(audio.join("u_1_0.wav.tmp"), b"staged").unwrap();
        std::fs::write(audio.join("mixdown.wav.part"), b"partial").unwrap();
        std::fs::create_dir_all(audio.join("speakers/.speaker-build-tmp")).unwrap();
        std::fs::write(audio.join("speakers/.speaker-build-tmp/u.wav"), b"tmp").unwrap();
        // Remote already has the mixdown and the mapping.
        *fake.list_result.lock().unwrap() = vec![
            "workspaces/g/vc/m/audio/mixdown.wav".to_owned(),
            "workspaces/g/vc/m/audio/ssrc_mapping.json".to_owned(),
            ".tombstones/legacy/".to_owned(),
        ];
        // A legacy-layout meeting dir whose prefix was deleted remotely.
        let legacy = base.join("legacy");
        std::fs::create_dir_all(legacy.join("speakers")).unwrap();
        std::fs::write(legacy.join("mixdown.wav"), b"old").unwrap();
        std::fs::write(legacy.join("speakers/u.wav"), b"old").unwrap();

        objects.reconcile_staged_uploads_now();
        objects.wait_uploads_idle();

        let mut puts: Vec<String> = fake
            .puts
            .lock()
            .unwrap()
            .iter()
            .map(|(key, _, _)| key.clone())
            .collect();
        puts.sort();
        assert_eq!(
            puts,
            vec![
                "workspaces/g/vc/m/audio/speakers/u.wav".to_owned(),
                "workspaces/g/vc/m/audio/u_1_0.wav".to_owned(),
            ],
            "only missing, non-tombstoned candidates are re-enqueued"
        );
        let _ = std::fs::remove_dir_all(&base);
    }

    /// A rejected enqueue rolls its generation back so it cannot supersede
    /// an accepted older write for the same key.
    #[test]
    fn rejected_enqueue_does_not_supersede_accepted_write() {
        let mut state = GenerationState::default();
        state.track("k", 1);
        state.track("k", 2);
        state.untrack("k", 2);
        assert!(!state.is_superseded("k", 1));
        state.settle("k", 1);
        assert!(!state.is_superseded("k", 3));
    }

    #[test]
    fn presigned_get_uses_configured_ttl() {
        let (_, dyn_store) = fake_store();
        let store = RecordingObjectStore::new(dyn_store, String::new(), 42, "/data");
        assert_eq!(store.presigned_get("k"), "https://example.test/k?ttl=42");
    }
}
