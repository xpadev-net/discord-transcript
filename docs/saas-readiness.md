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
- Per meeting value is `max(1, ceil(recording_duration_seconds / 60))` if the meeting status ever transitioned to `recording`; otherwise `ceil(recording_duration_seconds / 60)`.
- A meeting shorter than one second records zero minutes if the meeting status never transitions to `recording`; otherwise it records at least one minute.
- Period usage increments when a meeting reaches a terminal processed or failed state with a known recording duration.
- Hard entitlement enforcement at meeting start only checks whether current period usage is already at or above the limit. It does not guarantee the meeting will fit inside the remaining balance.

`storage_bytes`

- Measures retained bytes currently owned by the tenant.
- This is a current gauge, not a period counter.
- Include raw audio, mixdown audio, transcripts, summaries, manifests, prompts, and generated debug artifacts while retained.
- Decrement when data is deleted by TTL cleanup or explicit deletion.

`asr_seconds`

- Measures audio seconds submitted to ASR.
- Count the actual audio duration sent to the ASR engine. For per-speaker chunk ASR, sum submitted chunk durations; for mixdown ASR, use the mixdown duration.
- Retries count again only after a retry submits audio to ASR.
- Period usage increments when the ASR request is started or durably queued with a known input duration.

`summary_runs`

- Measures LLM summary invocations.
- Count each invocation that starts sending a prompt to the summary harness, including retries and regeneration requests.
- Do not count validation failures or jobs that fail before an LLM invocation begins.
- Period usage increments at invocation start.
- Idempotency keys for `summary_runs` must be per invocation attempt, not per meeting, so retries are counted exactly once each.

`debug_downloads`

- Measures authorized debug artifact downloads.
- Count each successful authorized file response start, including prompt, transcript, summary, whisper debug, raw audio, speaker audio, and mixdown debug artifacts.
- The unit is per artifact download intent, not per HTTP response. Count only the first successful 200 or 206 response for the same `(tenant_id, meeting_id, artifact_id, download_session_id)`; later full or range responses in that session do not increment usage.
- `download_session_id` is a server-assigned opaque id created when a user is authorized for a debug artifact download. It is scoped to one `(tenant_id, meeting_id, artifact_id, user_id)` logical access, is embedded in the generated download URL or token, and expires no later than the download authorization.
- Do not count denied, missing, or validation-failed requests.
- Period usage increments when the response begins streaming or returns an inline artifact.

### Period And Gauge Semantics

- Period counter usage keys are scoped by `tenant_id`, `unit`, `period_start`, and `period_end`.
- `period_start` is inclusive and `period_end` is exclusive.
- The initial billing period is monthly in UTC and is computed from the tenant-scoped `period_anchor`. In the initial guild-assignment model, `guild_plan_assignments.period_anchor` stores that tenant anchor and every active guild assignment for the same tenant must use the same value.
- A plan change starts a new entitlement evaluation window at `effective_at`; historical usage events remain attached to their original period.
- Usage events should keep `source_type` and `source_id` so meeting, job, artifact, and debug download usage can be reconciled.
- `storage_bytes` is not keyed by period. Store it as a current gauge keyed by `tenant_id` and `unit`, with `current_value`, `measured_at`, and optional `source_watermark`.
- `source_watermark` is the latest tenant-scoped `artifact_mutation_sequence` included in the gauge. `artifact_mutation_sequence` is a monotonically increasing integer assigned transactionally whenever retained artifact inventory changes.
- Current storage usage should be separately queryable by tenant and may be rebuilt from artifact/workspace inventories if a gauge update fails.
- Treat the `storage_bytes` gauge as stale for hard enforcement when no gauge row exists, when `measured_at` is more than 5 minutes old, when `source_watermark` is null, or when `source_watermark` is behind the latest known artifact mutation watermark. The latest known artifact mutation watermark is `MAX(artifact_mutation_sequence)` across retained artifact inventory rows for that tenant, or `0` when none exist. New tenants should initialize the gauge with `current_value = 0` and `source_watermark = 0` before allowing storage-increasing operations.
- When a finite `storage_bytes` hard quota is enforced and the gauge is stale, fail closed for operations that increase storage. Cleanup and delete operations may proceed because they reduce or preserve storage. A rebuild is complete when the rebuilt gauge's `source_watermark` is at or beyond the latest artifact mutation watermark captured from retained artifact inventory rows when the rebuild started.
- When a finite `storage_bytes` soft quota is enforced and the gauge is stale, allow the operation, enqueue or trigger a gauge rebuild, and record an observable stale-gauge quota event using the last known `current_value` when present or `0` when no row exists.

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
- `soft` enforcement allows the operation, records usage normally, and records an observable quota violation event for alerting/admin UI. It must not silently drop or skip usage.
- Soft quota violation events are recorded as `quota_violation_events` with `tenant_id`, optional `guild_id`, `plan_assignment_id`, `unit`, `limit_value`, `observed_value`, `amount_over`, optional `period_start`, optional `period_end`, `source_type`, `source_id`, `observed_at`, and `created_at`. `period_start` and `period_end` are required for period-counter units and null only for current-gauge units such as `storage_bytes`. Keep events for at least 13 monthly periods.

### Tenant Guild Binding

Purpose: resolve Discord guild ownership to the active SaaS tenant.

Fields:

- `tenant_id`
- `guild_id`
- `status`: `active` or `revoked`.
- `effective_at`
- `revoked_at`
- `source`: `system`, `admin`, `billing_provider`, or `migration`.
- `created_at`, `updated_at`.

Rules:

- The intended active-row uniqueness constraint is `guild_id WHERE status = 'active'`.
- A `(tenant_id, guild_id)` pair may have only one active row.
- Revoked rows remain for history and audit but must not be used for SaaS query scoping.
- Moving a guild to another tenant is a single transaction: revoke the current active tenant-guild row with `revoked_at`, end the old `(tenant_id, guild_id)` active plan assignment and any scheduled plan assignment, insert or activate the new tenant binding, and create or activate the new tenant's plan assignment for that guild. If the new assignment cannot be created in the same transaction, the move fails.
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
- `period_anchor`: tenant-scoped UTC timestamp used to compute monthly period boundaries. Set it from the billing provider subscription anchor when present; otherwise, inherit the existing tenant `period_anchor` when the tenant already has an active assignment; otherwise default to `effective_at` for the tenant's first assignment. Every active guild assignment for the same tenant must share the same `period_anchor`.
- `assigned_by_user_id`: nullable Discord user id with a conditional requirement. `source = admin` requires a non-null user id via application validation and a database check constraint; `system`, `billing_provider`, and `migration` use null unless a real initiating user is known.
- `source`: `system`, `admin`, `billing_provider`, or `migration`.
- `created_at`, `updated_at`.

Rules:

- Initially there is one active assignment per `(tenant_id, guild_id)`.
- The intended active-row uniqueness constraint is `(tenant_id, guild_id) WHERE status = 'active'`.
- The intended scheduled-row uniqueness constraint is `(tenant_id, guild_id) WHERE status = 'scheduled'`.
- Current guild ownership is resolved separately through the active tenant-guild binding, which must allow at most one active tenant for a Discord `guild_id`.
- Any billing or admin request for a scheduled change upserts the single scheduled row for `(tenant_id, guild_id)`. Exact duplicates are idempotent; corrections with a different `plan_id`, `effective_at`, `period_anchor`, or any combination of those fields replace the existing scheduled row.
- Activating a scheduled assignment is a single transaction: set the current active row to `ended` with `ended_at = scheduled.effective_at`, then set the scheduled row to `active`.
- Direct cancellation or provider termination sets the active row to `ended` with the provider/admin termination time.
- Monthly periods for period-counter units are derived from the original `period_anchor` day in UTC; period boundaries fall at midnight UTC (`00:00:00 UTC`) on that calendar day. If the anchor day does not exist in a later month, use that month's last day for that boundary only; subsequent boundaries still derive from the original anchor day.
- Example: with `period_anchor = 2026-01-31 00:00:00 UTC`, monthly periods use boundaries Jan 31 -> Feb 28, Feb 28 -> Mar 31, Mar 31 -> Apr 30, and Apr 30 -> May 31.
- Future organization support may assign default plans at the organization level, but the effective guild assignment must still be resolvable without ambiguity.
- Plan changes do not rewrite past usage or meeting snapshots.

### Effective Settings Snapshot

Purpose: preserve the exact settings used for a meeting after later settings edits.

Fields:

- `meeting_id`
- `tenant_id`
- `guild_id`
- `resolved_at`
- `precedence_version`: integer version of the settings resolution contract; increment when the precedence order or inheritance semantics change, or when non-env-default fields are added to or removed from the snapshot. Changes to which env-default fields are snapshotted increment `env.version` instead; do not increment `precedence_version` for that case alone.
- `source_versions`: metadata for the env, tenant, and guild layers used to resolve the snapshot, shaped as `{ "env": { "version": 1, "hash": "<lowercase-hex-sha256>" }, "tenant": { "id": "<tenant_id>", "version": 3 }, "guild": { "id": "<guild_id>", "version": 7 } }` where `version` fields are JSON integers and `id`/`hash` fields are JSON strings. `env.version` is the environment-settings schema version, initially `1`; increment it when snapshotted env-default fields are added, removed, or renamed, or when an existing snapshotted field's type or allowed values change. `env.hash` is calculated as:
  - Algorithm: lowercase hex SHA-256 over UTF-8 encoded bytes.
  - Input: JSON-serialized non-secret environment defaults that participate in the snapshot.
  - JSON serialization: keys sorted lexicographically at every object level, recursively; absent optional values and null optional values both omitted; booleans serialized as JSON `true` or `false` literals; integers serialized without a decimal point; decimals serialized in standard decimal notation with no trailing zeros; a decimal whose fractional part is zero is serialized as an integer.
  The env layer is always present; `"env": null` is invalid. An absent tenant or guild layer is represented by a null key value. If the tenant exists but has no tenant-default settings row, use `{ "id": "<tenant_id>", "version": null }` for the tenant layer. If the guild exists but has no guild override row, use `{ "id": "<guild_id>", "version": null }` for the guild layer.
- `settings`: structured values for the effective settings fields listed above.

Rules:

- Create the snapshot in the same logical operation as meeting creation or before the recording can start.
- Processing jobs must read settings from the snapshot for that meeting.
- Admin settings APIs may show current effective settings, but meeting detail APIs should expose snapshot-derived settings only if a user-facing need exists.
- If snapshot creation fails, recording start fails rather than falling back to live settings.

## Implementation Guidance

- Add tenant and quota schema in small, additive migrations.
- Keep current `guild_settings` behavior compatible by treating existing rows as guild overrides.
- Prefer explicit null inheritance for defaults and overrides.
- Keep usage accounting idempotent by using deterministic source identifiers, for example `meeting_id + unit` or `job_id + unit`.
- For units where retries count as new usage, such as `asr_seconds` and `summary_runs`, the deterministic source identifier must include the attempt or invocation id.
- Keep entitlement checks fail-closed for hard quotas and observable for soft quotas.
- Record audit timestamps for plan assignments and settings changes.
- Avoid backfilling historical tenant data beyond the current guild mapping unless a later task explicitly owns that migration.
