use crate::audio::recording_session::{PersistedChunk, PreparedChunk};
use crate::infrastructure::storage_fs::ChunkStorage;
use std::future::Future;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;
use std::sync::Mutex as StdMutex;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use tokio::sync::{Notify, mpsc, oneshot};
use tokio::task::JoinHandle;

pub const DEFAULT_RECORDING_WRITER_TASK_LIMIT: usize = 64;
pub const DEFAULT_RECORDING_WRITER_BYTE_LIMIT: usize = 256 * 1024 * 1024;

#[derive(Debug)]
pub enum ChunkWriteOutcome {
    Persisted(PersistedChunk),
    Failed { chunk: PreparedChunk, error: String },
}

enum WriterMessage<S> {
    Save {
        chunk: PreparedChunk,
        storage: S,
        reply: oneshot::Sender<ChunkWriteOutcome>,
    },
    Shutdown(oneshot::Sender<()>),
}

struct WriterState {
    admission: StdMutex<()>,
    pending_tasks: AtomicUsize,
    pending_bytes: AtomicUsize,
    closed: AtomicBool,
    drained: Notify,
    max_tasks: usize,
    max_bytes: usize,
}

struct PendingWritePermit {
    state: Arc<WriterState>,
    bytes: usize,
}

impl Drop for PendingWritePermit {
    fn drop(&mut self) {
        self.state
            .pending_bytes
            .fetch_sub(self.bytes, Ordering::AcqRel);
        if self.state.pending_tasks.fetch_sub(1, Ordering::AcqRel) == 1 {
            self.state.drained.notify_waiters();
        }
    }
}

pub struct ChunkWriteReceipt {
    reply: oneshot::Receiver<ChunkWriteOutcome>,
    _permit: PendingWritePermit,
}

pub struct ChunkWriteCompletion {
    pub outcome: ChunkWriteOutcome,
    // Keep the capacity reservation until the caller has acknowledged the
    // result (including retaining a failed prepared chunk in the session).
    _permit: PendingWritePermit,
}

impl ChunkWriteCompletion {
    pub async fn process<F, Fut, Output>(self, handle: F) -> Output
    where
        F: FnOnce(ChunkWriteOutcome) -> Fut,
        Fut: Future<Output = Output>,
    {
        let output = handle(self.outcome).await;
        // The remaining capacity permit is released after acknowledgement
        // handling completes.
        output
    }
}

impl ChunkWriteReceipt {
    pub async fn wait(self) -> ChunkWriteCompletion {
        let outcome = self
            .reply
            .await
            .expect("recording writer must return an outcome for every accepted chunk");
        ChunkWriteCompletion {
            outcome,
            _permit: self._permit,
        }
    }
}

/// Single-worker, bounded queue for synchronous chunk storage. Enqueueing is
/// always non-blocking; callers retain the returned chunk when the queue is
/// full and can await `drain` before final synchronous flushes.
#[derive(Clone)]
pub struct BoundedRecordingWriter<S: ChunkStorage + Send + Sync + 'static> {
    sender: mpsc::Sender<WriterMessage<S>>,
    state: Arc<WriterState>,
    worker: Arc<tokio::sync::Mutex<Option<JoinHandle<()>>>>,
    shutdown_gate: Arc<tokio::sync::Mutex<()>>,
}

impl<S: ChunkStorage + Send + Sync + 'static> BoundedRecordingWriter<S> {
    pub fn new(max_tasks: usize, max_bytes: usize) -> Self {
        assert!(
            max_tasks > 0,
            "recording writer task bound must be positive"
        );
        assert!(
            max_bytes > 0,
            "recording writer byte bound must be positive"
        );
        let (sender, mut receiver) = mpsc::channel(max_tasks);
        let state = Arc::new(WriterState {
            admission: StdMutex::new(()),
            pending_tasks: AtomicUsize::new(0),
            pending_bytes: AtomicUsize::new(0),
            closed: AtomicBool::new(false),
            drained: Notify::new(),
            max_tasks,
            max_bytes,
        });
        let worker = tokio::spawn(async move {
            while let Some(message) = receiver.recv().await {
                match message {
                    WriterMessage::Save {
                        chunk,
                        storage,
                        reply,
                    } => {
                        let recovery = Arc::new(StdMutex::new(Some(chunk)));
                        let recovery_in_task = Arc::clone(&recovery);
                        let saved = tokio::task::spawn_blocking(move || {
                            let mut chunk = recovery_in_task.lock().unwrap();
                            let result = catch_unwind(AssertUnwindSafe(|| {
                                chunk
                                    .as_ref()
                                    .expect("prepared chunk exists")
                                    .save(&storage)
                            }));
                            (chunk.take().expect("prepared chunk is preserved"), result)
                        })
                        .await;
                        let outcome = match saved {
                            Ok((chunk, Ok(Ok(saved)))) => {
                                ChunkWriteOutcome::Persisted(chunk.complete(saved))
                            }
                            Ok((chunk, Ok(Err(error)))) => ChunkWriteOutcome::Failed {
                                chunk,
                                error: error.to_string(),
                            },
                            Ok((chunk, Err(_))) => ChunkWriteOutcome::Failed {
                                chunk,
                                error: "chunk storage panicked".to_owned(),
                            },
                            Err(error) => {
                                let chunk = recovery
                                    .lock()
                                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                                    .take()
                                    .expect("failed writer task preserves prepared chunk");
                                ChunkWriteOutcome::Failed {
                                    chunk,
                                    error: format!("chunk storage task failed: {error}"),
                                }
                            }
                        };
                        let _ = reply.send(outcome);
                    }
                    WriterMessage::Shutdown(reply) => {
                        let _ = reply.send(());
                        break;
                    }
                }
            }
        });
        Self {
            sender,
            state,
            worker: Arc::new(tokio::sync::Mutex::new(Some(worker))),
            shutdown_gate: Arc::new(tokio::sync::Mutex::new(())),
        }
    }

    pub fn try_enqueue(
        &self,
        chunk: PreparedChunk,
        storage: S,
    ) -> Result<ChunkWriteReceipt, PreparedChunk> {
        let _admission = self
            .state
            .admission
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if self.state.closed.load(Ordering::Acquire) {
            return Err(chunk);
        }
        let bytes = chunk.wav().bytes.len();
        if !reserve_at_most(&self.state.pending_tasks, 1, self.state.max_tasks) {
            return Err(chunk);
        }
        if !reserve_at_most(&self.state.pending_bytes, bytes, self.state.max_bytes) {
            if self.state.pending_tasks.fetch_sub(1, Ordering::AcqRel) == 1 {
                self.state.drained.notify_waiters();
            }
            return Err(chunk);
        }

        let permit = PendingWritePermit {
            state: Arc::clone(&self.state),
            bytes,
        };
        let (reply, receiver) = oneshot::channel();
        let message = WriterMessage::Save {
            chunk,
            storage,
            reply,
        };
        match self.sender.try_send(message) {
            Ok(()) => Ok(ChunkWriteReceipt {
                reply: receiver,
                _permit: permit,
            }),
            Err(error) => {
                drop(permit);
                let message = error.into_inner();
                match message {
                    WriterMessage::Save { chunk, .. } => Err(chunk),
                    WriterMessage::Shutdown(_) => unreachable!("only save messages are sent here"),
                }
            }
        }
    }

    pub async fn drain(&self) {
        loop {
            let notified = self.state.drained.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if self.state.pending_tasks.load(Ordering::Acquire) == 0 {
                return;
            }
            notified.await;
        }
    }

    /// Stop accepting writes after all accepted results have been handled.
    pub async fn shutdown(&self) {
        let _shutdown = self.shutdown_gate.lock().await;
        let should_shutdown = {
            let _admission = self
                .state
                .admission
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            !self.state.closed.swap(true, Ordering::AcqRel)
        };
        if !should_shutdown {
            return;
        }
        self.drain().await;
        let (reply, receiver) = oneshot::channel();
        if self
            .sender
            .send(WriterMessage::Shutdown(reply))
            .await
            .is_ok()
        {
            let _ = receiver.await;
        }
        if let Some(worker) = self.worker.lock().await.take() {
            let _ = worker.await;
        }
    }
}

fn reserve_at_most(counter: &AtomicUsize, amount: usize, limit: usize) -> bool {
    let mut current = counter.load(Ordering::Acquire);
    loop {
        let Some(next) = current.checked_add(amount) else {
            return false;
        };
        if next > limit {
            return false;
        }
        match counter.compare_exchange_weak(current, next, Ordering::AcqRel, Ordering::Acquire) {
            Ok(_) => return true,
            Err(observed) => current = observed,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::receiver::{BufferedFrame, ReceiverConfig};
    use crate::audio::recording_session::RecordingSession;
    use crate::infrastructure::storage_fs::{ChunkStorageError, SavedChunk};
    use std::path::PathBuf;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    type BlockingGate = Arc<(Mutex<(bool, bool)>, std::sync::Condvar)>;

    #[derive(Debug, Clone)]
    struct MemoryStorage {
        failures_remaining: Arc<AtomicUsize>,
        gate: Option<BlockingGate>,
    }

    impl ChunkStorage for MemoryStorage {
        fn save_chunk(
            &self,
            _meeting_id: &str,
            user_id: &str,
            sequence: u64,
            start_ms: u64,
            bytes: &[u8],
        ) -> Result<SavedChunk, ChunkStorageError> {
            if let Some(gate) = &self.gate {
                let (state, changed) = &**gate;
                let mut state = state.lock().unwrap();
                state.0 = true;
                changed.notify_all();
                while !state.1 {
                    state = changed.wait(state).unwrap();
                }
            }
            if self
                .failures_remaining
                .try_update(Ordering::AcqRel, Ordering::Acquire, |remaining| {
                    remaining.checked_sub(1)
                })
                .is_ok()
            {
                return Err(ChunkStorageError::Io("injected write failure".to_owned()));
            }
            Ok(SavedChunk {
                path: PathBuf::from(format!("{user_id}_{sequence}_{start_ms}.wav")),
                size_bytes: bytes.len(),
            })
        }
    }

    fn prepared_chunks() -> Vec<PreparedChunk> {
        let storage = MemoryStorage {
            failures_remaining: Arc::new(AtomicUsize::new(0)),
            gate: None,
        };
        let mut session = RecordingSession::new(
            "meeting-1".to_owned(),
            storage,
            ReceiverConfig {
                chunk_duration: Duration::from_secs(20),
                silence_flush_duration: Duration::from_secs(30),
            },
            48_000,
        );
        [1_000, 2_000]
            .into_iter()
            .map(|timestamp_ms| {
                session.ingest_frame(
                    "user-1",
                    BufferedFrame {
                        timestamp_ms,
                        pcm_16le_bytes: vec![0, 0, 1, 0],
                    },
                );
                session
                    .prepare_all()
                    .expect("chunk preparation should succeed")
                    .pop()
                    .expect("one chunk should be prepared")
            })
            .collect()
    }

    #[tokio::test]
    async fn enqueue_is_nonblocking_bounded_and_drain_waits_for_ack_handling() {
        let writer = BoundedRecordingWriter::<MemoryStorage>::new(1, 1024 * 1024);
        let gate = Arc::new((Mutex::new((false, false)), std::sync::Condvar::new()));
        let storage = MemoryStorage {
            failures_remaining: Arc::new(AtomicUsize::new(0)),
            gate: Some(Arc::clone(&gate)),
        };
        let mut chunks = prepared_chunks().into_iter();
        let first = chunks.next().unwrap();
        let second = chunks.next().unwrap();
        let receipt = tokio::time::timeout(Duration::from_millis(100), async {
            writer.try_enqueue(first, storage.clone())
        })
        .await
        .expect("enqueue must not wait on the writer");
        let receipt = match receipt {
            Ok(receipt) => receipt,
            Err(_) => panic!("first write should fit in the queue"),
        };

        let gate_for_wait = Arc::clone(&gate);
        tokio::task::spawn_blocking(move || {
            let (state, changed) = &*gate_for_wait;
            let mut state = state.lock().unwrap();
            while !state.0 {
                state = changed.wait(state).unwrap();
            }
        })
        .await
        .unwrap();

        let rejected = match writer.try_enqueue(second, storage.clone()) {
            Err(chunk) => chunk,
            Ok(_) => panic!("task limit must return the chunk for bounded retention"),
        };
        assert_eq!(rejected.sequence(), 2);

        let drain_writer = writer.clone();
        let mut drain = tokio::spawn(async move { drain_writer.drain().await });
        assert!(
            tokio::time::timeout(Duration::from_millis(20), &mut drain)
                .await
                .is_err()
        );

        {
            let (state, changed) = &*gate;
            state.lock().unwrap().1 = true;
            changed.notify_all();
        }
        let completion = receipt.wait().await;
        let persisted = completion
            .process(|outcome| async move { matches!(outcome, ChunkWriteOutcome::Persisted(_)) })
            .await;
        assert!(persisted);
        drain.await.unwrap();
        writer.shutdown().await;
    }

    #[tokio::test]
    async fn failed_write_returns_the_same_prepared_chunk_for_retry() {
        let writer = BoundedRecordingWriter::<MemoryStorage>::new(2, 1024 * 1024);
        let storage = MemoryStorage {
            failures_remaining: Arc::new(AtomicUsize::new(1)),
            gate: None,
        };
        let chunk = prepared_chunks().remove(0);
        let receipt = writer
            .try_enqueue(chunk, storage.clone())
            .expect("initial write should fit");
        let completion = receipt.wait().await;
        let failed = completion
            .process(|outcome| async move {
                match outcome {
                    ChunkWriteOutcome::Failed { chunk, .. } => chunk,
                    ChunkWriteOutcome::Persisted(_) => {
                        panic!("injected storage error should fail")
                    }
                }
            })
            .await;
        assert_eq!(failed.sequence(), 1);
        assert_eq!(failed.start_ms(), 1_000);
        let retry = writer
            .try_enqueue(failed, storage)
            .expect("retry should fit after first acknowledgement");
        let completion = retry.wait().await;
        assert!(
            completion
                .process(
                    |outcome| async move { matches!(outcome, ChunkWriteOutcome::Persisted(_)) }
                )
                .await
        );
        writer.drain().await;
        writer.shutdown().await;
    }

    #[tokio::test]
    async fn byte_limit_rejects_oversized_chunks_without_queueing() {
        let writer = BoundedRecordingWriter::<MemoryStorage>::new(2, 1);
        let chunk = prepared_chunks().remove(0);
        let rejected = match writer.try_enqueue(
            chunk,
            MemoryStorage {
                failures_remaining: Arc::new(AtomicUsize::new(0)),
                gate: None,
            },
        ) {
            Err(chunk) => chunk,
            Ok(_) => panic!("WAV chunk should exceed the configured byte budget"),
        };
        assert_eq!(rejected.sequence(), 1);
        writer.drain().await;
        writer.shutdown().await;
    }

    #[tokio::test]
    async fn shutdown_closes_admission_and_waits_for_accepted_acknowledgements() {
        let writer = BoundedRecordingWriter::<MemoryStorage>::new(2, 1024 * 1024);
        let gate = Arc::new((Mutex::new((false, false)), std::sync::Condvar::new()));
        let storage = MemoryStorage {
            failures_remaining: Arc::new(AtomicUsize::new(0)),
            gate: Some(Arc::clone(&gate)),
        };
        let mut chunks = prepared_chunks().into_iter();
        let receipt = writer
            .try_enqueue(chunks.next().unwrap(), storage.clone())
            .expect("first write should fit");
        let drain_gate = Arc::clone(&gate);
        tokio::task::spawn_blocking(move || {
            let (state, changed) = &*drain_gate;
            let mut state = state.lock().unwrap();
            while !state.0 {
                state = changed.wait(state).unwrap();
            }
        })
        .await
        .unwrap();

        let shutdown_writer = writer.clone();
        let mut shutdown = tokio::spawn(async move { shutdown_writer.shutdown().await });
        tokio::time::timeout(Duration::from_millis(100), async {
            while !writer.state.closed.load(Ordering::Acquire) {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("shutdown should close admission before draining");

        let rejected = match writer.try_enqueue(chunks.next().unwrap(), storage) {
            Err(chunk) => chunk,
            Ok(_) => panic!("closed writer should reject new chunks"),
        };
        assert_eq!(rejected.sequence(), 2);
        assert!(
            tokio::time::timeout(Duration::from_millis(20), &mut shutdown)
                .await
                .is_err(),
            "shutdown must wait for the accepted write acknowledgement"
        );

        {
            let (state, changed) = &*gate;
            state.lock().unwrap().1 = true;
            changed.notify_all();
        }
        receipt.wait().await.process(|_| async {}).await;
        shutdown.await.unwrap();
    }
}
