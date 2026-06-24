use discord_transcript::infrastructure::sql_store::{
    FakeSqlExecutor, SqlExecutor, SqlRow, sql_row_from_strings,
};

const PARAM_SEPARATOR: &str = "\u{1f}";

fn exact_key(sql: &str, params: &[&str]) -> String {
    format!("{sql}|{}", params.join(PARAM_SEPARATOR))
}

#[test]
fn strict_fake_execute_rejects_unregistered_sql() {
    let mut executor = FakeSqlExecutor::strict();

    let err = executor
        .execute(
            "UPDATE meetings SET status=$1 WHERE id=$2",
            &["posted".to_owned(), "m1".to_owned()],
        )
        .expect_err("strict fake should reject unregistered execute");

    assert!(err.contains("unregistered fake execute"));
    assert!(err.contains("UPDATE meetings SET status=$1 WHERE id=$2"));
    assert_eq!(executor.executed.len(), 1);
}

#[test]
fn strict_fake_query_rows_rejects_unregistered_sql() {
    let mut executor = FakeSqlExecutor::strict();

    let err = executor
        .query_rows(
            "SELECT status FROM meetings WHERE id=$1",
            &["m1".to_owned()],
        )
        .expect_err("strict fake should reject unregistered query");

    assert!(err.contains("unregistered fake query_rows"));
    assert!(err.contains("SELECT status FROM meetings WHERE id=$1"));
    assert_eq!(executor.executed.len(), 1);
}

#[test]
fn strict_fake_run_migration_rejects_unregistered_sql() {
    let mut executor = FakeSqlExecutor::strict();

    let err = executor
        .run_migration("ALTER TABLE meetings ADD COLUMN smoke TEXT")
        .expect_err("strict fake should reject unregistered migration");

    assert!(err.contains("unregistered fake run_migration"));
    assert!(err.contains("ALTER TABLE meetings ADD COLUMN smoke TEXT"));
    assert_eq!(executor.executed.len(), 1);
}

#[test]
fn strict_fake_execute_uses_registered_exact_and_wildcard_results() {
    let mut executor = FakeSqlExecutor::strict();
    executor.execute_result.insert(
        exact_key("UPDATE jobs SET status=$1 WHERE id=$2", &["done", "j1"]),
        2,
    );
    executor
        .execute_result
        .insert("INSERT INTO jobs(id, status) VALUES($1, $2)|*".to_owned(), 1);

    let exact = executor
        .execute(
            "UPDATE jobs SET status=$1 WHERE id=$2",
            &["done".to_owned(), "j1".to_owned()],
        )
        .expect("exact execute registration should be used");
    let wildcard = executor
        .execute(
            "INSERT INTO jobs(id, status) VALUES($1, $2)",
            &["generated-id".to_owned(), "queued".to_owned()],
        )
        .expect("wildcard execute registration should be used");

    assert_eq!(exact, 2);
    assert_eq!(wildcard, 1);
}

#[test]
fn strict_fake_query_rows_uses_registered_exact_and_wildcard_results() {
    let mut executor = FakeSqlExecutor::strict();
    executor.query_rows_result.insert(
        exact_key("SELECT status FROM meetings WHERE id=$1", &["m1"]),
        vec![sql_row_from_strings(vec!["recording".to_owned()])],
    );
    executor.query_rows_result.insert(
        "SELECT id FROM jobs WHERE status=$1 ORDER BY created_at LIMIT 1|*".to_owned(),
        vec![sql_row_from_strings(vec!["j1".to_owned()])],
    );

    let exact = executor
        .query_rows(
            "SELECT status FROM meetings WHERE id=$1",
            &["m1".to_owned()],
        )
        .expect("exact query registration should be used");
    let wildcard = executor
        .query_rows(
            "SELECT id FROM jobs WHERE status=$1 ORDER BY created_at LIMIT 1",
            &["queued".to_owned()],
        )
        .expect("wildcard query registration should be used");

    assert_eq!(exact, vec![sql_row_from_strings(vec!["recording".to_owned()])]);
    assert_eq!(wildcard, vec![sql_row_from_strings(vec!["j1".to_owned()])]);
}

#[test]
fn strict_fake_run_migration_uses_registered_success() {
    let mut executor = FakeSqlExecutor::strict();
    executor
        .run_migration_success
        .insert("CREATE TABLE smoke(id TEXT PRIMARY KEY)".to_owned());

    executor
        .run_migration("CREATE TABLE smoke(id TEXT PRIMARY KEY)")
        .expect("registered migration should succeed");
}

#[test]
fn default_fake_remains_permissive_for_legacy_tests() {
    let mut executor = FakeSqlExecutor::default();

    let affected = executor
        .execute("DELETE FROM meetings WHERE id=$1", &["m1".to_owned()])
        .expect("default fake execute should remain permissive");
    let rows = executor
        .query_rows("SELECT id FROM meetings", &[])
        .expect("default fake query should remain permissive");
    executor
        .run_migration("ALTER TABLE meetings ADD COLUMN smoke TEXT")
        .expect("default fake migration should remain permissive");

    assert_eq!(affected, 1);
    assert_eq!(rows, Vec::<SqlRow>::new());
}
