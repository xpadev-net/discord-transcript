/// Persistence boundary for a generated meeting summary.
pub trait SummaryPersistencePort {
    fn persist_generated_summary(&mut self, meeting_id: &str, markdown: &str)
    -> Result<(), String>;
}

/// Persists a generated summary before running either workspace follow-up.
/// Persistence failures retain the workspace for retry and never run cleanup.
pub fn persist_before_cleanup<P, C, F, Context>(
    port: &mut P,
    meeting_id: &str,
    markdown: &str,
    context: Context,
    retain_for_retry: F,
    cleanup_after_persist: C,
) -> Result<(), String>
where
    P: SummaryPersistencePort,
    C: FnOnce(Context),
    F: FnOnce(Context),
{
    match port.persist_generated_summary(meeting_id, markdown) {
        Ok(()) => {
            cleanup_after_persist(context);
            Ok(())
        }
        Err(err) => {
            retain_for_retry(context);
            Err(err)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{SummaryPersistencePort, persist_before_cleanup};
    use std::cell::Cell;

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
    fn persistence_failure_retains_workspace_without_cleanup() {
        let mut port = FakeSummaryPersistence {
            result: Err("database unavailable".to_owned()),
        };
        let retained = Cell::new(false);
        let cleaned = Cell::new(false);

        let result = persist_before_cleanup(
            &mut port,
            "m1",
            "## Summary",
            (),
            |_| retained.set(true),
            |_| cleaned.set(true),
        );

        assert_eq!(result, Err("database unavailable".to_owned()));
        assert!(retained.get());
        assert!(!cleaned.get());
    }

    #[test]
    fn persistence_success_cleans_workspace_without_retry() {
        let mut port = FakeSummaryPersistence { result: Ok(()) };
        let retained = Cell::new(false);
        let cleaned = Cell::new(false);

        let result = persist_before_cleanup(
            &mut port,
            "m1",
            "## Summary",
            (),
            |_| retained.set(true),
            |_| cleaned.set(true),
        );

        assert_eq!(result, Ok(()));
        assert!(!retained.get());
        assert!(cleaned.get());
    }
}
