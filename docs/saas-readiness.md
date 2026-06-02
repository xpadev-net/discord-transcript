# SaaS Readiness Contracts

Status: initial contract for Tasks 46-68. This document is implementation-guiding, not a migration spec.

## Goals

- Establish the initial tenant boundary for SaaS work without redesigning the current single-guild runtime.
- Make future organization and multi-guild support additive.
- Define settings precedence and the immutable meeting-time settings snapshot.
- Define quota and usage units clearly enough for storage, metering, entitlement checks, and UI/API contracts.

## Tenant Boundary

Initial SaaS tenant:

- A tenant is the billable, isolated customer boundary.
- For the first SaaS phase, a tenant maps one-to-one to a Discord guild.
- In the initial phase, create `tenant_id` by copying the Discord `guild_id` value into the tenant record. Future identifier changes must preserve a stable mapping through `tenant_guilds(tenant_id, guild_id)` rather than rewriting historical SaaS rows.
- All SaaS-owned data must be tenant-scoped. A query that lists or mutates meetings, settings, usage, plan assignments, artifacts, or debug downloads must include `tenant_id` directly or resolve it explicitly from `tenant_guilds.guild_id` to `tenant_guilds.tenant_id`.
- Existing `meetings.guild_id` remains the source for legacy meeting ownership until tenant columns/tables are introduced.

Future organization relationship:

- An organization is an account container that can own one or more tenants.
- A tenant remains the isolation and entitlement boundary even when organizations are added.
- A Discord guild belongs to exactly one active tenant at a time.
- Future relationship shape:
  - `organizations.id` owns account-level metadata and members.
  - `tenants.id` belongs to one organization when organizations exist.
  - `tenant_guilds` links `tenant_id` to Discord `guild_id` and records active/revoked membership over time.
- Cross-guild organization dashboards may aggregate usage, but write operations still target a tenant or tenant-guild binding explicitly.

## Settings Precedence

Effective runtime settings are resolved in this order:

1. Environment default
2. Tenant default
3. Guild override
4. Meeting snapshot

Rules:

- Environment defaults are the process-level fallback values already represented by configuration and `GuildSettingsDefaults`.
- Tenant defaults apply to every guild under a tenant unless a guild override is set.
- Guild overrides are nullable per setting. A null override means "inherit from tenant default"; if tenant default is absent, inherit from environment default.
- Existing `guild_settings` rows map to the guild override layer.
- A meeting snapshot is captured when recording starts and is immutable for that meeting. Recording, ASR, summary, retention, and debug availability decisions for that meeting use the snapshot, not later settings edits.
- A meeting snapshot must not contain secrets. For credentials, store only a non-secret source marker such as `global_bot_token`, `guild_bot_token`, or future `tenant_bot_token`.
- Tenant default rows and guild override rows must carry `settings_version`: a required integer that increments on every settings change. Snapshot `source_versions.tenant.version` and `source_versions.guild.version` refer to those required values; a row without `settings_version` is invalid for snapshot resolution.

Initial effective settings fields:

- `whisper_language`
- `whisper_language_explicit`
- `whisper_vad`
- `whisper_beam_size`
- `whisper_suppress_non_speech`
- `whisper_prompt`
- `whisper_temperature`
- `whisper_resample_to_16k`
- `auto_stop_grace_seconds`
- `retention_raw_audio_ttl_days`
- `retention_transcript_ttl_days`
- `retention_summary_ttl_days`
- `summary_enabled`
- `bot_token_source`

Fields that are not yet tenant- or guild-editable still belong in the snapshot when they affect recording, ASR, summary, retention, or artifact availability. Future settings may be added with the same precedence. New fields should define whether they are snapshotted at meeting start or read live.

`bot_token_source` allowed values are:

- `global_bot_token`
- `guild_bot_token`
- `tenant_bot_token` (reserved until tenant-level bot tokens are implemented)

`tenant_bot_token` must not be emitted until tenant-level bot tokens are implemented. If encountered before support exists, token resolution fails closed rather than falling back to another token source.

## Usage And Entitlement Units

Usage must be recorded as append-only events for period units and as current-state measurements for storage. Entitlement checks should be done before starting work when the needed amount is knowable, and usage should still be recorded for work that started and consumed external resources.

Usage timing differs by unit:

- Pre-flight units with known amounts should use a check-then-record or check-then-reserve pattern before the resource is consumed.
- `recording_minutes` is post-hoc because final duration is unknown at meeting start. A hard quota check at meeting start blocks when current period `recording_minutes >= limit`; a single active meeting may overrun the remaining balance and is recorded after the duration is known.
- If later product requirements need strict no-overrun recording limits, that task must add a maximum session duration or reservation model before changing the enforcement semantics.

### Units

`recording_minutes`

- Measures wall-clock meeting recording time.
- `recording_duration_seconds` is the elapsed wall-clock time the meeting spent in the `recording` state; it is zero when the meeting never transitioned to `recording`.
- Per meeting value is `max(1, ceil(recording_duration_seconds / 60))` if the meeting status ever transitioned to `recording`; otherwise `0`.
- A meeting with zero recording duration records zero minutes if the meeting status never transitions to `recording`; otherwise it records at least one minute.
- Period usage increments when a meeting reaches a terminal processed or failed state with a known recording duration.
- Hard entitlement enforcement at meeting start only checks whether current period usage is already at or above the limit. It does not guarantee the meeting will fit inside the remaining balance.
- Soft entitlement enforcement for `recording_minutes` does not emit the quota violation event at meeting start because the consumed amount is unknown. Record usage at the terminal state; if the resulting period usage exceeds the finite soft limit, record one quota violation event with `observed_value` equal to the period usage after adding the meeting, `source_type = meeting`, `source_id` equal to the meeting id, and `plan_assignment_id` equal to the assignment active when recording started.

`storage_bytes`

- Measures retained bytes currently owned by the tenant.
- This is a current gauge, not a period counter.
- Include raw audio, mixdown audio, transcripts, summaries, manifests, prompts, and generated debug artifacts while retained.
- Decrement when data is deleted by TTL cleanup or explicit deletion.

`asr_seconds`

- Measures audio seconds submitted to ASR.
- Count the actual audio duration sent to the ASR engine. For per-speaker chunk ASR, sum submitted chunk durations; for mixdown ASR, use the mixdown duration.
- When the planned per-speaker chunk durations are known before submission, entitlement pre-flight must check or reserve the aggregate planned ASR seconds for the meeting before enqueueing any chunk. Do not rely on independent per-chunk checks that can pass concurrently and exceed the tenant quota in aggregate.
- When per-speaker chunk durations are not knowable before submission, serialize chunk authorization under a meeting-scoped ASR accounting lock. Each chunk must check or reserve against the tenant's current period usage plus any already reserved/submitted chunks for that meeting; the prohibition above applies to independent concurrent per-chunk checks, not to this serialized unknowable-duration fallback.
- For finite hard `asr_seconds` quotas, pre-flight must reserve or atomically increment-and-compare against a tenant-scoped period counter that is shared across concurrent meetings before any ASR submission is queued. A bare read-then-enqueue check is invalid for hard ASR enforcement.
- Retries count again only after a retry submits audio to ASR.
- Period usage increments when the ASR request is started or durably queued with a known input duration.
- Idempotency keys for `asr_seconds` must be per ASR attempt, not per meeting or job, so retries are counted exactly once each.
- `asr_attempt_id` is a server-assigned opaque id created before durably queueing or submitting each ASR attempt. It is scoped to one `(tenant_id, meeting_id, job_type, submission_sequence, attempt_number)` where `submission_sequence` is a monotonically increasing integer assigned per distinct audio submission to ASR within the meeting, for example one per speaker chunk or `1` for mixdown ASR. It is stored with the usage event or job-attempt metadata before the external ASR request starts. Redelivery of the same queued attempt reuses the same `asr_attempt_id`; a retry that submits audio creates a new `asr_attempt_id` with the same `submission_sequence` and an incremented `attempt_number`.

`summary_runs`

- Measures LLM summary invocations.
- Count each invocation that starts sending a prompt to the summary harness, including retries and regeneration requests.
- Do not count validation failures or jobs that fail before an LLM invocation begins.
- Period usage increments at invocation start.
- Idempotency keys for `summary_runs` must be per invocation attempt, not per meeting, so retries are counted exactly once each.
- `summary_invocation_id` is a server-assigned opaque id created before sending each summary prompt. It is scoped to one `(tenant_id, meeting_id, job_type, invocation_number)` where `invocation_number` is a monotonically increasing counter across all prompt sends for that meeting and job type, including retries and regenerations. It is stored with the usage event or job-attempt metadata before the LLM request starts. Redelivery of the same queued invocation reuses the same `summary_invocation_id`; a retry or regeneration that sends a prompt creates a new `summary_invocation_id` with an incremented `invocation_number`.

`debug_downloads`

- Measures authorized debug artifact downloads.
- Count each successful authorized file response start, including prompt, transcript, summary, whisper debug, raw audio, speaker audio, and mixdown debug artifacts.
- The unit is per artifact download intent, not per HTTP response. Count only the first successful 200 or 206 response for the same `(tenant_id, meeting_id, artifact_id, download_session_id)`; later full or range responses in that session do not increment usage.
- `download_session_id` is a server-assigned opaque id created when a user is authorized for a debug artifact download. It is scoped to one `(tenant_id, meeting_id, artifact_id, user_id)` logical access, is embedded in the generated download URL or token, and expires no later than the download authorization.
- Download session status is `active`, `expired`, or `revoked`; an authorization whose expiry time has passed must not remain `active`.
- The intended active-session uniqueness constraint is `(tenant_id, meeting_id, artifact_id, user_id) WHERE status = 'active'`. Repeated authorization for the same tuple should reuse the active, unexpired `download_session_id` or atomically mark any expired session as `expired` or any replaced session as `revoked` before creating a replacement; multiple concurrent active sessions for the same tuple are not valid in the initial contract.
- Authorization must use an atomic upsert or a serialized lock on `(tenant_id, meeting_id, artifact_id, user_id)`. If concurrent insertion loses to the active-session uniqueness constraint, retry the active-session lookup in the same request and return the winning active `download_session_id`; do not surface the conflict as a user-visible failure.
- Do not count denied, missing, or validation-failed requests.
- Period usage increments when the response begins streaming or returns an inline artifact.
- Idempotency keys for `debug_downloads` must be per `download_session_id`, not per meeting or artifact, so each authorized logical download intent is counted at most once.

### Period And Gauge Semantics

- Period counter usage keys are scoped by `tenant_id`, `unit`, `period_start`, and `period_end`.
- `period_start` is inclusive and `period_end` is exclusive.
- The initial billing period is monthly in UTC and is computed from the authoritative tenant-scoped `period_anchor`. In the initial guild-assignment model, `guild_plan_assignments.period_anchor` stores a copy of that tenant anchor and every active guild assignment for the same tenant must use the same value.
- After initialization, `period_anchor` is immutable in this initial contract. Billing-provider anchor corrections require a future migration or re-bucketing task that explicitly defines how historical usage events are treated.
- A plan change starts a new entitlement evaluation window at `effective_at`; historical usage events remain attached to their original period.
- Usage events should keep `source_type` and `source_id` so meeting, job, artifact, and debug download usage can be reconciled.
- `storage_bytes` is not keyed by period. Store it as a current gauge keyed by `tenant_id` and `unit`, with `current_value`, `measured_at`, and optional `source_watermark`.
- `source_watermark` is the latest tenant-scoped artifact inventory watermark included in the gauge.
- Current storage usage should be separately queryable by tenant and may be rebuilt from artifact/workspace inventories if a gauge update fails.
- Normal retained artifact inventory changes must update `storage_bytes.current_value`, `measured_at`, and `source_watermark` to the new `artifact_inventory_watermarks.current_sequence` in the same transaction as the artifact mutation whenever the byte delta is known. If the gauge cannot be updated atomically, mark it stale by setting `source_watermark = null` and enqueue or trigger a rebuild.
- Treat the `storage_bytes` gauge as stale when no gauge row exists, when `measured_at` is more than 5 minutes old, when `source_watermark` is null, or when `source_watermark` is behind `artifact_inventory_watermarks.current_sequence` for that tenant. New tenants must initialize the gauge synchronously with tenant provisioning, using `current_value = 0` and `source_watermark = 0`, before allowing storage-increasing operations.
- When a finite `storage_bytes` hard quota is enforced and the gauge is stale, fail closed for operations that increase storage and enqueue or trigger a gauge rebuild. Cleanup and delete operations may proceed because they reduce or preserve storage. A rebuild is complete when the rebuilt gauge's `source_watermark` is at or beyond the tenant artifact inventory watermark captured when the rebuild started.
- When a finite `storage_bytes` soft quota is enforced and the gauge is stale, allow the operation, enqueue or trigger a gauge rebuild, and record an observable stale-gauge quota event using the last known `current_value` when present or `0` when no row exists. For that event, use `source_type = stale_gauge`, set `source_id` to the storage-increasing operation id when available or the rebuild request id otherwise, and set `amount_over = max(0, observed_value - limit_value)`.

## Data Contracts

The contracts below describe the intended shape. Exact SQL names may change, but implementations should preserve these semantics.

### Plan

Purpose: catalog entry for what a tenant can be assigned.

Fields:

- `id`: stable internal identifier.
- `code`: stable external code used by configuration, admin UI, and tests.
- `name`: display name.
- `status`: `draft`, `active`, `archived`.
- `version`: integer version, incremented on each update, for optimistic locking and auditability.
- `created_at`, `updated_at`.

Rules:

- `code` is unique across all plan rows, including archived plans. The initial lifecycle has no deleted state.
- Only `active` plans may be newly assigned.
- `draft` plans may be edited and validated but cannot be assigned.
- `archived` plans cannot be newly assigned but remain valid for historical assignments.
- Existing active and scheduled assignments on an archived plan continue to be honored until they end normally or are replaced by an explicit plan change.
- Pricing fields are out of scope for this initial contract unless a later task explicitly adds billing integration.

### Plan Quota

Purpose: entitlement row for one plan and one usage unit.

Fields:

- `plan_id`
- `unit`: one of `recording_minutes`, `storage_bytes`, `asr_seconds`, `summary_runs`, `debug_downloads`.
- `limit_type`: `finite` or `unlimited`.
- `limit_value`: non-negative integer when `limit_type = finite`; null when `limit_type = unlimited`.
- `period`: `monthly` for period counters; `current` for `storage_bytes`.
- `enforcement`: `hard` or `soft`.
- `created_at`, `updated_at`.

Rules:

- Unlimited quota must be represented by `limit_type = unlimited` with `limit_value = null`; do not use sentinel values such as `-1`.
- `storage_bytes` uses `period = current`.
- `recording_minutes`, `asr_seconds`, `summary_runs`, and `debug_downloads` use `period = monthly` initially.
- Missing quota rows mean the unit is not entitled unless a later migration explicitly defines default quota inheritance.
- A migration that introduces a new unit must add quota rows for every affected existing plan in the same migration step, or explicitly accept that tenants on plans without that row are denied for the new unit.
- `hard` enforcement rejects the operation when the applicable finite quota is exhausted, subject to the `recording_minutes` post-hoc overrun rule above.
- `soft` enforcement allows the operation, records usage normally, and records an observable quota violation event for alerting/admin UI when the applicable finite quota is exceeded. It must not silently drop or skip usage.
- Soft quota violation events use the Quota Violation Event contract below.

### Period Counter Usage Event

Purpose: append-only metering record for period-counter units.

Fields:

- `tenant_id`
- `guild_id`
- `plan_assignment_id`
- `unit`: one of `recording_minutes`, `asr_seconds`, `summary_runs`, or `debug_downloads`.
- `period_start`
- `period_end`
- `amount`: non-negative integer amount for the unit.
- `source_type`
- `source_id`
- `idempotency_key`
- `created_at`

Rules:

- The intended uniqueness constraint is `(tenant_id, unit, period_start, period_end, idempotency_key)`.
- `period_start` and `period_end` are required and use the Period And Gauge Semantics boundaries.
- `plan_assignment_id` is the assignment active when work was authorized or started for post-hoc units, and the assignment active at usage start for immediate units.
- `source_type` and `source_id` identify the meeting, ASR attempt, summary invocation, or download session that produced the usage.
- Retrying the same durable attempt uses the same `idempotency_key`; a retry that intentionally counts as new usage must use a new idempotency key as defined by the unit contract.

### Period Counter Reservation

Purpose: mutable tenant-period counter used for hard pre-flight reservations that cannot rely on read-then-insert usage aggregation.

Fields:

- `tenant_id`
- `unit`: initially `asr_seconds`.
- `period_start`
- `period_end`
- `reserved_value`: non-negative integer amount reserved but not yet converted into usage events.
- `updated_at`

Rules:

- The intended uniqueness constraint is `(tenant_id, unit, period_start, period_end)`.
- Hard `asr_seconds` pre-flight must lock or atomically upsert this row, compare `current_usage + reserved_value + requested_amount` against the finite quota, and increment `reserved_value` only if the result fits.
- `current_usage` is the committed sum of Period Counter Usage Event `amount` for the same tenant, unit, and period.
- When an ASR attempt records its usage event or is cancelled before submitting audio, release the matching reservation in the same transaction or an equivalent idempotent reconciliation step.

### Storage Bytes Gauge

Purpose: current retained-byte measurement used by `storage_bytes` entitlement checks.

Fields:

- `tenant_id`
- `unit`: always `storage_bytes`.
- `current_value`: non-negative byte count.
- `measured_at`
- `source_watermark`: nullable tenant artifact inventory watermark included in this measurement.
- `created_at`, `updated_at`.

Rules:

- The intended uniqueness constraint is `(tenant_id, unit)`.
- New tenants must create the initial row synchronously with tenant provisioning using `current_value = 0`, `source_watermark = 0`, and `unit = storage_bytes`.
- Staleness, rebuild, and hard/soft enforcement behavior are defined in Period And Gauge Semantics.

### Quota Violation Event

Purpose: observable record for soft quota overages used by alerting and admin UI.

Fields:

- `tenant_id`
- `guild_id`: nullable.
- `plan_assignment_id`
- `unit`
- `limit_value`
- `observed_value`
- `amount_over`
- `period_start`: nullable.
- `period_end`: nullable.
- `source_type`
- `source_id`
- `observed_at`
- `created_at`

Rules:

- `amount_over = max(0, observed_value - limit_value)`.
- `period_start` and `period_end` are required for period-counter units and null only for current-gauge units such as `storage_bytes`.
- In the initial one-guild-per-tenant phase, `guild_id` is required for every quota violation event and is resolved from the active tenant-guild binding at `observed_at`, including tenant-level `storage_bytes` gauge events. `guild_id` may be null only for a future tenant-level or organization-level event that does not correspond to one active guild.
- For post-hoc units such as `recording_minutes`, `plan_assignment_id` is the assignment active when the work was authorized or started, not the assignment active at `observed_at`. The meeting or usage-start metadata must retain that assignment id so terminal-state accounting does not misattribute usage after a plan change.
- Keep events for at least 13 monthly periods.

### Download Session

Purpose: authorize one logical debug artifact download intent and provide the idempotency key for `debug_downloads`.

Fields:

- `download_session_id`: server-assigned opaque id.
- `tenant_id`
- `meeting_id`
- `artifact_id`
- `user_id`
- `status`: `active`, `expired`, or `revoked`.
- `expires_at`
- `revoked_at`: nullable.
- `created_at`, `updated_at`.

Rules:

- The intended active-row uniqueness constraint is `(tenant_id, meeting_id, artifact_id, user_id) WHERE status = 'active'`.
- `expires_at` is the database source of truth for whether an active session is still reusable. The authorization handler must expire any active row whose `expires_at <= now` inside the same atomic upsert or lock used to create or reuse sessions; a background expiry job is optional but not sufficient by itself.
- The download handler must also validate `expires_at > now` while authorizing the file response. If the session is expired, it must reject the download and atomically mark the row `expired` before any bytes stream or usage is counted.
- Revoking or replacing a session sets `status = revoked` and `revoked_at`.
- Usage idempotency uses `download_session_id`, as defined in the `debug_downloads` unit.

### Artifact Inventory Watermark

Purpose: tenant-scoped storage inventory version used to decide whether the `storage_bytes` gauge is fresh.

Fields:

- `tenant_id`
- `current_sequence`: monotonically increasing integer, initialized to `0` for a new tenant.
- `updated_at`

Rules:

- `current_sequence` is tenant-scoped, not global. The intended uniqueness constraint is `tenant_id` with one row per tenant; concurrent inserts must be prevented by this constraint or by the tenant-provisioning transaction.
- New tenants must create the initial artifact inventory watermark row synchronously with tenant provisioning using `current_sequence = 0`.
- Every retained artifact inventory row that contributes to `storage_bytes` carries the tenant's latest `artifact_mutation_sequence` at the time that row last changed.
- A retained artifact inventory change is any create, byte-size change, tenant attribution change, retention-state change, TTL cleanup, explicit deletion, or soft-delete/tombstone transition that changes whether bytes are counted for a tenant.
- Each retained artifact inventory change increments `artifact_inventory_watermarks.current_sequence` in the same transaction and assigns that value as the affected row's `artifact_mutation_sequence` when a counted row remains. If the change removes the row from retained inventory, the tenant watermark still advances even though no retained row carries that new sequence afterward.
- A storage gauge rebuild captures the tenant `current_sequence` at rebuild start and writes that value to `storage_bytes.source_watermark` when the rebuilt byte total reflects all retained artifact inventory changes through that captured sequence.

### Processing Job Status For Boundary Checks

Purpose: authoritative record for tenant-close and guild-move checks that need to know whether ASR or summary work is still active.

Meeting boundary checks:

- Terminal meeting statuses are `posted`, `failed`, and `aborted`.
- Non-terminal meeting statuses are `scheduled`, `recording`, `stopping`, `transcribing`, and `summarizing`.
- Tenant close, guild move, and standalone binding revocation must use this terminal/non-terminal split when verifying that no non-terminal meetings remain.

Fields:

- `tenant_id`
- `guild_id`
- `meeting_id`
- `job_type`: `asr` or `summary`.
- `status`: `queued`, `running`, `succeeded`, `failed`, or `cancelled`.
- `created_at`, `updated_at`.

Rules:

- The database job-status row is the system of record for boundary checks; external queue visibility is a delivery signal and is not authoritative for tenant close or guild move decisions.
- The intended uniqueness constraint is `(tenant_id, meeting_id, job_type)`. Retries update the same logical job-status row; attempt-level usage accounting for `asr_seconds` and `summary_runs` is recorded separately in usage events.
- A job is in flight until its database status is one of `succeeded`, `failed`, or `cancelled`.
- For ASR jobs with multiple parallel `submission_sequence` values, such as per-speaker chunks, the aggregate `(tenant_id, meeting_id, asr)` job-status row remains non-terminal until every chunk-level submission has reached a terminal state. A single chunk completion must not set the aggregate row to `succeeded` while another chunk is still queued or running.
- Code that queues or starts ASR or summary work must create or update the database job-status row before enqueueing or executing the work.

### Tenant

Purpose: authoritative SaaS isolation and entitlement record.

Fields:

- `id`
- `status`: `active`, `suspended`, or `closed`.
- `period_anchor`: nullable tenant-scoped UTC timestamp used for period-counter quota windows; null only before the first guild plan assignment initializes it.
- `created_at`, `updated_at`.

Rules:

- `period_anchor` is initialized in the same transaction as the first guild plan assignment, from the billing provider subscription anchor when present, otherwise from the first assignment's `effective_at`. The stored value must be truncated to midnight UTC on the initializing value's UTC calendar day.
- After initialization, `period_anchor` is immutable in this initial contract. Billing-provider anchor corrections require a future migration or re-bucketing task that explicitly defines how historical usage events are treated.
- A `suspended` tenant keeps data ownership but fails a tenant-status pre-gate for all new resource-consuming operations, regardless of whether the active plan's quota enforcement is `hard` or `soft`. Read-only access, cleanup, and delete operations may proceed when otherwise authorized.
- Closing a tenant is a single transaction under a tenant-scoped lock or equivalent serializable isolation: first verify and lock that the tenant has no non-terminal meetings and no in-flight ASR or summary jobs according to the database job-status records, then set tenant status to `closed`, revoke all active tenant-guild bindings with `revoked_at` set to the close transaction timestamp, end all active guild plan assignments with `ended_at` set to the same close transaction timestamp, and cancel all scheduled guild plan assignments. If any precondition fails while the lock is held, the close attempt fails without partial changes.
- A `closed` tenant keeps historical data ownership for retention, audit, and read-only access, but cannot receive new guild bindings or plan assignments and must fail all new resource-consuming operations.

### Tenant Default Settings

Purpose: tenant-level settings layer between environment defaults and guild overrides.

Fields:

- `tenant_id`
- `settings_version`: required integer incremented on every settings change.
- One nullable column or structured value per initial effective settings field: `whisper_language`, `whisper_language_explicit`, `whisper_vad`, `whisper_beam_size`, `whisper_suppress_non_speech`, `whisper_prompt`, `whisper_temperature`, `whisper_resample_to_16k`, `auto_stop_grace_seconds`, `retention_raw_audio_ttl_days`, `retention_transcript_ttl_days`, `retention_summary_ttl_days`, `summary_enabled`, and `bot_token_source`.
- `created_at`, `updated_at`.

Rules:

- The intended uniqueness constraint is `tenant_id` with one default-settings row per tenant.
- Null setting values inherit from the environment default layer.
- The row must not store secrets. For credentials, store only non-secret source markers such as `global_bot_token`, `guild_bot_token`, or future `tenant_bot_token`.
- Snapshot `source_versions.tenant.version` reads `settings_version`; a tenant-default settings row without `settings_version` is invalid for snapshot resolution.

### Tenant Guild Binding

Purpose: resolve Discord guild ownership to the active SaaS tenant.

Fields:

- `tenant_id`
- `guild_id`
- `status`: `active` or `revoked`.
- `effective_at`
- `revoked_at`
- `assigned_by_user_id`: nullable Discord user id with a conditional requirement. `source = admin` requires a non-null user id via application validation and a database check constraint; `system`, `billing_provider`, and `migration` use null unless a real initiating user is known.
- `source`: `system`, `admin`, `billing_provider`, or `migration`.
- `created_at`, `updated_at`.

Rules:

- The intended active-row uniqueness constraint is `guild_id WHERE status = 'active'`.
- A `(tenant_id, guild_id)` pair may have only one active row.
- Revoked rows remain for history and audit but must not be used for SaaS query scoping.
- Moving a guild to another tenant is a single transaction under locks on the guild and both old and new tenants, or under equivalent serializable isolation: verify the guild has no non-terminal meetings and no in-flight ASR or summary jobs according to the database job-status records, revoke the current active tenant-guild row with `revoked_at` set to the move transaction timestamp, end the old `(tenant_id, guild_id)` active plan assignment with `ended_at` set to the same move transaction timestamp and cancel any scheduled plan assignment, insert or activate the new tenant binding, and insert a new active plan assignment for the new tenant and guild with `period_anchor` matching the authoritative tenant `period_anchor` after any first-assignment initialization. The new assignment `plan_id` must be explicit from the move source: admin moves require a caller-supplied active plan, billing-provider moves use the provider-supplied plan mapping, and system or migration moves use the explicit system/migration plan. The old tenant's plan is never inherited implicitly. The move must invalidate or advance storage gauge watermarks for both the old and new tenants in the same transaction, either by generating tenant-scoped artifact mutation sequences for both de-attribution and attribution or by marking both gauges stale in the transaction, for example by setting `storage_bytes.source_watermark = null` for both tenants. If any precondition fails while the locks are held or the new assignment cannot be created in the same transaction, the move fails.
- If the receiving tenant's `period_anchor` is null during a guild move, the move transaction must initialize it using the same first-assignment rule as guild plan assignment creation: billing-provider subscription anchor when present, otherwise the new assignment's `effective_at`, truncated to midnight UTC on the initializing value's UTC calendar day. The new active assignment then copies that initialized tenant anchor.
- Standalone binding revocation, such as bot removal or admin eviction without tenant close or guild move, is a single transaction under a tenant-scoped lock or equivalent serializable isolation: verify the guild has no non-terminal meetings and no in-flight ASR or summary jobs according to the database job-status records, revoke the active tenant-guild binding with `revoked_at` set to the revocation transaction timestamp, end the active guild plan assignment with `ended_at` set to the same revocation transaction timestamp, cancel any scheduled assignment for that `(tenant_id, guild_id)`, and mark the tenant's storage gauge stale or advance the artifact inventory watermark in the same transaction. If any step cannot be completed, the revocation fails without partial changes.
- SaaS queries that start from `guild_id` must resolve through an active `tenant_guilds` row before reading or mutating tenant-owned data.

### Guild Plan Assignment

Purpose: assign a SaaS plan to a tenant-guild boundary.

Fields:

- `id`
- `tenant_id`
- `guild_id`
- `plan_id`
- `status`: `active`, `scheduled`, `ended`.
- `effective_at`
- `ended_at`
- `period_anchor`: tenant-scoped UTC timestamp used to compute monthly period boundaries. The tenant record is the authoritative source. When the tenant already has a `period_anchor`, every assignment copies that value regardless of any billing-provider anchor in the request. Only when the tenant has no `period_anchor`, initialize it from the billing provider subscription anchor when present, otherwise from `effective_at` for the tenant's first active assignment. Initializing the tenant `period_anchor` and writing the guild assignment must happen in one serializable transaction or under an equivalent tenant-scoped lock. After initialization, `period_anchor` is immutable in this contract; billing anchor corrections require a future explicit re-bucketing or migration task. Every active guild assignment for the same tenant must share the same `period_anchor`.
- `assigned_by_user_id`: nullable Discord user id with a conditional requirement. `source = admin` requires a non-null user id via application validation and a database check constraint; `system`, `billing_provider`, and `migration` use null unless a real initiating user is known.
- `source`: `system`, `admin`, `billing_provider`, or `migration`.
- `created_at`, `updated_at`.

Rules:

- Initially there is one active assignment per `(tenant_id, guild_id)`.
- The first plan assignment for a tenant-guild must be created with `status = active`; scheduled assignments require an existing active assignment and an initialized tenant `period_anchor`.
- Every transaction that writes a guild plan assignment must assert that the row's `period_anchor` equals the authoritative tenant `period_anchor`, using a trigger, deferred constraint, or explicit locked CTE assertion in that transaction. A mismatch fails the write.
- The intended active-row uniqueness constraint is `(tenant_id, guild_id) WHERE status = 'active'`.
- The intended scheduled-row uniqueness constraint is `(tenant_id, guild_id) WHERE status = 'scheduled'`.
- Current guild ownership is resolved separately through the active tenant-guild binding, which must allow at most one active tenant for a Discord `guild_id`.
- Any billing or admin request for a scheduled change upserts the single scheduled row for `(tenant_id, guild_id)`. Exact duplicates are idempotent; corrections with a different `plan_id`, `effective_at`, or both replace the existing scheduled row. Scheduled changes must use the authoritative tenant `period_anchor`; they must not introduce a different period anchor, must fail if there is no current active assignment, and must use an `effective_at` later than the scheduling transaction timestamp.
- Activating a scheduled assignment is a single transaction: verify the scheduled row's `period_anchor` already matches the authoritative tenant `period_anchor`, set the current active row to `ended`, then set the scheduled row to `active`. If activation runs on time, use `ended_at = scheduled.effective_at`. If activation is delayed past `scheduled.effective_at`, the change is not retroactive: set `ended_at` and the activated row's `effective_at` to the activation transaction timestamp and record an observable delayed-activation event. If the anchor does not match, activation fails rather than rewriting the scheduled row.
- Cancelling a scheduled assignment means setting that scheduled row to `status = ended` with `ended_at` equal to the cancellation transaction timestamp. Do not delete the row and do not introduce a separate `cancelled` status in this initial contract.
- Direct cancellation or provider termination sets the active row to `ended` with the provider/admin termination time, and cancels any scheduled row for the same `(tenant_id, guild_id)` in the same transaction.
- Monthly periods for period-counter units are derived from the original `period_anchor` day in UTC; period boundaries fall at midnight UTC (`00:00:00 UTC`) on that calendar day. If the anchor day does not exist in a later month, use that month's last day for that boundary only; subsequent boundaries still derive from the original anchor day. `period_anchor` must always be stored as midnight UTC (`00:00:00 UTC`) on the anchor calendar day; when the initializing value, such as the billing provider anchor or `effective_at`, has a non-zero time component, truncate to midnight UTC before storing.
- Example: with `period_anchor = 2026-01-31 00:00:00 UTC`, monthly periods use boundaries Jan 31 -> Feb 28, Feb 28 -> Mar 31, Mar 31 -> Apr 30, and Apr 30 -> May 31.
- Future organization support may assign default plans at the organization level, but the effective guild assignment must still be resolvable without ambiguity.
- Plan changes do not rewrite past usage or meeting snapshots.

### Effective Settings Snapshot

Purpose: preserve the exact settings used for a meeting after later settings edits.

Fields:

- `meeting_id`
- `tenant_id`
- `guild_id`
- `plan_assignment_id`: guild plan assignment active when the snapshot is created; authoritative for post-hoc `recording_minutes` usage and quota violation attribution even if the guild changes plans before the meeting reaches a terminal state.
- `resolved_at`
- `precedence_version`: integer version of the settings resolution contract, initially `1`; increment when the precedence order or inheritance semantics change, or when non-env-default fields are added to or removed from the snapshot. Changes to which env-default fields are snapshotted increment `env.version` instead; do not increment `precedence_version` for that case alone.
- `source_versions`: metadata for the env, tenant, and guild layers used to resolve the snapshot, shaped as `{ "env": { "version": 1, "hash": "<lowercase-hex-sha256>" }, "tenant": { "id": "<tenant_id>", "version": 3 }, "guild": { "id": "<guild_id>", "version": 7 } }` where `version` fields are JSON integers and `id`/`hash` fields are JSON strings. `env.version` is the environment-settings schema version, initially `1`; increment it when snapshotted env-default fields are added, removed, or renamed, or when an existing snapshotted field's type or allowed values change. `env.hash` is calculated as:
  - Algorithm: lowercase hex SHA-256 over UTF-8 encoded bytes.
  - Input: JSON-serialized non-secret environment defaults that participate in the snapshot.
  - Initial `env.version = 1` field set: exactly `whisper_language`, `whisper_language_explicit`, `whisper_vad`, `whisper_beam_size`, `whisper_suppress_non_speech`, `whisper_prompt`, `whisper_temperature`, `whisper_resample_to_16k`, `auto_stop_grace_seconds`, `retention_raw_audio_ttl_days`, `retention_transcript_ttl_days`, `retention_summary_ttl_days`, `summary_enabled`, and `bot_token_source`. `bot_token_source` is included because it is a non-secret source marker. Credential values and any other environment defaults are excluded.
  - Initial `env.version = 1` field types: strings are `whisper_language`, `whisper_prompt`, and `bot_token_source`; booleans are `whisper_language_explicit`, `whisper_vad`, `whisper_suppress_non_speech`, `whisper_resample_to_16k`, and `summary_enabled`; integers are `whisper_beam_size`, `auto_stop_grace_seconds`, `retention_raw_audio_ttl_days`, `retention_transcript_ttl_days`, and `retention_summary_ttl_days`; the only decimal field is `whisper_temperature`.
  - Initial `env.version = 1` optional fields are `whisper_language` and `whisper_prompt`. Their canonical absent representation is omission from the hash input object; null and empty environment strings for those fields are normalized to absent before hashing. Every other initial field is required in the hash input.
  - JSON serialization: keys sorted lexicographically at every object level, recursively; absent optional values and null optional values both omitted; booleans serialized as JSON `true` or `false` literals; integers serialized without a decimal point; decimal settings such as `whisper_temperature` must be captured from raw environment strings before typed parsing (for example `process.env`, `os.environ`, or the injected config map string value) and serialized from normalized exact decimal source strings, not binary floating-point renderings, in standard decimal notation with no trailing zeros; a decimal whose fractional part is zero is serialized as an integer. Raw decimal environment strings that are not already in standard decimal notation, including scientific notation such as `1.0e-2`, make snapshot hash generation fail. If the raw string is unavailable, snapshot hash generation fails. Every decimal setting that participates in the snapshot must therefore be supplied as an environment variable string in all deployment environments, including local development and CI; a typed code default is not sufficient for hash computation. Implementations must validate the presence and format of all participating decimal env strings at process startup and refuse to start if any participating decimal value is absent, is in scientific notation, has trailing zeros after a decimal point, or is otherwise not in standard decimal form. Integer-valued decimals must be configured without a decimal point, so `0` and `1` are valid but `0.0` and `1.0` are invalid. This makes the failure surface at deploy time rather than at recording start.
  The env layer is always present; `"env": null` is invalid. For new SaaS snapshots, the tenant layer is also always present because `tenant_id` is required on the snapshot; if the tenant has no tenant-default settings row, use `{ "id": "<tenant_id>", "version": null }`. A null tenant layer is reserved only for an explicit future pre-SaaS legacy backfill path and must not be emitted by normal snapshot creation. For new SaaS snapshots, the guild layer is also always present because `guild_id` is required on the snapshot; if the guild has no guild override row, use `{ "id": "<guild_id>", "version": null }`. A null guild layer is reserved only for an explicit future pre-SaaS legacy backfill path and must not be emitted by normal snapshot creation.
- `settings`: structured values for the effective settings fields listed above.

Rules:

- Create the snapshot in the same logical operation as meeting creation or before the recording can start.
- Processing jobs must read settings from the snapshot for that meeting.
- Stored snapshots remain valid for the lifetime of the meeting and retention period using their recorded `env.version` and `precedence_version`. Implementations must keep backward-compatible readers for every prior snapshot version that can still exist, or run an explicit migration that rewrites retained snapshots to a newer version.
- Admin settings APIs may show current effective settings, but meeting detail APIs should expose snapshot-derived settings only if a user-facing need exists.
- If snapshot creation fails, recording start fails rather than falling back to live settings.

## Implementation Guidance

- Add tenant and quota schema in small, additive migrations.
- Keep current `guild_settings` behavior compatible by treating existing rows as guild overrides. A migration must backfill `settings_version` on all existing `guild_settings` rows before enabling snapshot resolution for those guilds; until backfilled, snapshot creation fails for any meeting in a guild that has a legacy override row without `settings_version`.
- Add every decimal snapshot environment variable, initially `WHISPER_TEMPERATURE`, to local development, CI, and deployment configuration before enabling startup validation for decimal env-string presence and format.
- Prefer explicit null inheritance for defaults and overrides.
- Keep usage accounting idempotent by using deterministic source identifiers, for example `meeting_id + unit` or `job_id + unit`.
- For units where retries count as new usage, such as `asr_seconds` and `summary_runs`, the deterministic source identifier must include the attempt or invocation id.
- Keep entitlement checks fail-closed for hard quotas and observable for soft quotas.
- Record audit timestamps for plan assignments and settings changes.
- Avoid backfilling historical tenant data beyond the current guild mapping unless a later task explicitly owns that migration.
