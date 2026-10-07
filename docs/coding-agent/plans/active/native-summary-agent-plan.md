# Plan: Native Summary Agent (ChatGPT subscription / OpenCode Go)

- status: approved
- generated: 2026-10-07
- last_updated: 2026-10-07
- date_basis: UTC
- work_type: code

## Goal

- Replace the CLI-subprocess summary harnesses (`claude`, `cursor_agent`, `opencode`) with an in-process agent built on a Rust agent library (rig), so that tool calls are implemented natively instead of delegated to external CLIs.
- Support two backends:
  - **ChatGPT subscription** via "Sign in with ChatGPT" OAuth (the Codex-style auth; upstream `https://chatgpt.com/backend-api/codex/responses`).
  - **OpenCode Go API key** (subscription plan; endpoints under `https://opencode.ai/zen/go/v1/*`, `Authorization: Bearer $OPENCODE_API_KEY`).
- Preserve the existing agent-workspace contract: the model may only read files under `input/` and write files under `output/`; the app validates the expected output file exactly as today (`read_validated_agent_output`).

## Background / Current State

- `ClaudeSummaryClient` (trait, `src/application/summary.rs`) is the single seam: `summarize(prompt, workdir)`, `summarize_with_output_contract(...)`, plus capability flags.
- `HarnessCliSummaryClient` (`src/infrastructure/integrations.rs`) shells out to `claude -p`, `opencode run`, `cursor-agent -p` with `current_dir` = a materialized agent workspace.
- `AgentWorkspaceBuilder` (`src/infrastructure/workspace.rs`) creates `input/` + `output/` + per-CLI config files (`.claude/settings.json`, `.claude/mcp_servers.json`, `.cursor/cli.json`, `opencode.json`) that pin each CLI to `Read(./input/**)` / `Write(./output/**)`.
- Consumers: summary pipeline (`summary.rs`, `worker.rs`, `runtime.rs`), transcript GEC correction, AI-memory extraction (`ai_memory_extraction.rs`) — all through the same trait.

## Backend Research (verified)

### ChatGPT subscription — the "Sign in with ChatGPT" mechanism

- OpenAI's official OAuth issuer `https://auth.openai.com` with the Codex public client (`client_id app_EMoamEEZ73f0CkXaXp7hrann`) — the identical flow Codex CLI, the ChatGPT desktop app, and opencode's built-in `openai` provider (`packages/core/src/plugin/provider/openai.ts`, methods "chatgpt-browser" / "chatgpt-headless") all use.
- Two grant types:
  - Browser PKCE: `/oauth/authorize` → redirect `http://localhost:1455/auth/callback` → `/oauth/token` (scopes `openid email profile offline_access`, extras `id_token_add_organizations=true`, `codex_cli_simplified_flow=true`).
  - **Device code** (headless): POST `/api/accounts/deviceauth/usercode` → user opens `https://auth.openai.com/codex/device` and enters the code → poll `/api/accounts/deviceauth/token` → token exchange at `/oauth/token` with redirect `https://auth.openai.com/deviceauth/callback`.
- Token bundle = `access_token` + `refresh_token` + `id_token` (JWT carries `chatgpt_account_id`); refresh via `grant_type=refresh_token` at `/oauth/token`, refresh-token rotation must be persisted.
- Requests hit `POST https://chatgpt.com/backend-api/codex/responses` (Responses API, SSE, Codex contract: `store:false`, `stream:true`, `instructions` field, system messages lifted, `originator`/`user-agent`/`chatgpt-account-id` headers). Usage bills against the user's ChatGPT Plus/Pro/Business/Enterprise subscription quota, not API credits.
- **rig already implements all of this**: `rig_core::providers::chatgpt::auth::Authenticator` with `AuthSource::OAuth` reads a flat auth-record file (`access_token/refresh_token/id_token/expires_at/account_id`), auto-refreshes and writes rotated tokens back, and falls back to the device flow via a `DeviceCodeHandler` callback — so first login on a headless server works without Codex CLI. `AuthSource::AccessToken` covers pasted tokens / Enterprise access tokens.
- Caveat: rig's auth file is its own flat JSON, not `~/.codex/auth.json`'s nested shape; importing a codex login is a small conversion if ever needed.

### OpenCode Go

- Subscription (Go / Go Plus) via the OpenCode Console; API key sent as `Authorization: Bearer`.
- Endpoints per model family (from opencode docs):
  - Grok / GPT models → `POST https://opencode.ai/zen/go/v1/responses` (OpenAI Responses API)
  - GLM / Kimi / LongCat / DeepSeek models → `POST https://opencode.ai/zen/go/v1/chat/completions` (OpenAI chat-completions compatible)
- So Go needs BOTH dialects; rig covers both (Responses client + `completions_api()` on the same OpenAI client with a custom base URL).
- Model catalog: `/zen/go/v1/models` (verify at implementation).

Both backends are reachable with the same Rust HTTP plumbing; only the auth header(s) and the wire dialect differ.

## Library Choice

Recommended: **`rig` (`rig-core` + `rig-agent`, crates.io, v0.43, actively maintained)** — the closest Rust equivalent to Strands/ADK:

- Agent abstraction with automatic multi-turn tool-call loop; tools defined via trait/`#[tool]` derive with schemars.
- Ships a dedicated `providers::chatgpt` module (verified in 0.43.0 source): Codex-dialect Responses contract + `auth::Authenticator` handling OAuth device-flow login, token refresh with rotation persistence, and access-token mode. This is exactly the ChatGPT-subscription path.
- Generic `openai` provider supports Responses and (via `completions_api()`) chat-completions dialects with custom base URLs → covers `opencode.ai/zen/go/v1` for both Grok/GPT and GLM/Kimi/DeepSeek families.

Alternatives considered:

| Option | Verdict |
| --- | --- |
| `rig` (rig-core/rig-agent) | **Recommended** — agent loop + tools + chatgpt/openai-compatible providers already exist |
| `genai` 0.6 (jeremychone) | Fallback — client-only; has `openai_resp`, `anthropic`, and a native `opencode_go` adapter + custom `ServiceTargetResolver`, but the tool loop must be hand-rolled |
| Hand-rolled reqwest+SSE mini-loop | Fallback — ~400-600 lines; zero new deps; more code to own |
| Python sidecar (Strands / Google ADK) | Rejected — second runtime, IPC, Docker/packaging churn, contradicts single-binary worker |
| Go sidecar (google.golang.org/adk) | Rejected — same sidecar cost |

Caveat: rig is 0.x (API churn). Mitigation: keep a thin adapter module so swapping to `genai` or a hand-rolled loop stays a small diff.

## Design

### `src/infrastructure/agent/` (new module)

- `NativeAgentSummaryClient` implements `ClaudeSummaryClient`:
  - `supports_untrusted_agent_workspace()` → true (the sandbox is enforced in-process).
  - `summarize` / `summarize_with_output_contract` → build agent, run loop with `command_timeout`, then reuse `read_validated_agent_output` for the output contract unchanged.
- Tools registered on the agent (the only capabilities the model gets):
  - `list_input_files()` → relative paths under `input/`
  - `read_input_file(path)` → rejects anything not under `input/` (reuse `validate_agent_relative_path`)
  - `write_output_file(path, contents)` → only under `output/`; size-capped
  - (`read_output_file(path)` optional for self-check)
- Loop guards: max turns (e.g. 32), `command_timeout`, bounded tool output, retry via existing `RetryPolicy`.
- `require_agent_workdir` marker check is CLI-config-specific today; native path gets a slim variant checking only `input/` + `output/` (or the builder emits a `native.json` marker).
- Async: the trait is sync today; run the rig loop via `tokio::task::block_in_place` + `Handle::block_on` (same pattern `runtime.rs` already uses for workspace materialization), or add an async default method — decide at implementation.

### Auth / config

- `SUMMARY_HARNESS=native` + `SUMMARY_PROVIDER=chatgpt|opencode_go`, `SUMMARY_MODEL` (e.g. `gpt-5.3-codex`, `grok-4.7`, `kimi-k3`, `glm-5.3`).
- ChatGPT:
  - Login: a small `auth login-chatgpt` subcommand (or worker start-up path) drives rig's `Authenticator` device flow — prints `https://auth.openai.com/codex/device` + user code, polls, and writes the auth-record file under a mounted dir (`LLM_CHATGPT_AUTH_DIR`, same pattern as `LLM_CLAUDE_CONFIG_DIR`/`LLM_OPENCODE_DATA_DIR`). No browser or Codex CLI needed on the host.
  - Runtime: `Authenticator` transparently refreshes via `auth.openai.com/oauth/token` and persists rotated tokens; mount the dir read-write.
  - Escape hatch: `SUMMARY_CHATGPT_ACCESS_TOKEN` (+ `SUMMARY_CHATGPT_ACCOUNT_ID`) envs → `AccessToken` mode (also the official ChatGPT Enterprise access-token path for automation).
- OpenCode Go: `OPENCODE_API_KEY` env, optional `SUMMARY_OPENCODE_GO_BASE_URL` override (default `https://opencode.ai/zen/go/v1`).

### Security parity checklist

- Only the 3 file tools above are registered — no shell, web, or MCP; deny-by-default is structural (not a config file).
- Path validation identical to today (`validate_agent_relative_path`, reject `..`/absolute/symlink escapes).
- Output contract validation unchanged (`read_validated_agent_output`, size + content checks).
- Tokens never logged; `Debug` impls redact (existing `HarnessCliSummaryClient` already redacts).
- Workspace cleanup/retention paths unchanged.

## PR Breakdown (small PRs, independently reviewable)

1. `rig` dep + `SUMMARY_HARNESS=native`/`SUMMARY_PROVIDER=opencode_go`/`OPENCODE_API_KEY` config plumbing (#220, merged).
2. Workspace tools (`list_input_files`, `read_input_file`, `write_output_file`) with deny-by-default path enforcement + spawn_blocking lifecycle handling (#221, merged).
3. Native client driven by rig: OpenCode Go dual-dialect (`/responses` + `/chat/completions`) with model→dialect resolution, output contract validation, unsafe opt-in gate, retries (#222).
4. Flip `SUMMARY_HARNESS` default to `native`, make `SUMMARY_MODEL` optional-but-preserved for native, and gate CLI values behind explicit opt-in (this PR); docs/README env table.
5. ChatGPT provider: rig `providers::chatgpt` wiring — `SUMMARY_PROVIDER=chatgpt` with `CHATGPT_AUTH_FILE` OAuth record (or static `CHATGPT_ACCESS_TOKEN`), plus `auth login-chatgpt` subcommand (device flow); README/.env.example docs (this PR).
6. (Deferred, needs sign-off) Remove CLI harnesses + per-CLI workspace configs once native is validated in production.

## Risks / Open Questions

- Q1 (decided): rig is the library.
- Q2 (resolved by research): Sign in with ChatGPT = OpenAI's OAuth at auth.openai.com with the Codex public client; device-code flow is the headless-friendly path and rig implements it end-to-end. Caveat: the client_id belongs to Codex — third-party reuse is common (opencode does it) but not a documented third-party API; the officially sanctioned non-interactive path is ChatGPT Enterprise access tokens (supported via `AccessToken` mode either way).
- Q3 (decided): cover BOTH OpenCode Go dialects — `/responses` (Grok/GPT) and `/chat/completions` (GLM/Kimi/LongCat/DeepSeek).
- Q4 (decided): CLI harnesses become disabled by default going forward — `native` becomes the default `SUMMARY_HARNESS`; `claude`/`cursor_agent`/`opencode` remain only as explicit opt-in (still behind `SUMMARY_ALLOW_UNSAFE_AGENT_HARNESS`), with full removal as a later cleanup.
- rig 0.43's chatgpt auth internals were verified against the crate source (device flow, refresh, auth-file format); minor API names may shift between 0.x releases — pin the version and keep the adapter thin.
- Transcript prompt-injection risk is unchanged: tools are in-process, so worst case the model writes a bad `output/summary.md`, which output validation already bounds.
