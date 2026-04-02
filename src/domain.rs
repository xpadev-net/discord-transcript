#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MeetingStatus {
    Scheduled,
    Recording,
    Stopping,
    Transcribing,
    Summarizing,
    Posted,
    Failed,
    Aborted,
}

impl MeetingStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Scheduled => "scheduled",
            Self::Recording => "recording",
            Self::Stopping => "stopping",
            Self::Transcribing => "transcribing",
            Self::Summarizing => "summarizing",
            Self::Posted => "posted",
            Self::Failed => "failed",
            Self::Aborted => "aborted",
        }
    }

    pub fn from_str(value: &str) -> Option<Self> {
        match value {
            "scheduled" => Some(Self::Scheduled),
            "recording" => Some(Self::Recording),
            "stopping" => Some(Self::Stopping),
            "transcribing" => Some(Self::Transcribing),
            "summarizing" => Some(Self::Summarizing),
            "posted" => Some(Self::Posted),
            "failed" => Some(Self::Failed),
            "aborted" => Some(Self::Aborted),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopReason {
    Manual,
    AutoEmpty,
    ClientDisconnect,
    Error,
}

impl StopReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Manual => "manual",
            Self::AutoEmpty => "auto_empty",
            Self::ClientDisconnect => "client_disconnect",
            Self::Error => "error",
        }
    }

    pub fn from_str(value: &str) -> Option<Self> {
        match value {
            "manual" => Some(Self::Manual),
            "auto_empty" => Some(Self::AutoEmpty),
            "client_disconnect" => Some(Self::ClientDisconnect),
            "error" => Some(Self::Error),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobType {
    Transcribe,
    Summarize,
    Cleanup,
}

impl JobType {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Transcribe => "transcribe",
            Self::Summarize => "summarize",
            Self::Cleanup => "cleanup",
        }
    }

    pub fn from_str(value: &str) -> Option<Self> {
        match value {
            "transcribe" => Some(Self::Transcribe),
            "summarize" => Some(Self::Summarize),
            "cleanup" => Some(Self::Cleanup),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobStatus {
    Queued,
    Running,
    Failed,
    Done,
}

impl JobStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Running => "running",
            Self::Failed => "failed",
            Self::Done => "done",
        }
    }

    pub fn from_str(value: &str) -> Option<Self> {
        match value {
            "queued" => Some(Self::Queued),
            "running" => Some(Self::Running),
            "failed" => Some(Self::Failed),
            "done" => Some(Self::Done),
            _ => None,
        }
    }
}
