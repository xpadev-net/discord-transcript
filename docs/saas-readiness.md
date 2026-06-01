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
- The initial tenant identifier may be derived from `guild_id`, but new SaaS tables should use a dedicated `tenant_id` so a later organization model does not require rewriting historical rows.
- All SaaS-owned data must be tenant-scoped. A query that lists or mutates meetings, settings, usage, plan assignments, artifacts, or debug downloads must include `tenant_id` directly or derive it through a tenant-owned `guild_id`.
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

## Usage And Entitlement Units

Usage must be recorded as append-only events for period units and as current-state measurements for storage. Entitlement checks should be done before starting work when the needed amount is knowable, and usage should still be recorded for work that started and consumed external resources.

### Units

`recording_minutes`

- Measures wall-clock meeting recording time.
- Per meeting value is `ceil(recording_duration_seconds / 60)`.
- A meeting shorter than one second records zero minutes if it never reaches `recording`; otherwise it records at least one minute.
- Period usage increments when a meeting reaches a terminal processed or failed state with a known recording duration.

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

`debug_downloads`

- Measures authorized debug artifact downloads.
- Count each successful authorized file response start, including prompt, transcript, summary, whisper debug, raw audio, speaker audio, and mixdown debug artifacts.
- Do not count denied, missing, or validation-failed requests.
- Period usage increments when the response begins streaming or returns an inline artifact.

### Period Semantics

- Period usage keys are scoped by `tenant_id`, `unit`, `period_start`, and `period_end`.
- The initial billing period is monthly in UTC unless the plan assignment specifies otherwise.
- A plan change starts a new entitlement evaluation window at `effective_at`; historical usage events remain attached to their original period.
- Usage events should keep `source_type` and `source_id` so meeting, job, artifact, and debug download usage can be reconciled.
- Current storage usage should be separately queryable by tenant and may be rebuilt from artifact/workspace inventories if a gauge update fails.

## Data Contracts

The contracts below describe the intended shape. Exact SQL names may change, but implementations should preserve these semantics.

### Plan

Purpose: catalog entry for what a tenant can be assigned.

Fields:

- `id`: stable internal identifier.
- `code`: stable external code used by configuration, admin UI, and tests.
- `name`: display name.
- `status`: `draft`, `active`, `archived`.
- `version`: integer or timestamp version for auditability.
- `created_at`, `updated_at`.

Rules:

- `code` is unique among non-deleted plans.
- Archived plans cannot be newly assigned but remain valid for historical assignments.
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
- `period_anchor`
- `assigned_by_user_id`
- `source`: `system`, `admin`, `billing_provider`, or `migration`.
- `created_at`, `updated_at`.

Rules:

- Initially there is one active assignment per guild/tenant.
- Future organization support may assign default plans at the organization level, but the effective guild assignment must still be resolvable without ambiguity.
- Plan changes do not rewrite past usage or meeting snapshots.

### Effective Settings Snapshot

Purpose: preserve the exact settings used for a meeting after later settings edits.

Fields:

- `meeting_id`
- `tenant_id`
- `guild_id`
- `resolved_at`
- `precedence_version`
- `source_versions`: metadata for the env, tenant, and guild layers used to resolve the snapshot.
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
- Keep entitlement checks fail-closed for hard quotas and observable for soft quotas.
- Record audit timestamps for plan assignments and settings changes.
- Avoid backfilling historical tenant data beyond the current guild mapping unless a later task explicitly owns that migration.
