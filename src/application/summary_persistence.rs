/// Persistence boundary for a generated meeting summary.
pub trait SummaryPersistencePort {
    fn persist_generated_summary(&mut self, meeting_id: &str, markdown: &str)
    -> Result<(), String>;
}

/// The runtime applies the follow-up after releasing its service lock.
#[derive(Debug, PartialEq, Eq)]
pub enum SummaryPersistenceOutcome<Context> {
    Persisted(Context),
    Retry { context: Context, error: String },
}

/// Persists a generated summary and returns the follow-up decision without
/// doing workspace IO while the persistence adapter is borrowed.
pub fn persist_summary<P, Context>(
    port: &mut P,
    meeting_id: &str,
    markdown: &str,
    context: Context,
) -> SummaryPersistenceOutcome<Context>
where
    P: SummaryPersistencePort,
{
    match port.persist_generated_summary(meeting_id, markdown) {
        Ok(()) => SummaryPersistenceOutcome::Persisted(context),
        Err(error) => SummaryPersistenceOutcome::Retry { context, error },
    }
}

#[cfg(test)]
mod tests {
    use super::{SummaryPersistenceOutcome, SummaryPersistencePort, persist_summary};

    struct FakeSummaryPersistence {
        result: Result<(), String>,
    }

    impl SummaryPersistencePort for FakeSummaryPersistence {
        fn persist_generated_summary(
            &mut self,
            _meeting_id: &str,
            _markdown: &str,
        ) -> Result<(), String> {
            self.result.clone()
        }
    }

    #[test]
    fn persistence_failure_returns_retry_outcome_with_workspace_context() {
        let mut port = FakeSummaryPersistence {
            result: Err("database unavailable".to_owned()),
        };

        let outcome = persist_summary(&mut port, "m1", "## Summary", "workspace-for-retry");

        assert_eq!(
            outcome,
            SummaryPersistenceOutcome::Retry {
                context: "workspace-for-retry",
                error: "database unavailable".to_owned(),
            }
        );
    }

    #[test]
    fn persistence_success_returns_workspace_for_cleanup() {
        let mut port = FakeSummaryPersistence { result: Ok(()) };

        let outcome = persist_summary(&mut port, "m1", "## Summary", "workspace-after-persist");

        assert_eq!(
            outcome,
            SummaryPersistenceOutcome::Persisted("workspace-after-persist")
        );
    }
}
