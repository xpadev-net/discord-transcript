use crate::domain::{JobStatus, JobType};
use std::collections::VecDeque;
use std::fmt::{Display, Formatter};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Job {
    pub id: String,
    pub meeting_id: String,
    pub job_type: JobType,
    pub status: JobStatus,
    pub retry_count: u32,
    pub error_message: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QueueError {
    Backend(String),
    AlreadyExists { job_id: String },
    NotFound { job_id: String },
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
    fn mark_done(&mut self, job_id: &str) -> Result<(), QueueError>;
    fn mark_failed(&mut self, job_id: &str, error_message: String) -> Result<(), QueueError>;
    fn retry(
        &mut self,
        job_id: &str,
        error_message: String,
        max_retries: u32,
    ) -> Result<JobStatus, QueueError>;
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
        for job_id in &self.order {
            let Some(job) = self.jobs.iter_mut().find(|j| j.id == *job_id) else {
                continue;
            };
            if job.job_type == job_type && job.status == JobStatus::Queued {
                job.status = JobStatus::Running;
                return Ok(Some(job.clone()));
            }
        }
        Ok(None)
    }

    fn claim_by_id(&mut self, job_id: &str) -> Result<Option<Job>, QueueError> {
        let Some(job) = self.jobs.iter_mut().find(|job| job.id == job_id) else {
            return Ok(None);
        };
        if job.status != JobStatus::Queued {
            return Ok(None);
        }
        job.status = JobStatus::Running;
        Ok(Some(job.clone()))
    }

    fn mark_done(&mut self, job_id: &str) -> Result<(), QueueError> {
        let Some(job) = self.jobs.iter_mut().find(|job| job.id == job_id) else {
            return Err(QueueError::NotFound {
                job_id: job_id.to_owned(),
            });
        };
        if job.status != JobStatus::Running {
            return Err(QueueError::InvalidState {
                job_id: job_id.to_owned(),
                expected: "running".to_owned(),
                actual: job.status.as_str().to_owned(),
            });
        }
        job.status = JobStatus::Done;
        job.error_message = None;
        Ok(())
    }

    fn mark_failed(&mut self, job_id: &str, error_message: String) -> Result<(), QueueError> {
        let Some(job) = self.jobs.iter_mut().find(|job| job.id == job_id) else {
            return Err(QueueError::NotFound {
                job_id: job_id.to_owned(),
            });
        };
        if job.status != JobStatus::Running {
            return Err(QueueError::InvalidState {
                job_id: job_id.to_owned(),
                expected: "running".to_owned(),
                actual: job.status.as_str().to_owned(),
            });
        }
        job.status = JobStatus::Failed;
        job.error_message = Some(error_message);
        Ok(())
    }

    fn retry(
        &mut self,
        job_id: &str,
        error_message: String,
        max_retries: u32,
    ) -> Result<JobStatus, QueueError> {
        let Some(job) = self.jobs.iter_mut().find(|job| job.id == job_id) else {
            return Err(QueueError::NotFound {
                job_id: job_id.to_owned(),
            });
        };
        if job.status != JobStatus::Running {
            return Err(QueueError::InvalidState {
                job_id: job_id.to_owned(),
                expected: "running".to_owned(),
                actual: job.status.as_str().to_owned(),
            });
        }
        job.retry_count += 1;
        job.error_message = Some(error_message);
        if job.retry_count > max_retries {
            job.status = JobStatus::Failed;
        } else {
            job.status = JobStatus::Queued;
        }
        Ok(job.status)
    }
}
