use crate::infrastructure::s3::{ObjectStore, S3Error, S3Settings};
use crate::infrastructure::storage_fs::{
    ChunkStorage, ChunkStorageError, LocalChunkStorage, SavedChunk,
};
use crate::infrastructure::workspace::MeetingWorkspacePaths;
use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, sync_channel};
use std::sync::{Arc, Condvar, Mutex};
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
/// Keep upload-worker capacity available for latency-sensitive recording
/// writes while startup reconciliation drains older staging files.
const RECONCILE_RESERVED_TASKS: usize = 16;
const RECONCILE_PENDING_LIMIT: usize = UPLOAD_QUEUE_BOUND - RECONCILE_RESERVED_TASKS;
/// How long `cancel_prefix` waits for an in-flight PUT under the prefix
/// before giving up (the S3 request timeout bounds a single PUT at 60s).
/// On expiry the delete is aborted with an error instead of racing a
/// still-running upload.
const CANCEL_INFLIGHT_WAIT: Duration = Duration::from_secs(65);
/// How often the upload worker re-fetches remote tombstone objects so a
/// delete issued by another process stops pending uploads here as well.
const TOMBSTONE_REFRESH_INTERVAL: Duration = Duration::from_secs(10);
/// Pause between staging-file enqueue attempts so the reconciliation producer
/// does not fill the upload queue faster than a slow object store can drain it.
const RECONCILE_ENQUEUE_INTERVAL: Duration = Duration::from_millis(1);
/// Retry transient list failures and incomplete staging-tree scans while the
/// process stays up; queue pressure uses a shorter capped retry delay.
const RECONCILE_RETRY_BASE: Duration = Duration::from_millis(250);
const RECONCILE_RETRY_MAX: Duration = Duration::from_secs(30);
const RECONCILE_BACKPRESSURE_RETRY_MAX: Duration = Duration::from_secs(2);

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
    receipt: Option<Arc<UploadReceiptCell>>,
}

/// In-memory completion state for one object key and upload generation.
/// Nothing in the receipt is serialized or persisted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UploadReceiptStatus {
    Queued,
    Retrying { attempts: u32, error: String },
    Durable,
    Failed(String),
    Superseded,
    Cancelled,
    Shutdown,
}

impl UploadReceiptStatus {
    fn is_terminal(&self) -> bool {
        matches!(
            self,
            Self::Durable | Self::Failed(_) | Self::Superseded | Self::Cancelled | Self::Shutdown
        )
    }
}

#[derive(Debug)]
struct UploadReceiptCell {
    status: Mutex<UploadReceiptStatus>,
    changed: Condvar,
}

impl UploadReceiptCell {
    fn update(&self, status: UploadReceiptStatus) {
        *self.status.lock().unwrap() = status;
        self.changed.notify_all();
    }
}

/// Tracks whether a queued object PUT is durably available in the store.
/// Receipts are scoped to one key and generation and are only held in memory.
#[derive(Debug)]
pub struct UploadReceipt {
    key: String,
    generation: u64,
    cell: Arc<UploadReceiptCell>,
}

impl UploadReceipt {
    pub fn key(&self) -> &str {
        &self.key
    }

    pub fn generation(&self) -> u64 {
        self.generation
    }

    pub fn status(&self) -> UploadReceiptStatus {
        self.cell.status.lock().unwrap().clone()
    }

    /// Waits until the receipt changes from `observed`, returning its latest
    /// state. Retry status is observable without consuming the receipt.
    pub fn wait_for_change(&self, observed: &UploadReceiptStatus) -> UploadReceiptStatus {
        let mut status = self.cell.status.lock().unwrap();
        while *status == *observed && !status.is_terminal() {
            status = self.cell.changed.wait(status).unwrap();
        }
        status.clone()
    }

    /// Waits for durable success or a terminal failure, including shutdown.
    pub fn wait(&self) -> UploadReceiptStatus {
        let mut status = self.cell.status.lock().unwrap();
        while !status.is_terminal() {
            status = self.cell.changed.wait(status).unwrap();
        }
        status.clone()
    }
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

impl Drop for UploadTask {
    fn drop(&mut self) {
        if let Some(receipt) = &self.receipt {
            let status = receipt.status.lock().unwrap();
            if !status.is_terminal() {
                drop(status);
                receipt.update(UploadReceiptStatus::Shutdown);
            }
        }
    }
}

fn is_retryable_upload_error(error: &S3Error) -> bool {
    match error {
        S3Error::Config(_) => false,
        S3Error::Http(_) => true,
        S3Error::Status { status, detail } => {
            if *status == 408 || *status == 429 || *status >= 500 {
                return true;
            }
            let detail = detail.to_ascii_lowercase();
            [
                "requesttimeout",
                "requesttimeoutexception",
                "slowdown",
                "operationaborted",
                "throttling",
                "temporarilyunavailable",
                "serviceunavailable",
            ]
            .iter()
            .any(|code| detail.contains(code))
        }
    }
}

/// Generation tracking for one key. `outstanding` holds every generation
/// that is queued, delayed, or in-flight; `delivered` records the newest
/// generation confirmed uploaded. Both are garbage-collected once
/// `outstanding` empties — while anything older remains, `delivered` keeps
/// a delayed retry from overwriting a newer snapshot (e.g. `ssrc_mapping.json`).
#[derive(Debug, Default)]
struct GenerationState {
    outstanding: HashMap<String, BTreeSet<u64>>,
    delivered: HashMap<String, u64>,
}

impl GenerationState {
    /// Records a fresh enqueue for the key.
    fn track(&mut self, key: &str, generation: u64) {
        self.outstanding
            .entry(key.to_owned())
            .or_default()
            .insert(generation);
    }

    /// A task is superseded when a newer write for its key is queued or was
    /// already delivered — a rejected enqueue never counts because it is
    /// rolled back via `untrack` before this is consulted.
    fn is_superseded(&self, key: &str, generation: u64) -> bool {
        let newest = [
            self.outstanding
                .get(key)
                .and_then(|set| set.iter().next_back())
                .copied(),
            self.delivered.get(key).copied(),
        ]
        .into_iter()
        .flatten()
        .max();
        newest.is_some_and(|newest| newest > generation)
    }

    /// Drops one generation; forgets the key entirely (including the
    /// delivered marker) once no queued or in-flight write for it remains.
    fn untrack(&mut self, key: &str, generation: u64) {
        if let Some(set) = self.outstanding.get_mut(key) {
            set.remove(&generation);
            if set.is_empty() {
                self.outstanding.remove(key);
                self.delivered.remove(key);
            }
        }
    }

    /// Releases a task; when `delivered`, the generation also counts as a
    /// completed write so older outstanding retries stay superseded.
    fn settle(&mut self, key: &str, generation: u64, delivered: bool) {
        if delivered {
            let slot = self.delivered.entry(key.to_owned()).or_default();
            *slot = (*slot).max(generation);
        }
        self.untrack(key, generation);
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
    /// Per-key write ordering state (outstanding set + delivered marker).
    generations: Arc<Mutex<GenerationState>>,
    next_generation: AtomicU64,
    /// Cancellation state shared with the worker (see `CancelState`).
    cancel: Arc<Mutex<CancelState>>,
    /// Object store the worker PUTs through; also used by `cancel_prefix`
    /// to write tombstones visible to other processes' queues.
    objects: Arc<dyn ObjectStore>,
    /// `{key_prefix}.tombstones/` — one object per deleted prefix.
    tombstone_prefix: String,
    reconcile_stop: Arc<AtomicBool>,
    reconciler: Mutex<Option<std::thread::JoinHandle<()>>>,
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
            reconcile_stop: Arc::new(AtomicBool::new(false)),
            reconciler: Mutex::new(None),
        }
    }

    fn start_reconciler(
        self: &Arc<Self>,
        objects: Arc<dyn ObjectStore>,
        key_prefix: String,
        storage_dir: PathBuf,
    ) -> Result<(), std::io::Error> {
        let mut reconciler = self.reconciler.lock().unwrap();
        if reconciler
            .as_ref()
            .is_some_and(|worker| !worker.is_finished())
        {
            return Ok(());
        }
        if let Some(worker) = reconciler.take() {
            let _ = worker.join();
        }
        let uploads = Arc::downgrade(self);
        let stop = Arc::clone(&self.reconcile_stop);
        *reconciler = Some(
            std::thread::Builder::new()
                .name("s3-reconcile".to_owned())
                .spawn(move || {
                    reconcile_staged_uploads_loop(uploads, objects, key_prefix, storage_dir, stop)
                })?,
        );
        Ok(())
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
        // `delivered` marks the write as landed so older queued generations
        // for the key stay superseded.
        let settle = |task: &UploadTask, status: UploadReceiptStatus, delivered: bool| {
            pending.fetch_sub(1, Ordering::SeqCst);
            pending_bytes.fetch_sub(task.counted_bytes(), Ordering::SeqCst);
            generations
                .lock()
                .unwrap()
                .settle(&task.key, task.generation, delivered);
            if let Some(receipt) = &task.receipt {
                receipt.update(status);
            }
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
                    settle(&task, UploadReceiptStatus::Superseded, false);
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
                    settle(
                        &task,
                        UploadReceiptStatus::Failed("upload source file is gone".to_owned()),
                        false,
                    );
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
                    settle(&task, UploadReceiptStatus::Cancelled, false);
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
                    Ok(()) => settle(&task, UploadReceiptStatus::Durable, true),
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
                            settle(&task, UploadReceiptStatus::Shutdown, false);
                        } else if !is_retryable_upload_error(&err) {
                            warn!(
                                key = %task.key,
                                attempts = task.attempts,
                                error = %err,
                                "recording upload failed permanently; only the local staging copy remains"
                            );
                            settle(&task, UploadReceiptStatus::Failed(err.to_string()), false);
                        } else {
                            let backoff = retry_base * (1 << task.attempts.min(5));
                            warn!(
                                key = %task.key,
                                attempts = task.attempts,
                                error = %err,
                                "recording upload failed; retrying"
                            );
                            task.not_before = Instant::now() + backoff;
                            if let Some(receipt) = &task.receipt {
                                receipt.update(UploadReceiptStatus::Retrying {
                                    attempts: task.attempts,
                                    error: err.to_string(),
                                });
                            }
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
    fn enqueue(&self, task: UploadTask) -> Result<UploadReceipt, String> {
        self.enqueue_with_pending_limit(task, UPLOAD_MAX_PENDING_TASKS)
    }

    fn enqueue_reconciliation(&self, task: UploadTask) -> Result<(), String> {
        self.enqueue_with_pending_limit(task, RECONCILE_PENDING_LIMIT)
            .map(|_| ())
    }

    fn enqueue_with_pending_limit(
        &self,
        mut task: UploadTask,
        pending_limit: usize,
    ) -> Result<UploadReceipt, String> {
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
        let reserved = self
            .pending
            .try_update(Ordering::SeqCst, Ordering::SeqCst, |used| {
                (used < pending_limit).then_some(used + 1)
            });
        if reserved.is_err() {
            self.pending_bytes.fetch_sub(size, Ordering::SeqCst);
            return Err("recording upload queue task limit exceeded".to_owned());
        }
        task.generation = self.next_generation.fetch_add(1, Ordering::SeqCst);
        let generation = task.generation;
        let key = task.key.clone();
        let cell = Arc::new(UploadReceiptCell {
            status: Mutex::new(UploadReceiptStatus::Queued),
            changed: Condvar::new(),
        });
        task.receipt = Some(Arc::clone(&cell));
        self.generations
            .lock()
            .unwrap()
            .track(&task.key, task.generation);
        let Some(sender) = &self.sender else {
            self.revert_enqueue(&task, size);
            return Err("recording upload worker is gone".to_owned());
        };
        match sender.try_send(task) {
            Ok(()) => Ok(UploadReceipt {
                key,
                generation,
                cell,
            }),
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
        self.reconcile_stop.store(true, Ordering::SeqCst);
        if let Some(reconciler) = self.reconciler.lock().unwrap().take() {
            let reconciler_thread = reconciler.thread().id();
            if reconciler_thread == std::thread::current().id() {
                // A final Weak upgrade can become the last Arc when the store
                // is dropped during a scan. The stop flag makes that worker
                // exit after this Drop returns; joining itself would deadlock.
                drop(reconciler);
            } else {
                reconciler.thread().unpark();
                let _ = reconciler.join();
            }
        }
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
    /// Shared only by public store handles (the reconciler holds a Weak queue
    /// reference), so dropping the final handle can stop an active scan.
    reconcile_owner: Arc<()>,
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
            reconcile_owner: Arc::new(()),
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
        self.put_bytes_with_receipt(path, bytes, content_type)
            .map(|_| ())
    }

    /// Enqueues an inline PUT and returns a receipt for the exact key and
    /// generation, which becomes durable only after the object-store PUT.
    pub fn put_bytes_with_receipt(
        &self,
        path: &Path,
        bytes: &[u8],
        content_type: &str,
    ) -> Result<UploadReceipt, String> {
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
            receipt: None,
        })
    }

    /// Queues `path` for upload under its mirrored key; the worker reads the
    /// file at PUT time so arbitrarily large playback files never occupy
    /// queue memory or trip the inline byte cap. Missing files are reported
    /// as `Ok(false)` so callers can treat them as already deleted.
    pub fn upload_file(&self, path: &Path, content_type: &str) -> Result<bool, String> {
        self.upload_file_with_receipt(path, content_type)
            .map(|receipt| receipt.is_some())
    }

    /// Queues a file upload and returns an in-memory receipt for its exact
    /// object key and generation. `None` means the path is outside the
    /// mirrored tree or no longer exists.
    pub fn upload_file_with_receipt(
        &self,
        path: &Path,
        content_type: &str,
    ) -> Result<Option<UploadReceipt>, String> {
        let Some(key) = self.object_key(path) else {
            return Ok(None);
        };
        if !path.is_file() {
            return Ok(None);
        }
        let receipt = self.uploads.enqueue(UploadTask {
            key,
            source: UploadSource::File(path.to_path_buf()),
            content_type: content_type.to_owned(),
            path: path.to_path_buf(),
            generation: 0, // assigned by `enqueue`
            attempts: 0,
            not_before: Instant::now(),
            receipt: None,
        })?;
        Ok(Some(receipt))
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
    #[cfg(test)]
    fn tombstone_prefix(&self) -> String {
        format!("{}.tombstones/", self.key_prefix)
    }

    /// Reconciles staged files whose remote object is missing on a background
    /// thread. It retries failed listings and queue saturation while this
    /// store is alive, so startup work is not lost until the next restart.
    pub fn reconcile_staged_uploads(&self) {
        if let Err(err) = self.uploads.start_reconciler(
            Arc::clone(&self.objects),
            self.key_prefix.clone(),
            self.storage_dir.clone(),
        ) {
            warn!(error = %err, "failed to spawn s3 reconcile thread");
        }
    }

    /// Synchronous body of `reconcile_staged_uploads`, split out for tests.
    #[cfg(test)]
    fn reconcile_staged_uploads_now(&self) {
        let mut accepted = HashMap::new();
        let scan = reconcile_staged_uploads_once(
            &self.storage_dir,
            &self.objects,
            &self.key_prefix,
            &self.tombstone_prefix(),
            &self.uploads,
            &mut accepted,
            &AtomicBool::new(false),
        );
        let (scanned, queued) = scan
            .map(|result| (result.scanned, result.queued))
            .unwrap_or_else(|err| {
                warn!(error = %err, "s3 reconcile: staging scan failed");
                (0, 0)
            });
        info!(
            scanned,
            queued, "s3 reconcile: re-enqueued staged files missing remotely"
        );
    }
}

impl Drop for RecordingObjectStore {
    fn drop(&mut self) {
        if Arc::strong_count(&self.reconcile_owner) == 1 {
            self.uploads.reconcile_stop.store(true, Ordering::SeqCst);
            if let Some(reconciler) = self.uploads.reconciler.lock().unwrap().as_ref() {
                reconciler.thread().unpark();
            }
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct StagedFileStamp {
    len: u64,
    modified: Option<std::time::SystemTime>,
}

#[derive(Debug)]
struct ReconcileScan {
    scanned: usize,
    queued: usize,
    complete: bool,
    backpressured: bool,
}

#[derive(Debug, Default)]
struct TombstoneIndex {
    prefixes: HashSet<String>,
}

impl TombstoneIndex {
    fn insert(&mut self, prefix: String) {
        self.prefixes.insert(prefix);
    }

    fn contains_prefix_of(&self, key: &str) -> bool {
        self.prefixes.contains(key)
            || key
                .char_indices()
                .any(|(index, _)| self.prefixes.contains(&key[..index]))
    }
}

fn incomplete_scan_retry_delay(scan: &ReconcileScan, retry_delay: &mut Duration) -> Duration {
    let delay = if scan.backpressured {
        (*retry_delay).min(RECONCILE_BACKPRESSURE_RETRY_MAX)
    } else {
        *retry_delay
    };
    *retry_delay = if scan.queued > 0 {
        RECONCILE_RETRY_BASE
    } else if scan.backpressured {
        delay
            .checked_mul(2)
            .unwrap_or(RECONCILE_BACKPRESSURE_RETRY_MAX)
            .min(RECONCILE_BACKPRESSURE_RETRY_MAX)
    } else {
        (*retry_delay)
            .checked_mul(2)
            .unwrap_or(RECONCILE_RETRY_MAX)
            .min(RECONCILE_RETRY_MAX)
    };
    delay
}

fn staged_file_stamp(path: &Path) -> Option<StagedFileStamp> {
    let metadata = std::fs::metadata(path).ok()?;
    Some(StagedFileStamp {
        len: metadata.len(),
        modified: metadata.modified().ok(),
    })
}

fn wait_reconciler(stop: &AtomicBool, delay: Duration) -> bool {
    if stop.load(Ordering::SeqCst) {
        return false;
    }
    std::thread::park_timeout(delay);
    !stop.load(Ordering::SeqCst)
}

fn reconcile_staged_uploads_loop(
    uploads: std::sync::Weak<UploadQueue>,
    objects: Arc<dyn ObjectStore>,
    key_prefix: String,
    storage_dir: PathBuf,
    stop: Arc<AtomicBool>,
) {
    let tombstone_prefix = format!("{key_prefix}.tombstones/");
    let mut accepted: HashMap<String, (PathBuf, StagedFileStamp)> = HashMap::new();
    let mut retry_delay = RECONCILE_RETRY_BASE;

    while !stop.load(Ordering::SeqCst) {
        let Some(uploads) = uploads.upgrade() else {
            return;
        };
        let scan = reconcile_staged_uploads_once(
            &storage_dir,
            &objects,
            &key_prefix,
            &tombstone_prefix,
            &uploads,
            &mut accepted,
            &stop,
        );
        drop(uploads);

        let delay = match scan {
            Ok(scan) if scan.complete => {
                info!(
                    scanned = scan.scanned,
                    queued = scan.queued,
                    "s3 reconcile: staged-file scan complete"
                );
                return;
            }
            Ok(scan) => {
                info!(
                    scanned = scan.scanned,
                    queued = scan.queued,
                    "s3 reconcile: incomplete scan will resume"
                );
                incomplete_scan_retry_delay(&scan, &mut retry_delay)
            }
            Err(err) => {
                warn!(error = %err, "s3 reconcile: scan failed; retrying");
                let delay = retry_delay;
                retry_delay = retry_delay
                    .checked_mul(2)
                    .unwrap_or(RECONCILE_RETRY_MAX)
                    .min(RECONCILE_RETRY_MAX);
                delay
            }
        };

        if !wait_reconciler(&stop, delay) {
            return;
        }
    }
}

fn reconcile_staged_uploads_once(
    storage_dir: &Path,
    objects: &Arc<dyn ObjectStore>,
    key_prefix: &str,
    tombstone_prefix: &str,
    uploads: &UploadQueue,
    accepted: &mut HashMap<String, (PathBuf, StagedFileStamp)>,
    stop: &AtomicBool,
) -> Result<ReconcileScan, String> {
    accepted.retain(|_, (path, stamp)| staged_file_stamp(path).as_ref() == Some(stamp));
    let mut scanned = 0usize;
    let mut queued = 0usize;
    let mut backpressured = false;

    let mut files = Vec::new();
    let mut complete = collect_staged_files(storage_dir, &mut files);
    files.sort();
    let mut staged = HashMap::new();
    for path in files {
        if stop.load(Ordering::SeqCst) {
            complete = false;
            break;
        }
        let Ok(rel) = path.strip_prefix(storage_dir) else {
            continue;
        };
        if !is_upload_candidate(rel) {
            continue;
        }
        scanned += 1;
        let Some(stamp) = staged_file_stamp(&path) else {
            complete = false;
            continue;
        };
        let rel_key = rel
            .components()
            .map(|component| component.as_os_str().to_string_lossy())
            .collect::<Vec<_>>()
            .join("/");
        if rel_key.is_empty() {
            continue;
        }
        let key = format!("{key_prefix}{rel_key}");
        if accepted
            .get(&key)
            .is_some_and(|(_, accepted_stamp)| accepted_stamp == &stamp)
        {
            continue;
        }
        staged.insert(key, (path, stamp));
    }

    let list_result = objects.visit_keys(key_prefix, &mut |key| {
        if stop.load(Ordering::SeqCst) {
            return Err(S3Error::Config("s3 reconciliation stopped".to_owned()));
        }
        staged.remove(key);
        Ok(())
    });
    if stop.load(Ordering::SeqCst) {
        return Ok(ReconcileScan {
            scanned,
            queued,
            complete: false,
            backpressured,
        });
    }
    list_result.map_err(|err| format!("cannot list remote objects: {err}"))?;

    // Check tombstones after the potentially long object listing so a deletion
    // created during that scan cannot be missed before an upload is enqueued.
    let mut tombstones = TombstoneIndex::default();
    let tombstone_result = objects.visit_keys(tombstone_prefix, &mut |key| {
        if stop.load(Ordering::SeqCst) {
            return Err(S3Error::Config("s3 reconciliation stopped".to_owned()));
        }
        if let Some(prefix) = key.strip_prefix(tombstone_prefix) {
            tombstones.insert(prefix.to_owned());
        }
        Ok(())
    });
    if stop.load(Ordering::SeqCst) {
        return Ok(ReconcileScan {
            scanned,
            queued,
            complete: false,
            backpressured,
        });
    }
    tombstone_result.map_err(|err| format!("cannot list remote tombstones: {err}"))?;
    accepted.retain(|key, _| !tombstones.contains_prefix_of(key));

    let mut staged: Vec<_> = staged.into_iter().collect();
    staged.sort_by(|left, right| left.0.cmp(&right.0));
    for (key, (path, stamp)) in staged {
        if stop.load(Ordering::SeqCst) {
            complete = false;
            break;
        }
        if tombstones.contains_prefix_of(&key) {
            accepted.remove(&key);
            continue;
        }
        let Ok(rel) = path.strip_prefix(storage_dir) else {
            continue;
        };
        let task = UploadTask {
            key: key.clone(),
            source: UploadSource::File(path.clone()),
            content_type: upload_content_type(rel).to_owned(),
            path: path.clone(),
            generation: 0,
            attempts: 0,
            not_before: Instant::now(),
            receipt: None,
        };
        match uploads.enqueue_reconciliation(task) {
            Ok(()) => {
                accepted.insert(key, (path, stamp));
                queued += 1;
                if !wait_reconciler(stop, RECONCILE_ENQUEUE_INTERVAL) {
                    complete = false;
                    break;
                }
            }
            Err(err)
                if err.contains("queue is full") || err.contains("queue task limit exceeded") =>
            {
                complete = false;
                backpressured = true;
                break;
            }
            Err(err) => {
                complete = false;
                warn!(path = %path.display(), error = %err, "s3 reconcile: failed to enqueue staged file");
            }
        }
    }

    Ok(ReconcileScan {
        scanned,
        queued,
        complete,
        backpressured,
    })
}

/// Recursive file walk under `dir`. Returns false if any directory entry
/// could not be read so the reconciler retries instead of losing that path.
fn collect_staged_files(dir: &Path, out: &mut Vec<PathBuf>) -> bool {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return false;
    };
    let mut complete = true;
    for entry in entries {
        let Ok(entry) = entry else {
            complete = false;
            continue;
        };
        let path = entry.path();
        match std::fs::metadata(&path) {
            Ok(metadata) if metadata.is_dir() => {
                complete &= collect_staged_files(&path, out);
            }
            Ok(metadata) if metadata.is_file() => out.push(path),
            Ok(_) => {}
            Err(_) => complete = false,
        }
    }
    complete
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
type FailedStagedUploads = Arc<Mutex<HashMap<u64, Vec<(PathBuf, usize)>>>>;

#[derive(Debug, Clone)]
pub struct S3ChunkStorage {
    local: LocalChunkStorage,
    objects: RecordingObjectStore,
    /// Staged files whose upload never reached the queue (`start_ms` →
    /// (path, byte length)). A retried save may stage under a different
    /// filename (sequence re-assigned, or the user re-keyed), so superseded
    /// duplicates are removed before they would double-count in assembly.
    failed_staged: FailedStagedUploads,
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
#[derive(Debug, Clone)]
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
                failed_staged: Arc::new(Mutex::new(HashMap::new())),
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
        timeout_put_failures: Mutex<usize>,
        permanent_put_failures: Mutex<bool>,
        /// Kills the upload worker (panic) to exercise queue-disconnect paths.
        panic_puts: Mutex<bool>,
        lists: Mutex<Vec<String>>,
        deletes: Mutex<Vec<Vec<String>>>,
        list_result: Mutex<Vec<String>>,
        list_failures: Mutex<HashMap<String, usize>>,
        list_delay: Mutex<Duration>,
        put_delay: Mutex<Duration>,
    }

    impl ObjectStore for FakeObjectStore {
        fn put_object(&self, key: &str, body: &[u8], content_type: &str) -> Result<(), S3Error> {
            let put_delay = *self.put_delay.lock().unwrap();
            if !put_delay.is_zero() && !key.starts_with(".tombstones/") {
                std::thread::sleep(put_delay);
            }
            *self.attempts.lock().unwrap() += 1;
            if *self.panic_puts.lock().unwrap() {
                panic!("injected put panic");
            }
            let mut timeouts = self.timeout_put_failures.lock().unwrap();
            if *timeouts > 0 && !key.starts_with(".tombstones/") {
                *timeouts -= 1;
                return Err(S3Error::Status {
                    status: 400,
                    detail: "<Error><Code>RequestTimeout</Code></Error>".to_owned(),
                });
            }
            drop(timeouts);
            if *self.permanent_put_failures.lock().unwrap() && !key.starts_with(".tombstones/") {
                return Err(S3Error::Status {
                    status: 403,
                    detail: "injected permanent failure".to_owned(),
                });
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
            let list_delay = *self.list_delay.lock().unwrap();
            if !list_delay.is_zero() {
                std::thread::sleep(list_delay);
            }
            let should_fail = {
                let mut failures = self.list_failures.lock().unwrap();
                if let Some(remaining) = failures.get_mut(prefix)
                    && *remaining > 0
                {
                    *remaining -= 1;
                    true
                } else {
                    false
                }
            };
            if should_fail {
                return Err(S3Error::Http("injected temporary list failure".to_owned()));
            }
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

    #[test]
    fn upload_receipt_reports_retry_then_durable_generation() {
        let base = temp_dir("receipt_retry");
        let _ = std::fs::remove_dir_all(&base);
        let (fake, dyn_store) = fake_store();
        *fake.fail_puts.lock().unwrap() = true;
        let objects = RecordingObjectStore::with_retry_base(
            dyn_store,
            String::new(),
            900,
            &base,
            Duration::from_millis(20),
        );
        let path = base.join("workspaces/g/vc/m/audio/chunk.wav");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, b"audio").unwrap();

        let receipt = objects
            .upload_file_with_receipt(&path, "audio/wav")
            .unwrap()
            .expect("staged file should enqueue");
        assert_eq!(receipt.key(), "workspaces/g/vc/m/audio/chunk.wav");
        assert_eq!(receipt.generation(), 0);
        let retry = receipt.wait_for_change(&UploadReceiptStatus::Queued);
        assert!(matches!(
            retry,
            UploadReceiptStatus::Retrying { attempts: 1, .. }
        ));
        *fake.fail_puts.lock().unwrap() = false;
        assert_eq!(receipt.wait(), UploadReceiptStatus::Durable);
        assert_eq!(
            receipt.wait_for_change(&UploadReceiptStatus::Durable),
            UploadReceiptStatus::Durable
        );

        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn inline_receipt_is_not_durable_before_the_put_finishes() {
        let base = temp_dir("inline_receipt");
        let _ = std::fs::remove_dir_all(&base);
        let (fake, dyn_store) = fake_store();
        *fake.put_delay.lock().unwrap() = Duration::from_millis(50);
        let objects = RecordingObjectStore::new(dyn_store, String::new(), 900, &base);
        let path = base.join("workspaces/g/vc/m/audio/chunk.wav");

        let receipt = objects
            .put_bytes_with_receipt(&path, b"audio", "audio/wav")
            .unwrap();
        assert_eq!(receipt.status(), UploadReceiptStatus::Queued);
        assert_eq!(receipt.wait(), UploadReceiptStatus::Durable);
        assert_eq!(fake.puts.lock().unwrap()[0].1, b"audio");

        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn s3_request_timeout_status_400_is_retried() {
        let base = temp_dir("receipt_request_timeout");
        let _ = std::fs::remove_dir_all(&base);
        let (fake, dyn_store) = fake_store();
        *fake.timeout_put_failures.lock().unwrap() = 1;
        let objects = RecordingObjectStore::with_retry_base(
            dyn_store,
            String::new(),
            900,
            &base,
            Duration::from_millis(1),
        );

        let receipt = objects
            .put_bytes_with_receipt(
                &base.join("workspaces/g/vc/m/audio/chunk.wav"),
                b"audio",
                "audio/wav",
            )
            .unwrap();
        assert!(matches!(
            receipt.wait_for_change(&UploadReceiptStatus::Queued),
            UploadReceiptStatus::Retrying { attempts: 1, .. }
        ));
        assert_eq!(receipt.wait(), UploadReceiptStatus::Durable);
        assert_eq!(*fake.attempts.lock().unwrap(), 2);
        assert_eq!(fake.puts.lock().unwrap().len(), 1);

        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn upload_receipt_reports_permanent_failure_and_shutdown() {
        let base = temp_dir("receipt_fail");
        let _ = std::fs::remove_dir_all(&base);
        let (fake, dyn_store) = fake_store();
        *fake.permanent_put_failures.lock().unwrap() = true;
        let objects = RecordingObjectStore::with_retry_base(
            dyn_store,
            String::new(),
            900,
            &base,
            Duration::from_millis(1),
        );
        let path = base.join("workspaces/g/vc/m/audio/forbidden.wav");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, b"audio").unwrap();
        let receipt = objects
            .upload_file_with_receipt(&path, "audio/wav")
            .unwrap()
            .unwrap();
        assert!(matches!(receipt.wait(), UploadReceiptStatus::Failed(_)));

        *fake.permanent_put_failures.lock().unwrap() = false;
        *fake.fail_puts.lock().unwrap() = true;
        let shutdown_path = base.join("workspaces/g/vc/m/audio/shutdown.wav");
        std::fs::write(&shutdown_path, b"audio").unwrap();
        let shutdown_receipt = objects
            .upload_file_with_receipt(&shutdown_path, "audio/wav")
            .unwrap()
            .unwrap();
        drop(objects);
        assert_eq!(shutdown_receipt.wait(), UploadReceiptStatus::Shutdown);

        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn upload_receipts_do_not_confuse_superseded_generations() {
        let base = temp_dir("receipt_generations");
        let _ = std::fs::remove_dir_all(&base);
        let (fake, dyn_store) = fake_store();
        *fake.fail_puts.lock().unwrap() = true;
        let objects = RecordingObjectStore::with_retry_base(
            dyn_store,
            String::new(),
            900,
            &base,
            Duration::from_secs(1),
        );
        let path = base.join("workspaces/g/vc/m/audio/snapshot.json");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, b"old").unwrap();
        let old = objects
            .upload_file_with_receipt(&path, "application/json")
            .unwrap()
            .unwrap();
        assert!(matches!(
            old.wait_for_change(&UploadReceiptStatus::Queued),
            UploadReceiptStatus::Retrying { .. }
        ));

        *fake.fail_puts.lock().unwrap() = false;
        std::fs::write(&path, b"new").unwrap();
        let new = objects
            .upload_file_with_receipt(&path, "application/json")
            .unwrap()
            .unwrap();
        assert_ne!(old.generation(), new.generation());
        assert_eq!(new.wait(), UploadReceiptStatus::Durable);
        assert_eq!(old.wait(), UploadReceiptStatus::Superseded);
        assert_eq!(
            fake.puts.lock().unwrap().as_slice(),
            &[(
                "workspaces/g/vc/m/audio/snapshot.json".to_owned(),
                b"new".to_vec(),
                "application/json".to_owned(),
            )]
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

    #[test]
    fn reconcile_retries_temporary_object_and_tombstone_list_failures() {
        let base = temp_dir("reconcile_list_retry");
        let _ = std::fs::remove_dir_all(&base);
        let (fake, dyn_store) = fake_store();
        let objects = RecordingObjectStore::new(dyn_store, String::new(), 900, &base);
        let staged = base.join("meeting/audio.wav");
        std::fs::create_dir_all(staged.parent().unwrap()).unwrap();
        std::fs::write(&staged, b"audio").unwrap();
        fake.list_failures
            .lock()
            .unwrap()
            .extend([(String::new(), 2), (".tombstones/".to_owned(), 1)]);

        objects.reconcile_staged_uploads();
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline && fake.puts.lock().unwrap().is_empty() {
            std::thread::sleep(Duration::from_millis(10));
        }
        objects.wait_uploads_idle();

        assert_eq!(
            fake.puts
                .lock()
                .unwrap()
                .iter()
                .map(|(key, _, _)| key.clone())
                .collect::<Vec<_>>(),
            vec!["meeting/audio.wav"]
        );
        let lists = fake.lists.lock().unwrap();
        assert!(lists.iter().filter(|prefix| prefix.is_empty()).count() >= 3);
        assert!(
            lists
                .iter()
                .filter(|prefix| prefix.as_str() == ".tombstones/")
                .count()
                >= 2
        );
        drop(lists);
        drop(objects);
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn reconcile_resumes_after_slow_uploads_fill_the_bounded_queue() {
        const STAGED_FILES: usize = 80;
        let base = temp_dir("reconcile_backpressure");
        let _ = std::fs::remove_dir_all(&base);
        let (fake, dyn_store) = fake_store();
        *fake.put_delay.lock().unwrap() = Duration::from_millis(15);
        let objects = RecordingObjectStore::new(dyn_store, String::new(), 900, &base);
        let meeting_dir = base.join("meeting");
        std::fs::create_dir_all(&meeting_dir).unwrap();
        for index in 0..STAGED_FILES {
            std::fs::write(meeting_dir.join(format!("audio-{index:03}.wav")), b"audio").unwrap();
        }

        objects.reconcile_staged_uploads();
        let reserve_deadline = Instant::now() + Duration::from_secs(5);
        let mut reached_reconcile_limit = false;
        while Instant::now() < reserve_deadline {
            if objects.uploads.pending.load(Ordering::SeqCst) >= RECONCILE_PENDING_LIMIT {
                reached_reconcile_limit = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        assert!(
            reached_reconcile_limit,
            "reconciliation should stop at its reserved capacity"
        );
        objects
            .put_bytes(&base.join("live_recording.wav"), b"live", "audio/wav")
            .expect("live recording uploads should have reserved queue capacity");
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline && fake.puts.lock().unwrap().len() < STAGED_FILES + 1 {
            std::thread::sleep(Duration::from_millis(10));
        }
        objects.wait_uploads_idle();

        let puts = fake.puts.lock().unwrap();
        assert_eq!(puts.len(), STAGED_FILES + 1);
        let unique_keys: HashSet<&str> = puts.iter().map(|(key, _, _)| key.as_str()).collect();
        assert_eq!(unique_keys.len(), STAGED_FILES + 1);
        drop(puts);
        drop(objects);
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn reconcile_backs_off_when_staging_tree_is_unavailable() {
        let base = temp_dir("reconcile_missing_tree");
        let _ = std::fs::remove_dir_all(&base);
        let (fake, dyn_store) = fake_store();
        let objects = RecordingObjectStore::new(dyn_store, String::new(), 900, &base);

        objects.reconcile_staged_uploads();
        std::thread::sleep(Duration::from_millis(1800));
        let list_count = fake.lists.lock().unwrap().len();
        assert!(
            list_count <= 10,
            "missing staging tree caused excessive LIST calls: {list_count}"
        );

        let staged = base.join("meeting/audio.wav");
        std::fs::create_dir_all(staged.parent().unwrap()).unwrap();
        std::fs::write(&staged, b"audio").unwrap();
        let deadline = Instant::now() + Duration::from_secs(6);
        while Instant::now() < deadline && fake.puts.lock().unwrap().is_empty() {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(fake.puts.lock().unwrap().len(), 1);
        drop(objects);
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn reconcile_stops_after_store_shutdown_during_listing() {
        let base = temp_dir("reconcile_shutdown_list");
        let _ = std::fs::remove_dir_all(&base);
        let (fake, dyn_store) = fake_store();
        *fake.list_delay.lock().unwrap() = Duration::from_millis(100);
        let objects = RecordingObjectStore::new(dyn_store, String::new(), 900, &base);

        objects.reconcile_staged_uploads();
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline && fake.lists.lock().unwrap().is_empty() {
            std::thread::sleep(Duration::from_millis(2));
        }
        assert!(!fake.lists.lock().unwrap().is_empty());
        let started = Instant::now();
        drop(objects);
        assert!(started.elapsed() < Duration::from_secs(2));
        std::thread::sleep(Duration::from_millis(200));
        assert_eq!(
            fake.lists.lock().unwrap().len(),
            1,
            "shutdown should stop before the object-prefix LIST"
        );

        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn reconcile_checks_tombstones_created_during_remote_scan() {
        let base = temp_dir("reconcile_tombstone_during_scan");
        let _ = std::fs::remove_dir_all(&base);
        let (fake, dyn_store) = fake_store();
        *fake.list_delay.lock().unwrap() = Duration::from_millis(100);
        let objects = RecordingObjectStore::new(dyn_store, String::new(), 900, &base);
        let staged = base.join("meeting/audio.wav");
        std::fs::create_dir_all(staged.parent().unwrap()).unwrap();
        std::fs::write(&staged, b"audio").unwrap();

        objects.reconcile_staged_uploads();
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline && fake.lists.lock().unwrap().is_empty() {
            std::thread::sleep(Duration::from_millis(2));
        }
        assert_eq!(fake.lists.lock().unwrap().as_slice(), &[String::new()]);
        fake.list_result
            .lock()
            .unwrap()
            .push(".tombstones/meeting/".to_owned());

        let deadline = Instant::now() + Duration::from_secs(3);
        while Instant::now() < deadline && fake.lists.lock().unwrap().len() < 2 {
            std::thread::sleep(Duration::from_millis(2));
        }
        objects.wait_uploads_idle();
        assert!(fake.puts.lock().unwrap().is_empty());
        drop(objects);
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn tombstone_index_matches_prefixes_without_scanning_every_tombstone() {
        let mut index = TombstoneIndex::default();
        index.insert("workspaces/g/vc/m/audio/".to_owned());
        index.insert("legacy/meeting".to_owned());

        assert!(index.contains_prefix_of("workspaces/g/vc/m/audio/chunk.wav"));
        assert!(index.contains_prefix_of("legacy/meeting/speakers/u.wav"));
        assert!(!index.contains_prefix_of("workspaces/g/vc/other/audio/chunk.wav"));
    }

    #[test]
    fn reconciliation_backpressure_uses_its_capped_delay_after_an_outage() {
        let scan = ReconcileScan {
            scanned: 64,
            queued: RECONCILE_PENDING_LIMIT,
            complete: false,
            backpressured: true,
        };
        let mut retry_delay = RECONCILE_RETRY_MAX;

        assert_eq!(
            incomplete_scan_retry_delay(&scan, &mut retry_delay),
            RECONCILE_BACKPRESSURE_RETRY_MAX
        );
        assert_eq!(retry_delay, RECONCILE_RETRY_BASE);
    }

    /// A rejected enqueue is fully rolled back: it can neither supersede
    /// an accepted older write nor weaken supersession by a delivered newer
    /// write for the same key.
    #[test]
    fn rejected_enqueue_never_supersedes_and_delivered_wins() {
        let mut state = GenerationState::default();
        // Rejected gen2 must not supersede accepted gen1.
        state.track("k", 1);
        state.track("k", 2);
        state.untrack("k", 2);
        assert!(!state.is_superseded("k", 1));
        // A delivered gen2 keeps superseding the delayed gen1...
        state.track("k", 2);
        state.settle("k", 2, true);
        assert!(state.is_superseded("k", 1));
        // ...even when a still-newer enqueue is rejected afterwards.
        state.track("k", 3);
        state.untrack("k", 3);
        assert!(state.is_superseded("k", 1));
        // Once gen1 is gone the key is forgotten entirely.
        state.settle("k", 1, false);
        assert!(!state.is_superseded("k", 4));
    }

    #[test]
    fn presigned_get_uses_configured_ttl() {
        let (_, dyn_store) = fake_store();
        let store = RecordingObjectStore::new(dyn_store, String::new(), 42, "/data");
        assert_eq!(store.presigned_get("k"), "https://example.test/k?ttl=42");
    }
}
