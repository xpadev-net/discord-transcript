use chrono::{DateTime, Utc};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum UsageMetric {
    RecordingMinutes,
    AsrSeconds,
    SummaryRuns,
    StorageBytes,
    DebugDownloads,
}

impl UsageMetric {
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsageEvent {
    pub id: String,
    pub tenant_id: Option<String>,
    pub guild_id: String,
    pub meeting_id: Option<String>,
    pub job_id: Option<String>,
    pub resource_type: Option<String>,
    pub resource_id: Option<String>,
    pub metric: UsageMetric,
    pub quantity: i64,
    pub detail_json: String,
    pub observed_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewUsageEvent {
    pub id: String,
    pub tenant_id: Option<String>,
    pub guild_id: String,
    pub meeting_id: Option<String>,
    pub job_id: Option<String>,
    pub resource_type: Option<String>,
    pub resource_id: Option<String>,
    pub metric: UsageMetric,
    pub quantity: i64,
    pub detail_json: String,
    pub observed_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct UsageSnapshot {
    quantities: BTreeMap<UsageMetric, i64>,
}

impl UsageSnapshot {
    pub fn from_aggregates(aggregates: Vec<UsageAggregate>) -> Self {
        let quantities = aggregates
            .into_iter()
            .map(|aggregate| (aggregate.metric, aggregate.quantity))
            .collect();
        Self { quantities }
    }

    pub fn quantity(&self, metric: UsageMetric) -> i64 {
        self.quantities.get(&metric).copied().unwrap_or(0)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsageAggregate {
    pub metric: UsageMetric,
    pub quantity: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntitlementMode {
    ObserveOnly,
    Enforce,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntitlementAction {
    StartRecording,
    CompleteWorker,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct EntitlementPolicy {
    pub recording_minutes_limit: Option<i64>,
    pub asr_seconds_limit: Option<i64>,
    pub summary_runs_limit: Option<i64>,
    pub storage_bytes_limit: Option<i64>,
    pub debug_downloads_limit: Option<i64>,
}

impl EntitlementPolicy {
    pub fn observe_all() -> Self {
        Self {
            recording_minutes_limit: Some(i64::MAX),
            asr_seconds_limit: Some(i64::MAX),
            summary_runs_limit: Some(i64::MAX),
            storage_bytes_limit: Some(i64::MAX),
            debug_downloads_limit: Some(i64::MAX),
        }
    }

    fn limit_for(&self, metric: UsageMetric) -> Option<i64> {
        match metric {
            UsageMetric::RecordingMinutes => self.recording_minutes_limit,
            UsageMetric::AsrSeconds => self.asr_seconds_limit,
            UsageMetric::SummaryRuns => self.summary_runs_limit,
            UsageMetric::StorageBytes => self.storage_bytes_limit,
            UsageMetric::DebugDownloads => self.debug_downloads_limit,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EntitlementObservation {
    pub metric: UsageMetric,
    pub quantity: i64,
    pub limit: i64,
    pub exceeded: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EntitlementDecision {
    pub mode: EntitlementMode,
    pub action: EntitlementAction,
    pub allowed: bool,
    pub observations: Vec<EntitlementObservation>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EntitlementEvaluator {
    mode: EntitlementMode,
    policy: EntitlementPolicy,
}

impl EntitlementEvaluator {
    pub fn observe_only() -> Self {
        Self {
            mode: EntitlementMode::ObserveOnly,
            policy: EntitlementPolicy::observe_all(),
        }
    }

    pub fn new(mode: EntitlementMode, policy: EntitlementPolicy) -> Self {
        Self { mode, policy }
    }

    pub fn evaluate(
        &self,
        action: EntitlementAction,
        snapshot: &UsageSnapshot,
    ) -> EntitlementDecision {
        let observations = [
            UsageMetric::RecordingMinutes,
            UsageMetric::AsrSeconds,
            UsageMetric::SummaryRuns,
            UsageMetric::StorageBytes,
            UsageMetric::DebugDownloads,
        ]
        .into_iter()
        .filter_map(|metric| {
            let limit = self.policy.limit_for(metric)?;
            let quantity = snapshot.quantity(metric);
            Some(EntitlementObservation {
                metric,
                quantity,
                limit,
                exceeded: quantity > limit,
            })
        })
        .collect::<Vec<_>>();
        let allowed = match self.mode {
            EntitlementMode::ObserveOnly => true,
            EntitlementMode::Enforce => {
                observations.iter().all(|observation| !observation.exceeded)
            }
        };
        EntitlementDecision {
            mode: self.mode,
            action,
            allowed,
            observations,
        }
    }
}

#[derive(Debug, Default)]
pub struct UsageEventLedger {
    events: Vec<UsageEvent>,
}

impl UsageEventLedger {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn append(&mut self, event: UsageEvent) {
        if self.events.iter().any(|existing| existing.id == event.id) {
            return;
        }
        self.events.push(event);
    }

    pub fn list(&self) -> &[UsageEvent] {
        &self.events
    }

    pub fn recent(&self, limit: usize) -> Vec<UsageEvent> {
        let mut events = self.events.clone();
        events.sort_by(|left, right| {
            right
                .observed_at
                .cmp(&left.observed_at)
                .then_with(|| right.created_at.cmp(&left.created_at))
                .then_with(|| right.id.cmp(&left.id))
        });
        events.truncate(limit);
        events
    }
}

pub fn recording_minutes_from_seconds(seconds: u64) -> i64 {
    seconds.div_ceil(60).min(i64::MAX as u64) as i64
}
