use chrono::{DateTime, Utc};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlanFallback {
    Default,
    Beta,
}

impl PlanFallback {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::Beta => "beta",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlanKind {
    Default,
    Beta,
    Custom,
}

impl PlanKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::Beta => "beta",
            Self::Custom => "custom",
        }
    }

    pub fn parse_str(value: &str) -> Option<Self> {
        match value {
            "default" => Some(Self::Default),
            "beta" => Some(Self::Beta),
            "custom" => Some(Self::Custom),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuotaDimension {
    RecordingMinutes,
    AsrSeconds,
    SummaryRuns,
    StorageBytes,
    DebugDownloads,
}

impl QuotaDimension {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::RecordingMinutes => "recording_minutes",
            Self::AsrSeconds => "asr_seconds",
            Self::SummaryRuns => "summary_runs",
            Self::StorageBytes => "storage_bytes",
            Self::DebugDownloads => "debug_downloads",
        }
    }

    pub fn parse_str(value: &str) -> Option<Self> {
        match value {
            "recording_minutes" => Some(Self::RecordingMinutes),
            "asr_seconds" => Some(Self::AsrSeconds),
            "summary_runs" => Some(Self::SummaryRuns),
            "storage_bytes" => Some(Self::StorageBytes),
            "debug_downloads" => Some(Self::DebugDownloads),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuotaPeriod {
    Daily,
    Monthly,
    Total,
    Current,
}

impl QuotaPeriod {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Daily => "daily",
            Self::Monthly => "monthly",
            Self::Total => "total",
            Self::Current => "current",
        }
    }

    pub fn parse_str(value: &str) -> Option<Self> {
        match value {
            "daily" => Some(Self::Daily),
            "monthly" => Some(Self::Monthly),
            "total" => Some(Self::Total),
            "current" => Some(Self::Current),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuotaEnforcementMode {
    ObserveOnly,
    Enforce,
}

impl QuotaEnforcementMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ObserveOnly => "observe_only",
            Self::Enforce => "enforce",
        }
    }

    pub fn parse_str(value: &str) -> Option<Self> {
        match value {
            "observe_only" => Some(Self::ObserveOnly),
            "enforce" => Some(Self::Enforce),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuotaLimit {
    Finite(i64),
    Unlimited,
}

impl QuotaLimit {
    pub fn from_parts(unlimited: bool, limit_value: Option<i64>) -> Result<Self, String> {
        match (unlimited, limit_value) {
            (true, None) => Ok(Self::Unlimited),
            (true, Some(_)) => Err("unlimited quota must not have a limit value".to_owned()),
            (false, Some(value)) if value >= 0 => Ok(Self::Finite(value)),
            (false, Some(value)) => Err(format!("finite quota limit must be nonnegative: {value}")),
            (false, None) => Err("finite quota must have a limit value".to_owned()),
        }
    }

    pub fn limit_value(self) -> Option<i64> {
        match self {
            Self::Finite(value) => Some(value),
            Self::Unlimited => None,
        }
    }

    pub fn is_unlimited(self) -> bool {
        matches!(self, Self::Unlimited)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanQuota {
    pub id: String,
    pub dimension: QuotaDimension,
    pub period: QuotaPeriod,
    pub limit: QuotaLimit,
    pub enforcement_mode: QuotaEnforcementMode,
}

impl PlanQuota {
    pub fn from_parts(
        id: String,
        dimension: QuotaDimension,
        period: QuotaPeriod,
        unlimited: bool,
        limit_value: Option<i64>,
        enforcement_mode: QuotaEnforcementMode,
    ) -> Result<Self, String> {
        Ok(Self {
            id,
            dimension,
            period,
            limit: QuotaLimit::from_parts(unlimited, limit_value)?,
            enforcement_mode,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedPlan {
    pub assignment_id: Option<String>,
    pub tenant_id: Option<String>,
    pub guild_id: String,
    pub plan_id: String,
    pub plan_code: String,
    pub plan_name: String,
    pub plan_kind: PlanKind,
    pub resolution_source: String,
    pub assignment_source: Option<String>,
    pub valid_from: Option<DateTime<Utc>>,
    pub valid_until: Option<DateTime<Utc>>,
    pub quotas: Vec<PlanQuota>,
}
