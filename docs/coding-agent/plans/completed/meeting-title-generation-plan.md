# Plan: Improve Meeting Title Generation

- status: done
- generated: 2026-10-15
- last_updated: 2026-10-15
- work_type: code

## Goal
- Generate a concise title that names the meeting's main subject instead of copying an arbitrary first summary sentence such as an attendee list.

## Definition of Done
- The default summary prompt explicitly requests a concise, topic-focused H1 as the first line of the generated summary.
- The first top-level heading supplies the meeting title and remains visible at the top of summary markdown.
- A valid newly generated H1 supersedes a previously stored generated title when the default summary structure is active; custom templates retain an existing title rather than treating a fixed template heading as a topic.
- Existing meeting title consumers continue to receive a plain string title.

## Planner-added requirements
- A valid generated H1 takes precedence over an existing stored title during summary regeneration. Needed because: the current code returns any existing title before inspecting newly generated markdown, which would preserve a title produced by the defective logic.
- Missing or invalid generated H1s and custom-template output preserve an existing valid title before falling back to the voice-channel name. Needed because: custom summary templates may omit the default prompt's H1 or use a fixed heading, and overwriting a usable title with a generic channel name or template label would lose information.
- Keep the stored title as the existing plain string consumed by meeting list/detail and Discord posting code. Needed because: these locatable consumers already depend on that representation and changing them is outside this task.

## Scope / Non-goals
- Scope: Default summary prompt, title extraction/normalization, and directly affected worker behavior.
- Non-goals: Editing one stored meeting title directly, changing summary sections/content policy, adding a new model call, or bulk reprocessing existing meetings.

## Design
- Chosen: Ask the existing summary generation call to begin with one concise `# <meeting topic>` heading, followed by the existing summary sections; extract a valid new H1 before considering an existing title, then use the channel fallback if neither is usable. Structure: one model call and the existing title owner; the same heading serves the summary and stored title. Evolution: one clear Markdown convention that fits the existing summary format. Verification: deterministic heading parsing can be reviewed and compile-checked without network access. Operation: no additional model latency or request cost. Human: the main subject is visible at the start of both the summary and Discord post. Safety: title text remains subject to current sanitization and existing untrusted-transcript instruction handling. Consumers: prompt output feeds the parser; the H1 stays in summary markdown, the title is stored for meeting detail/list UI, and Discord posting avoids duplicating a matching H1.
- Alternative: Ask the existing summary generation call for a separate `Meeting title:` metadata line and strip it before presenting the summary. Structure: requires carrying and removing a second representation of title text. Evolution: adds a parsing/cleanup convention. Verification: title extraction and body cleanup must stay synchronized. Operation: no additional model latency or request cost. Human: separates metadata from prose but hides the title from the summary's own heading hierarchy. Safety: same transcript trust boundary, with another output surface to validate.
- Why chosen: The user proposed putting the overview in the top-level title, and the existing post formatter already recognizes a matching leading H1 and avoids a duplicate heading; this fixes the heuristic without a second model call or persisted schema change.

## Compatibility stance
- surface: Internal summary-agent markdown output consumed by the worker and rendered in the meeting summary.
- stance: preserve
- justification: The parser and post formatter are locatable consumers in `src/application/worker.rs`; output remains markdown with the existing summary sections and one leading H1. That heading also supplies the stored plain string title consumed by meeting detail/list responses.

## Context (workspace)
- Related files/areas: `src/application/summary.rs`, `src/application/worker.rs`, title-related worker coverage in `tests/application/runtime_and_worker.rs`.
- Existing patterns or references: The prompt already marks metadata and transcript content untrusted; the worker already sanitizes and bounds titles and falls back to the voice-channel name.
- Research findings: The current prompt requests only summary sections. `derive_meeting_title` accepts the first non-built-in heading or first prose sentence after `## Summary`; therefore an attendee sentence can become the stored title. An existing stored title currently wins over a newly generated candidate.
- Rule context: `docs/coding-agent/rules/index.md` and the rules directory are absent. The repo-wide `docs/coding-agent/lessons.md` and `docs/plan.md` were inspected; neither defines title-generation policy.

## Open Questions (max 3)
- None.

## Assumptions
- A1: New meetings initially have no title — source: `src/infrastructure/sql_store.rs` meeting creation mapping; checked by Task_1.
- A2: The title convention should also work when a custom summary template is active, with the existing channel fallback if the template omits the H1 — source: `src/application/summary.rs` template instructions; checked by Task_1.
- A3: Existing meeting records are not automatically reprocessed or migrated — source: non-goal from the request's focus on generation logic.

## Tasks

### Task_1: Make the summary pipeline generate and consume a topic heading
- type: impl
- owns:
  - `src/application/summary.rs`
  - `src/application/worker.rs`
- depends_on: []
- description: |
  Update the default summary prompt to request a concise topic title as the first-level Markdown heading before the existing summary sections, explicitly excluding participant lists/counts and generic meeting metadata. Do not add that instruction when a custom template is materialized. Consume only a Markdown-valid leading H1 as the generated title, giving a valid new H1 precedence over a stored title; when it is missing or invalid, retain an existing title or use the channel fallback. Account for custom templates that may omit the H1 or contain fixed headings.
- acceptance:
  - Default prompt instructions clearly define the leading H1 as a concise overview of the meeting's main subject and disallow attendee-list titles; the instruction is absent when a custom template is materialized.
  - Only a Markdown-valid leading H1 with up to three spaces of indentation and a space or tab after the marker can supply a title from default generated markdown; the first summary prose sentence is never used as a title, and four-space indented code is rejected.
  - The leading H1 is visible at the top of persisted summary markdown and is not duplicated in the Discord post when it matches the stored title; custom-template leading H1s remain intact in posts without being promoted to stored titles.
  - A valid generated H1 supersedes an existing title when no custom template was materialized; custom-template output and missing or invalid H1s preserve an existing valid title or use the voice-channel fallback, while title sanitization and length limits remain in force.
- validation:
  - kind: command
    required: true
    owner: orchestrator
    detail: `rtk cargo check --workspace --all-targets --all-features`
  - kind: review
    required: true
    owner: reviewer
    detail: Inspect the prompt/extraction flow for attendee-title regression, title-line leakage into summary markdown, custom-template fallback, and existing title consumers.

## Task Waves (explicit parallel dispatch sets)

- Wave 1 (parallel): [Task_1]

## Rollback / Safety
- Revert the prompt and worker changes together; no database migration or external state change is part of this task.

## Progress Log (append-only)

- 2026-10-15 00:00 Wave 0 completed: [research]
  - Summary: Confirmed the title is inferred from the first summary heading/sentence and the prompt does not request a distinct title field.
  - Validation evidence: Read-only inspection of prompt, worker title derivation, and web meeting response mapping; no tests run.
  - Notes: Repository rule index is absent; no repository-local title policy was found.
- 2026-10-15 03:42 Wave 0 completed: [plan review and approval]
  - Summary: The user approved the reviewed plan, including the leading H1 design and generated-title precedence on regeneration.
  - Validation evidence: Balanced plan validator passed; plan reviewer APPROVED.
  - Notes: User requested that real meeting values not appear in Git/GitHub artifacts; implementation and validation use only generic inputs.
- 2026-10-15 03:44 Wave 1 implementation completed: [Task_1]
  - Summary: The Worker updated the default summary prompt and title extraction to use only a valid leading H1, preferring it over a stored title; missing or invalid H1s preserve the existing-title/channel fallback.
  - Validation evidence: `rtk cargo check --workspace --all-targets --all-features` passed. Worker report schema validation passed. No tests were added or run under the current instruction.
  - Notes: No real meeting values were used in repository changes or validation. No blockers; independent code review is the next step.
- 2026-10-15 03:45 Wave 1 review completed: [Task_1]
  - Summary: Reviewer APPROVED the title-generation change and identified a minor duplicate-heading edge when title sanitization normalizes the source H1.
  - Validation evidence: Independent review checked prompt, title derivation, fallback order, persisted title consumers, and Discord heading behavior.
  - Notes: The normalization edge was fixed in `markdown_with_title_for_post`; final delta re-review and compile check remain.
- 2026-10-15 03:46 Follow-up review found and fixed a built-in-heading edge.
  - Summary: Duplicate detection now normalizes any leading H1 independently of whether that heading is eligible as a generated title.
  - Validation evidence: Source diff review; compile check and delta re-review pending.
  - Notes: This preserves duplicate suppression for built-in headings while keeping them invalid as generated titles.
- 2026-10-15 03:47 Wave 1 completed: [Task_1]
  - Summary: The title prompt, generated-title selection, stored-title precedence, and Discord heading de-duplication meet the accepted criteria; Reviewer status is APPROVED.
  - Validation evidence: `rtk cargo check --workspace --all-targets --all-features` passed after the final code change; `rtk cargo fmt --all -- --check` passed; Worker report schema validation passed; plan validator passed. No tests were added or run under the current instruction.
  - Notes: No real meeting values appear in the source changes, plan, lessons, or harness candidate. Existing meeting records are not bulk reprocessed.
- 2026-10-15 04:00 PR review follow-up completed: [Task_1]
  - Summary: Updated the end-to-end fixture to use a leading topic H1; generated H1 titles are disabled for custom-template output so fixed template headings do not replace an existing meeting title. Added focused coverage for both behaviors and for ignoring fenced headings.
  - Validation evidence: `rtk cargo fmt --all -- --check`; `rtk cargo test --test runtime_and_worker worker_generates_persists_posts_and_debugs_title_from_summary`; `rtk cargo test --lib application::worker::tests::derive_meeting_title` all passed.
  - Notes: Codex review findings addressed; waiting for CI and a new review round. No real meeting values were used.
- 2026-10-15 04:02 PR review follow-up completed: [Task_1]
  - Summary: Leading-H1 parsing now preserves indentation so indented code headings are rejected; corrected the related harness candidate to limit its generalization to available evidence. Template eligibility now follows the template recorded in the materialized context manifest, including retries that reuse an earlier manifest.
  - Validation evidence: `rtk cargo test --lib application::worker::tests::derive_meeting_title` passed; the latest CI round remains in progress.
  - Notes: No real meeting values were used.
- 2026-10-15 04:05 PR review follow-up completed: [Task_1]
  - Summary: Title extraction now accepts Markdown-valid H1 indentation of up to three spaces and rejects four-space indented code. The topic-H1 prompt instruction is emitted only when the materialized context has no custom summary template.
  - Validation evidence: `rtk cargo fmt --all -- --check`; `rtk cargo test --lib application::worker::tests::derive_meeting_title` passed (4 tests); `rtk cargo test --test asr_summary` passed (54 tests). Latest full CI rerun remains in progress.
  - Notes: Codex review findings addressed in the working tree. No real meeting values were used.
- 2026-10-15 04:10 PR review follow-up completed: [Task_1]
  - Summary: Preserved custom-template leading H1s in Discord posts while keeping them ineligible as stored titles; accepted tab delimiters after ATX markers; preserved meaningful trailing `#` while removing Markdown closing hash sequences.
  - Validation evidence: `rtk cargo fmt --all -- --check`; 13 focused worker tests; 54 `asr_summary` tests; the targeted `runtime_and_worker` integration test; `rtk cargo check --workspace --all-targets --all-features`; and `rtk git diff --check` all passed.
  - Notes: Codex review findings addressed; next review round and CI pending. No real meeting values were used.
- 2026-10-15 04:25 PR review follow-up completed: [Task_1]
  - Summary: Separated syntactic H1 detection from title sanitization so long custom headings remain intact in posts; post formatting now preserves leading indentation while stripping only blank lines; empty closing-only headings are rejected as title candidates.
  - Validation evidence: `rtk cargo fmt --all -- --check`; 13 focused worker tests; the targeted `runtime_and_worker` integration test; `rtk cargo check --workspace --all-targets --all-features`; and `rtk git diff --check` all passed.
  - Notes: Codex review findings addressed in the working tree. No real meeting values were used.

## Decision Log (append-only; re-plans and major discoveries)

- 2026-10-15 00:00 Decision: Use an explicit title metadata line from the existing summary generation call.
  - Trigger / new insight: The title parser promotes arbitrary first summary prose to the title, and the prompt does not request a title.
  - Plan delta (what changed): Replace heuristic prose extraction with explicit title field parsing and remove that metadata line from the summary body.
  - Tradeoffs considered: Avoiding a second model call keeps latency/cost unchanged; a missing field falls back to the channel title.
  - User approval: no
  - Record proposed: none
- 2026-10-15 00:00 Decision: Use a leading H1 for the meeting overview.
  - Trigger / new insight: The user suggested putting the overview in the top-level title; the existing post formatter recognizes a matching leading H1.
  - Plan delta (what changed): Replace the metadata-line design with a leading H1 that remains in the summary and is also stored as the meeting title.
  - Tradeoffs considered: The web-rendered summary may show the H1 beneath its page title; Discord output avoids a duplicate matching heading and the summary itself gains an overview heading.
  - User approval: no
  - Record proposed: none
- 2026-10-15 00:00 Decision: Prefer a new valid H1 over an existing stored title during summary regeneration.
  - Trigger / new insight: Review found that current title precedence would preserve the bad generated title even after a summary regeneration.
  - Plan delta (what changed): Generated H1 wins when valid; existing title remains the fallback when the H1 is missing or invalid. No automatic reprocessing is included.
  - Tradeoffs considered: Reprocessing can correct a prior generated title, while template output without a title does not erase a usable existing value.
  - User approval: no
  - Record proposed: none
- 2026-10-15 03:42 Decision: Proceed with the reviewed leading-H1 plan.
  - Trigger / new insight: The user explicitly approved proceeding and cautioned against exposing real meeting values in Git/GitHub.
  - Plan delta (what changed): Plan status is approved; implementation will use generic title examples only and will not modify or bulk reprocess stored meeting data.
  - Tradeoffs considered: The linked meeting remains unchanged unless normal summary regeneration occurs; changing generation logic addresses future runs without writing real meeting data into repository history.
  - User approval: yes
  - Record proposed: none
- 2026-10-15 03:45 Decision: Compare normalized title headings before adding a Discord title heading.
  - Trigger / new insight: Reviewer found exact-string duplicate detection could add a second title when the generated H1 normalized to the stored title.
  - Plan delta (what changed): Reuse the existing leading-H1 title normalization when deciding whether to prepend a post heading.
  - Tradeoffs considered: Keeps title selection and post de-duplication aligned, with no new state or consumer surface.
  - User approval: yes
  - Record proposed: none
- 2026-10-15 03:46 Decision: Separate H1 normalization from title-candidate eligibility.
  - Trigger / new insight: Delta review found duplicate suppression must recognize built-in headings even though those headings cannot become generated titles.
  - Plan delta (what changed): Normalize a leading H1 in one helper; apply built-in-heading filtering only when selecting a generated title, not when suppressing duplicate post headings.
  - Tradeoffs considered: One normalization implementation serves both behaviors while retaining distinct title eligibility rules.
  - User approval: yes
  - Record proposed: none

## Notes
- Risks: The model may omit or poorly phrase the requested title field; the existing channel fallback remains the degradation path.
