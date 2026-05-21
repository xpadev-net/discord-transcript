use std::num::NonZeroU32;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetentionKind {
    RawAudio,
    Transcript,
    Summary,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetentionPolicy {
    pub raw_audio_ttl_days: NonZeroU32,
    pub transcript_ttl_days: NonZeroU32,
    pub summary_ttl_days: Option<NonZeroU32>,
}

impl Default for RetentionPolicy {
    fn default() -> Self {
        Self {
            raw_audio_ttl_days: NonZeroU32::new(7).expect("default raw audio TTL is nonzero"),
            transcript_ttl_days: NonZeroU32::new(30).expect("default transcript TTL is nonzero"),
            summary_ttl_days: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ArtifactRecord {
    pub kind: RetentionKind,
    pub created_at_unix_seconds: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CleanupCandidate {
    pub artifact_index: usize,
    pub kind: RetentionKind,
}

pub fn should_delete_artifact(
    record: ArtifactRecord,
    now_unix_seconds: u64,
    policy: RetentionPolicy,
) -> bool {
    let age_days = (now_unix_seconds.saturating_sub(record.created_at_unix_seconds)) / 86_400;
    match record.kind {
        RetentionKind::RawAudio => age_days >= policy.raw_audio_ttl_days.get() as u64,
        RetentionKind::Transcript => age_days >= policy.transcript_ttl_days.get() as u64,
        RetentionKind::Summary => policy
            .summary_ttl_days
            .is_some_and(|days| age_days >= days.get() as u64),
    }
}

pub fn select_cleanup_candidates(
    records: &[ArtifactRecord],
    now_unix_seconds: u64,
    policy: RetentionPolicy,
) -> Vec<CleanupCandidate> {
    records
        .iter()
        .enumerate()
        .filter_map(|(index, record)| {
            if should_delete_artifact(*record, now_unix_seconds, policy) {
                Some(CleanupCandidate {
                    artifact_index: index,
                    kind: record.kind,
                })
            } else {
                None
            }
        })
        .collect()
}
