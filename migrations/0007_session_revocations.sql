CREATE TABLE IF NOT EXISTS session_revocations (
  user_id TEXT NOT NULL,
  issued_at BIGINT NOT NULL,
  revoked_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
  PRIMARY KEY (user_id, issued_at)
);
