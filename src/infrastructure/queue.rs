use crate::domain::{JobStatus, JobType};
use chrono::{DateTime, Duration, Utc};
use std::collections::VecDeque;
use std::fmt::{Display, Formatter};
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Job {
    pub id: String,
    pub meeting_id: String,
    pub job_type: JobType,
    pub status: JobStatus,
    pub retry_count: u32,
    pub error_message: Option<String>,
    pub claim_token: Option<String>,
    pub next_run_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QueueError {
    Backend(String),
    AlreadyExists {
        job_id: String,
    },
    NotFound {
        job_id: String,
    },
    MissingClaimToken {
        job_id: String,
    },
    /// Job exists but is not in the expected state (e.g. not `Running`
    /// when `mark_done`/`mark_failed`/`retry` is called).
    InvalidState {
        job_id: String,
        expected: String,
        actual: String,
    },
}

impl Display for QueueError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Backend(err) => write!(f, "queue backend error: {err}"),
            Self::AlreadyExists { job_id } => write!(f, "job already exists: {job_id}"),
            Self::NotFound { job_id } => write!(f, "job not found: {job_id}"),
            Self::MissingClaimToken { job_id } => {
                write!(f, "job claim token is required: {job_id}")
            }
            Self::InvalidState {
                job_id,
                expected,
                actual,
            } => write!(
                f,
                "job {job_id} in wrong state: expected {expected}, got {actual}"
            ),
        }
    }
}

impl std::error::Error for QueueError {}

pub trait JobQueue {
    fn enqueue(&mut self, job: Job) -> Result<(), QueueError>;
    fn claim_next(&mut self, job_type: JobType) -> Result<Option<Job>, QueueError>;
    fn claim_by_id(&mut self, job_id: &str) -> Result<Option<Job>, QueueError>;
    fn heartbeat(&mut self, job: &Job) -> Result<(), QueueError>;
    fn mark_done(&mut self, job: &Job) -> Result<(), QueueError>;
    fn mark_failed(&mut self, job: &Job, error_message: String) -> Result<(), QueueError>;
    fn retry(
        &mut self,
        job: &Job,
        error_message: String,
        max_retries: u32,
    ) -> Result<JobStatus, QueueError>;
}

pub fn require_claim_token(job: &Job) -> Result<&str, QueueError> {
    job.claim_token
        .as_deref()
        .filter(|token| !token.is_empty())
        .ok_or_else(|| QueueError::MissingClaimToken {
            job_id: job.id.clone(),
        })
}

#[derive(Debug, Default)]
pub struct InMemoryJobQueue {
    jobs: Vec<Job>,
    order: VecDeque<String>,
}

impl InMemoryJobQueue {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn get(&self, job_id: &str) -> Option<&Job> {
        self.jobs.iter().find(|job| job.id == job_id)
    }

    pub fn cancel(&mut self, job_id: &str) -> Result<(), QueueError> {
        let Some(job) = self.jobs.iter_mut().find(|job| job.id == job_id) else {
            return Err(QueueError::NotFound {
                job_id: job_id.to_owned(),
            });
        };
        if job.status != JobStatus::Queued {
            return Err(QueueError::InvalidState {
                job_id: job_id.to_owned(),
                expected: "queued".to_owned(),
                actual: job.status.as_str().to_owned(),
            });
        }
        job.status = JobStatus::Canceled;
        job.next_run_at = None;
        job.claim_token = None;
        job.error_message = None;
        Ok(())
    }
}

pub fn retry_delay_seconds(retry_count: u32) -> u64 {
    let multiplier = 1_u64 << retry_count.saturating_sub(1).min(5);
    (30 * multiplier).min(900)
}

fn retry_delay(retry_count: u32) -> Duration {
    Duration::seconds(retry_delay_seconds(retry_count) as i64)
}

fn due_for_claim(job: &Job, now: DateTime<Utc>) -> bool {
    job.next_run_at.is_none_or(|next_run_at| next_run_at <= now)
}

impl JobQueue for InMemoryJobQueue {
    fn enqueue(&mut self, job: Job) -> Result<(), QueueError> {
        if self.jobs.iter().any(|existing| existing.id == job.id) {
            return Err(QueueError::AlreadyExists { job_id: job.id });
        }
        self.order.push_back(job.id.clone());
        self.jobs.push(job);
        Ok(())
    }

    fn claim_next(&mut self, job_type: JobType) -> Result<Option<Job>, QueueError> {
        let now = Utc::now();
        for job_id in &self.order {
            let Some(job) = self.jobs.iter_mut().find(|j| j.id == *job_id) else {
                continue;
            };
            if job.job_type == job_type
                && job.status == JobStatus::Queued
                && due_for_claim(job, now)
            {
                job.status = JobStatus::Running;
                job.claim_token = Some(Uuid::new_v4().to_string());
                job.next_run_at = None;
                return Ok(Some(job.clone()));
            }
        }
        Ok(None)
    }

    fn claim_by_id(&mut self, job_id: &str) -> Result<Option<Job>, QueueError> {
        let Some(job) = self.jobs.iter_mut().find(|job| job.id == job_id) else {
            return Ok(None);
        };
        if job.status != JobStatus::Queued || !due_for_claim(job, Utc::now()) {
            return Ok(None);
        }
        job.status = JobStatus::Running;
        job.claim_token = Some(Uuid::new_v4().to_string());
        job.next_run_at = None;
        Ok(Some(job.clone()))
    }

    fn heartbeat(&mut self, job: &Job) -> Result<(), QueueError> {
        let claim_token = require_claim_token(job)?;
        let Some(current) = self.jobs.iter().find(|current| current.id == job.id) else {
            return Err(QueueError::NotFound {
                job_id: job.id.clone(),
            });
        };
        if current.status != JobStatus::Running
            || current.claim_token.as_deref() != Some(claim_token)
        {
            return Err(QueueError::InvalidState {
                job_id: job.id.clone(),
                expected: "running with matching claim token".to_owned(),
                actual: current.status.as_str().to_owned(),
            });
        }
        Ok(())
    }

    fn mark_done(&mut self, job: &Job) -> Result<(), QueueError> {
        let claim_token = require_claim_token(job)?;
        let Some(current) = self.jobs.iter_mut().find(|current| current.id == job.id) else {
            return Err(QueueError::NotFound {
                job_id: job.id.clone(),
            });
        };
        if current.status != JobStatus::Running
            || current.claim_token.as_deref() != Some(claim_token)
        {
            return Err(QueueError::InvalidState {
                job_id: job.id.clone(),
                expected: "running with matching claim token".to_owned(),
                actual: current.status.as_str().to_owned(),
            });
        }
        current.status = JobStatus::Done;
        current.error_message = None;
        current.next_run_at = None;
        current.claim_token = None;
        Ok(())
    }

    fn mark_failed(&mut self, job: &Job, error_message: String) -> Result<(), QueueError> {
        let claim_token = require_claim_token(job)?;
        let Some(current) = self.jobs.iter_mut().find(|current| current.id == job.id) else {
            return Err(QueueError::NotFound {
                job_id: job.id.clone(),
            });
        };
        if current.status != JobStatus::Running
            || current.claim_token.as_deref() != Some(claim_token)
        {
            return Err(QueueError::InvalidState {
                job_id: job.id.clone(),
                expected: "running with matching claim token".to_owned(),
                actual: current.status.as_str().to_owned(),
            });
        }
        current.status = JobStatus::Failed;
        current.error_message = Some(error_message);
        current.next_run_at = None;
        current.claim_token = None;
        Ok(())
    }

    fn retry(
        &mut self,
        job: &Job,
        error_message: String,
        max_retries: u32,
    ) -> Result<JobStatus, QueueError> {
        let claim_token = require_claim_token(job)?;
        let Some(current) = self.jobs.iter_mut().find(|current| current.id == job.id) else {
            return Err(QueueError::NotFound {
                job_id: job.id.clone(),
            });
        };
        if current.status != JobStatus::Running
            || current.claim_token.as_deref() != Some(claim_token)
        {
            return Err(QueueError::InvalidState {
                job_id: job.id.clone(),
                expected: "running with matching claim token".to_owned(),
                actual: current.status.as_str().to_owned(),
            });
        }
        current.retry_count += 1;
        current.error_message = Some(error_message);
        current.claim_token = None;
        if current.retry_count > max_retries {
            current.status = JobStatus::Failed;
            current.next_run_at = None;
        } else {
            current.status = JobStatus::Queued;
            current.next_run_at = Some(Utc::now() + retry_delay(current.retry_count));
        }
        Ok(current.status)
    }
}
