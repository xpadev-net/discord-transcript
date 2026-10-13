use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::sync::mpsc;

pub const DEFAULT_LIVE_TRANSCRIPTION_QUEUE_LIMIT: usize = 32;

/// Bounded admission for persisted live-transcription metadata. A single
/// receiver worker performs ASR, so this queue never creates per-chunk waiter
/// tasks or copies recording PCM into memory.
pub struct LiveTranscriptionDispatcher<T> {
    sender: mpsc::Sender<T>,
    accepting: Arc<AtomicBool>,
}

impl<T> Clone for LiveTranscriptionDispatcher<T> {
    fn clone(&self) -> Self {
        Self {
            sender: self.sender.clone(),
            accepting: Arc::clone(&self.accepting),
        }
    }
}

impl<T> LiveTranscriptionDispatcher<T> {
    pub fn channel(capacity: usize) -> (Self, mpsc::Receiver<T>) {
        assert!(capacity > 0, "live transcription queue must be non-empty");
        let (sender, receiver) = mpsc::channel(capacity);
        (
            Self {
                sender,
                accepting: Arc::new(AtomicBool::new(true)),
            },
            receiver,
        )
    }

    pub fn try_enqueue(&self, item: T) -> Result<(), T> {
        if !self.accepting.load(Ordering::Acquire) {
            return Err(item);
        }
        self.sender
            .try_send(item)
            .map_err(|error| error.into_inner())
    }

    pub fn stop_accepting(&self) {
        self.accepting.store(false, Ordering::Release);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;
    use std::time::Duration;
    use tokio::sync::{Notify, Semaphore};

    #[tokio::test]
    async fn slow_worker_keeps_in_flight_plus_queue_within_bound() {
        let (dispatcher, mut receiver) = LiveTranscriptionDispatcher::channel(2);
        let started = Arc::new(Notify::new());
        let release = Arc::new(Semaphore::new(0));
        let completed = Arc::new(AtomicUsize::new(0));
        let worker_started = Arc::clone(&started);
        let worker_release = Arc::clone(&release);
        let worker_completed = Arc::clone(&completed);
        let worker = tokio::spawn(async move {
            while let Some(_item) = receiver.recv().await {
                worker_started.notify_one();
                worker_release.acquire().await.unwrap().forget();
                worker_completed.fetch_add(1, Ordering::AcqRel);
            }
        });

        dispatcher.try_enqueue(1).expect("first item should fit");
        tokio::time::timeout(Duration::from_millis(100), started.notified())
            .await
            .expect("worker should start the first item");
        dispatcher
            .try_enqueue(2)
            .expect("first queued item should fit");
        dispatcher
            .try_enqueue(3)
            .expect("second queued item should fit");
        assert_eq!(dispatcher.try_enqueue(4), Err(4));
        assert_eq!(completed.load(Ordering::Acquire), 0);

        dispatcher.stop_accepting();
        assert_eq!(dispatcher.try_enqueue(5), Err(5));
        release.add_permits(3);
        drop(dispatcher);
        worker.await.expect("dispatcher worker should finish");
        assert_eq!(completed.load(Ordering::Acquire), 3);
    }
}
