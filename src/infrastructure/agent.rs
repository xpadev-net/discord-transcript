//! Workspace tools for the native in-process summary agent.
//!
//! Instead of spawning a coding CLI (`claude`, `opencode`, `cursor-agent`),
//! the native harness (`SUMMARY_HARNESS=native`) runs the agentic tool-call
//! loop inside the worker process via `rig`. The model sees exactly three
//! tools — `list_input_files`, `read_input_file`, `write_output_file` —
//! scoped to the per-job agent workspace (`input/` read-only, `output/`
//! write-only), which keeps the deny-by-default posture of the CLI
//! harnesses without depending on their binaries or config files.
//!
//! Providers are OpenAI-compatible endpoints reachable by API key today:
//! `opencode_go` (OpenCode Go subscription quota). ChatGPT subscription auth
//! (`chatgpt`) lands separately.

use std::fmt::Debug;
use std::fs;
use std::io::{ErrorKind, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use rig_agent::{Agent, AgentBuilder};
use rig_core::DynModel;
use rig_core::operation::Completion;
use rig_core::providers::openai::{self, OpenAIConfig};
use rig_core::tool::{DynamicTool, ToolExecutionError, ToolOutput};
use rig_core::wire::Secret;
use serde_json::{Value, json};
use tokio::runtime::Handle;
use tokio::task::block_in_place;
use tokio::time::timeout;
use tracing::debug;

use crate::application::summary::{
    AgentOutputContract, ClaudeSummaryClient, SUMMARY_OUTPUT_CONTRACT, SummaryError,
};
use crate::bootstrap::config::{SummaryHarness, SummaryProvider};
use crate::infrastructure::integrations::{
    HarnessCliSummaryClient, read_validated_agent_output, remove_stale_agent_output,
};
use crate::infrastructure::retry::{RetryPolicy, retry_with_backoff};
use crate::infrastructure::workspace::{
    AGENT_INPUT_DIR, AGENT_OUTPUT_DIR, validate_agent_relative_path,
};

/// Per-read byte cap for `read_input_file`; the model pages larger files
/// with `offset`/`limit`.
const MAX_TOOL_READ_BYTES: u64 = 4 * 1024 * 1024;
/// Per-write byte cap for `write_output_file`; the output contract's own
/// `max_bytes` is enforced again when the file is validated.
const MAX_TOOL_WRITE_BYTES: u64 = 4 * 1024 * 1024;

fn summary_engine_error(message: impl Into<String>) -> SummaryError {
    SummaryError::SummaryEngine(message.into())
}

/// Path/limits snapshot the tool bodies run against. Bodies capture this
/// — never the `AgentToolFs` itself — so the last `AgentToolFs` reference
/// cannot be dropped inside a tracked task (see `Drop` below).
struct ToolFsInner {
    root: PathBuf,
    max_read_bytes: u64,
    max_write_bytes: u64,
}

/// Filesystem surface the workspace tools expose to the model: reads and
/// listings under `input/`, writes under `output/`. All paths go through
/// `validate_agent_relative_path` and are re-checked against the
/// canonicalized workspace root so symlinked components cannot escape.
pub struct AgentToolFs {
    inner: Arc<ToolFsInner>,
    /// Every tool body spawned so far. `spawn_blocking` tasks keep running
    /// after their tool future is dropped (e.g. when the agent run times
    /// out), so the driver drains this set via `settle` before validating
    /// output, retrying, or letting the workspace be cleaned up.
    pending: tokio::sync::Mutex<tokio::task::JoinSet<()>>,
}

impl AgentToolFs {
    pub fn new(workdir: &Path) -> Result<Self, SummaryError> {
        let root = workdir.canonicalize().map_err(|err| {
            summary_engine_error(format!(
                "summary agent: failed to resolve workspace {}: {err}",
                workdir.display()
            ))
        })?;
        for dir in [AGENT_INPUT_DIR, AGENT_OUTPUT_DIR] {
            if !root.join(dir).is_dir() {
                return Err(summary_engine_error(format!(
                    "summary agent: workspace {} missing {dir}/ directory",
                    root.display()
                )));
            }
        }
        Ok(Self {
            inner: Arc::new(ToolFsInner {
                root,
                max_read_bytes: MAX_TOOL_READ_BYTES,
                max_write_bytes: MAX_TOOL_WRITE_BYTES,
            }),
            pending: tokio::sync::Mutex::new(tokio::task::JoinSet::new()),
        })
    }

    /// Run a synchronous tool body on the blocking pool so file IO does
    /// not stall the async worker driving the agent loop.
    async fn spawn_tool<F, T>(&self, body: F) -> Result<T, ToolExecutionError>
    where
        F: FnOnce() -> Result<T, ToolExecutionError> + Send + 'static,
        T: Send + 'static,
    {
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.pending.lock().await.spawn_blocking(move || {
            let _ = tx.send(body());
        });
        rx.await.map_err(|_| {
            ToolExecutionError::other("workspace tool task dropped before finishing")
        })?
    }

    /// Wait for every tool task started so far to finish. Must run after
    /// the agent loop ends — on timeout the run's future is dropped while
    /// a started write may still be in flight, and retrying or removing
    /// the workspace without draining first would race that write.
    pub async fn settle(&self) {
        let mut pending = self.pending.lock().await;
        while pending.join_next().await.is_some() {}
    }

    pub fn list_inputs(&self) -> Result<Value, ToolExecutionError> {
        self.inner.list_inputs()
    }

    pub fn read_input(
        &self,
        relative: &str,
        offset: u64,
        limit: u64,
    ) -> Result<String, ToolExecutionError> {
        self.inner.read_input(relative, offset, limit)
    }

    pub fn write_output(&self, relative: &str, contents: &str) -> Result<u64, ToolExecutionError> {
        self.inner.write_output(relative, contents)
    }
}

impl Drop for AgentToolFs {
    /// Join every started tool task before the fs goes away. A
    /// `spawn_blocking` task cannot be cancelled once running — dropping
    /// the fs (end of an attempt, workspace teardown) without this wait
    /// would let a timed-out write land on the workspace afterwards. Safe
    /// from self-join because tool bodies only hold `ToolFsInner`, never
    /// this object.
    fn drop(&mut self) {
        let pending = self.pending.get_mut();
        loop {
            while pending.try_join_next().is_some() {}
            if pending.is_empty() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
    }
}

impl ToolFsInner {
    /// Resolve a model-supplied relative path to an absolute path under
    /// `root`, refusing anything that does not start with `required_first`.
    fn resolve(&self, relative: &str, required_first: &str) -> Result<PathBuf, ToolExecutionError> {
        let relative = validate_agent_relative_path(Path::new(relative), required_first)
            .map_err(|err| ToolExecutionError::refused(format!("invalid workspace path: {err}")))?;
        let candidate = self.root.join(&relative);
        // Canonicalize the deepest existing ancestor so a symlinked component
        // (e.g. a leftover `input/` symlink) cannot redirect outside root.
        let mut ancestor = candidate.as_path();
        while let Some(parent) = ancestor.parent() {
            if parent.exists() {
                ancestor = parent;
                break;
            }
            ancestor = parent;
        }
        if ancestor.exists() {
            let canonical = ancestor.canonicalize().map_err(|err| {
                ToolExecutionError::other(format!("failed to resolve workspace path: {err}"))
            })?;
            if !canonical.starts_with(&self.root) {
                return Err(ToolExecutionError::refused(
                    "workspace path escapes the agent workspace",
                ));
            }
        }
        Ok(candidate)
    }

    fn list_inputs(&self) -> Result<Value, ToolExecutionError> {
        let input_dir = self.root.join(AGENT_INPUT_DIR);
        let mut files = Vec::new();
        let mut stack = vec![input_dir.clone()];
        while let Some(dir) = stack.pop() {
            let entries = fs::read_dir(&dir).map_err(|err| {
                ToolExecutionError::other(format!("failed to list {}: {err}", dir.display()))
            })?;
            for entry in entries {
                let entry = entry.map_err(|err| {
                    ToolExecutionError::other(format!("failed to read input dir entry: {err}"))
                })?;
                let path = entry.path();
                let file_type = entry.file_type().map_err(|err| {
                    ToolExecutionError::other(format!("failed to stat {}: {err}", path.display()))
                })?;
                if file_type.is_dir() {
                    stack.push(path);
                } else if file_type.is_file() {
                    let relative = path
                        .strip_prefix(&self.root)
                        .unwrap_or(&path)
                        .to_string_lossy()
                        .into_owned();
                    files.push(relative);
                }
                // Symlinks and other non-regular entries are skipped rather
                // than followed, so nothing outside the workspace is listed.
            }
        }
        files.sort();
        Ok(json!({ "files": files }))
    }

    fn read_input(
        &self,
        relative: &str,
        offset: u64,
        limit: u64,
    ) -> Result<String, ToolExecutionError> {
        let path = self.resolve(relative, AGENT_INPUT_DIR)?;
        let metadata = path.symlink_metadata().map_err(|err| match err.kind() {
            ErrorKind::NotFound => {
                ToolExecutionError::not_found(format!("input file not found: {relative}"))
            }
            _ => ToolExecutionError::other(format!("failed to stat {relative}: {err}")),
        })?;
        if !metadata.is_file() {
            return Err(ToolExecutionError::refused(format!(
                "not a regular file: {relative}"
            )));
        }
        // The resolved path could still traverse a symlink; re-check the
        // canonical file location stays inside the workspace.
        let canonical = path.canonicalize().map_err(|err| {
            ToolExecutionError::other(format!("failed to resolve {relative}: {err}"))
        })?;
        if !canonical.starts_with(&self.root) {
            return Err(ToolExecutionError::refused(
                "input path escapes the agent workspace",
            ));
        }
        // Bounded read: only the requested window (plus a few bytes of
        // char-boundary padding on each side) is loaded, so paging a huge
        // transcript never materializes the whole file. `win_start` backs
        // `offset` up by up to a full codepoint, and the buffer extends
        // `limit` bytes past `end` so mid-codepoint cuts can be snapped
        // forward instead of returning an empty page before EOF.
        let file_len = metadata.len();
        if offset >= file_len {
            // Past EOF (including empty files): an empty page, not an
            // error, so the model can page until the file runs out.
            return Ok(String::new());
        }
        let mut file = fs::File::open(&canonical).map_err(|err| {
            ToolExecutionError::other(format!("failed to open {relative}: {err}"))
        })?;
        let win_start = offset.saturating_sub(3).min(file_len);
        let win_end = offset
            .saturating_add(limit.min(self.max_read_bytes))
            .saturating_add(4)
            .min(file_len);
        let mut buf = vec![0u8; (win_end - win_start) as usize];
        file.seek(SeekFrom::Start(win_start)).map_err(|err| {
            ToolExecutionError::other(format!("failed to seek {relative}: {err}"))
        })?;
        file.read_exact(&mut buf).map_err(|err| {
            ToolExecutionError::other(format!("failed to read {relative}: {err}"))
        })?;
        // A byte is a UTF-8 continuation iff its top bits are 10xxxxxx.
        let mut start = (offset - win_start) as usize;
        while start > 0 && (buf[start] & 0xC0) == 0x80 {
            start -= 1;
        }
        let snapped = win_start as usize + start;
        let mut end = snapped
            .saturating_add(limit.min(self.max_read_bytes) as usize)
            .min(file_len as usize)
            .saturating_sub(win_start as usize);
        while end < buf.len() && (buf[end] & 0xC0) == 0x80 {
            end += 1;
        }
        String::from_utf8(buf[start..end].to_vec()).map_err(|_| {
            ToolExecutionError::invalid_args(format!("input file is not valid UTF-8: {relative}"))
        })
    }

    fn write_output(&self, relative: &str, contents: &str) -> Result<u64, ToolExecutionError> {
        if contents.len() as u64 > self.max_write_bytes {
            return Err(ToolExecutionError::refused(format!(
                "write exceeds {} bytes limit",
                self.max_write_bytes
            )));
        }
        let path = self.resolve(relative, AGENT_OUTPUT_DIR)?;
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|err| {
                ToolExecutionError::other(format!(
                    "failed to create output dir {}: {err}",
                    parent.display()
                ))
            })?;
            let canonical_parent = parent.canonicalize().map_err(|err| {
                ToolExecutionError::other(format!("failed to resolve output dir: {err}"))
            })?;
            if !canonical_parent.starts_with(&self.root) {
                return Err(ToolExecutionError::refused(
                    "output path escapes the agent workspace",
                ));
            }
        }
        if let Ok(metadata) = path.symlink_metadata()
            && !metadata.is_file()
        {
            return Err(ToolExecutionError::refused(format!(
                "refusing to overwrite non-regular file: {relative}"
            )));
        }
        fs::write(&path, contents).map_err(|err| {
            ToolExecutionError::other(format!("failed to write {relative}: {err}"))
        })?;
        Ok(contents.len() as u64)
    }
}

#[derive(serde::Deserialize)]
struct ReadInputArgs {
    path: String,
    /// Byte offset into the UTF-8 file (snapped to a char boundary).
    offset: Option<u64>,
    /// Byte limit for this read (capped server-side).
    limit: Option<u64>,
}

#[derive(serde::Deserialize)]
struct WriteOutputArgs {
    path: String,
    contents: String,
}

fn parse_args<T: serde::de::DeserializeOwned>(args: Value) -> Result<T, ToolExecutionError> {
    serde_json::from_value(args)
        .map_err(|err| ToolExecutionError::invalid_args(format!("invalid tool arguments: {err}")))
}

fn path_arg_schema(extra: serde_json::Map<String, Value>) -> Value {
    let mut properties = serde_json::Map::new();
    properties.insert(
        "path".to_owned(),
        json!({
            "type": "string",
            "description": "Workspace-relative path such as `input/transcript.md` or `output/summary.md`",
        }),
    );
    for (key, value) in extra {
        properties.insert(key, value);
    }
    json!({
        "type": "object",
        "properties": properties,
        "required": ["path"],
    })
}

/// OpenCode Go's OpenAI-compatible endpoint (Grok/GPT ids on `/responses`,
/// the open-model ids on `/chat/completions`).
const OPENCODE_GO_BASE_URL: &str = "https://opencode.ai/zen/go/v1";

/// Dialect descriptor for OpenCode Go; the registry name only affects error
/// and telemetry strings. `api_key_env` is unused because the key is passed
/// explicitly from config.
static OPENCODE_GO_DIALECT: openai::wire::Dialect =
    openai::wire::Dialect::gateway("opencode-go", OPENCODE_GO_BASE_URL, "OPENCODE_API_KEY");

/// Hard cap on the tool-call loop so a confused model cannot spin forever.
const DEFAULT_MAX_AGENT_TURNS: usize = 32;

/// System prompt for every native summary-agent run. The per-task prompt
/// (carrying the transcript/context) arrives as the user message.
const AGENT_PREAMBLE: &str = "You are a document-processing agent inside a sandboxed workspace.\n\
Use `list_input_files` to enumerate the files provided under `input/` and `read_input_file` to read them.\n\
Write with `write_output_file`, under `output/`, exactly the path the prompt requires. \
Your chat reply is discarded; the only artifact that matters is the file you write.\n\
Never fabricate file contents you have not read.";

/// The three tools the model can call during a summary run. Everything the
/// agent is allowed to touch is expressed here — there is no shell, no
/// network tool, no arbitrary fs access.
pub fn workspace_tools(fs: Arc<AgentToolFs>) -> Vec<DynamicTool> {
    let list_fs = Arc::clone(&fs);
    let read_fs = Arc::clone(&fs);
    let write_fs = fs;
    vec![
        DynamicTool::new(
            "list_input_files",
            "List every file available under input/ as workspace-relative paths.",
            json!({ "type": "object", "properties": {} }),
            move |args: Value| {
                let _ = args;
                let fs = Arc::clone(&list_fs);
                Box::pin(async move {
                    // File IO is synchronous; keep it off the async worker.
                    let inner = Arc::clone(&fs.inner);
                    fs.spawn_tool(move || inner.list_inputs())
                        .await
                        .map(ToolOutput::json)
                })
            },
        ),
        DynamicTool::new(
            "read_input_file",
            "Read a UTF-8 file under input/. Use offset/limit (bytes) to page large files.",
            path_arg_schema({
                let mut extra = serde_json::Map::new();
                extra.insert(
                    "offset".to_owned(),
                    json!({"type": "integer", "minimum": 0, "description": "byte offset to start reading at"}),
                );
                extra.insert(
                    "limit".to_owned(),
                    json!({"type": "integer", "minimum": 1, "description": "max bytes to return"}),
                );
                extra
            }),
            move |args: Value| {
                let fs = Arc::clone(&read_fs);
                Box::pin(async move {
                    let args: ReadInputArgs = parse_args(args)?;
                    let inner = Arc::clone(&fs.inner);
                    fs.spawn_tool(move || {
                        inner.read_input(
                            &args.path,
                            args.offset.unwrap_or(0),
                            args.limit.unwrap_or(u64::MAX),
                        )
                    })
                    .await
                    .map(ToolOutput::text)
                })
            },
        ),
        DynamicTool::new(
            "write_output_file",
            "Write a UTF-8 file under output/ (exactly the path the prompt requires).",
            {
                let mut schema = path_arg_schema({
                    let mut extra = serde_json::Map::new();
                    extra.insert(
                        "contents".to_owned(),
                        json!({"type": "string", "description": "full file contents to write"}),
                    );
                    extra
                });
                schema["required"] = json!(["path", "contents"]);
                schema
            },
            move |args: Value| {
                let fs = Arc::clone(&write_fs);
                Box::pin(async move {
                    let args: WriteOutputArgs = parse_args(args)?;
                    let inner = Arc::clone(&fs.inner);
                    fs.spawn_tool(move || inner.write_output(&args.path, &args.contents))
                        .await
                        .map(|written| ToolOutput::text(format!("wrote {written} bytes")))
                })
            },
        ),
    ]
}

/// Which wire protocol a model id uses on a provider's endpoint.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CompletionRoute {
    /// `POST /responses` (OpenAI Responses API dialect).
    Responses,
    /// `POST /chat/completions`.
    ChatCompletions,
}

/// OpenCode Go serves Grok/GPT ids on `/responses` and the open models
/// (GLM, Kimi, DeepSeek, LongCat, …) on `/chat/completions`.
fn opencode_go_route(model: &str) -> CompletionRoute {
    let normalized = model.trim().to_ascii_lowercase();
    if normalized.starts_with("grok") || normalized.starts_with("gpt") {
        CompletionRoute::Responses
    } else {
        CompletionRoute::ChatCompletions
    }
}

/// In-process agent client for `SUMMARY_HARNESS=native`.
#[derive(Debug)]
pub struct NativeAgentSummaryClient {
    pub provider: SummaryProvider,
    pub model: String,
    /// Provider credential. `Secret` redacts itself in Debug output.
    pub api_key: Secret,
    pub allow_unsafe_agent_harness: bool,
    pub retry_policy: RetryPolicy,
    pub command_timeout: Duration,
    /// Max model turns per attempt; defaults to `DEFAULT_MAX_AGENT_TURNS`.
    pub max_agent_turns: usize,
}

impl NativeAgentSummaryClient {
    fn completion_model(&self) -> Result<DynModel<Completion>, SummaryError> {
        if self.api_key.is_empty() {
            return Err(summary_engine_error(format!(
                "summary harness `native` provider `{}` requires an API key",
                self.provider
            )));
        }
        match self.provider {
            SummaryProvider::OpenCodeGo => {
                let config = OpenAIConfig::with_key(&OPENCODE_GO_DIALECT, self.api_key.expose());
                let client = config.client();
                match opencode_go_route(&self.model) {
                    CompletionRoute::Responses => Ok(client.responses(self.model.clone()).erase()),
                    CompletionRoute::ChatCompletions => Ok(client.chat(self.model.clone()).erase()),
                }
            }
        }
    }

    /// One full agent run over the workspace: builds the rig agent, runs the
    /// tool-call loop to completion (or turn/timeout limit), then validates
    /// the produced output file against `contract`.
    fn run_agent_attempt(
        &self,
        model: DynModel<Completion>,
        prompt: &str,
        workdir: &Path,
        output: AgentOutputContract,
    ) -> Result<String, SummaryError> {
        let fs = Arc::new(AgentToolFs::new(workdir)?);
        let agent = AgentBuilder::new(model)
            .preamble(AGENT_PREAMBLE)
            .default_max_turns(self.max_agent_turns)
            .dynamic_tools(workspace_tools(Arc::clone(&fs)))
            .build();
        let outcome = self.drive_agent(agent, prompt);
        // A dropped tool future (timeout, cancelled run) does not stop its
        // blocking file task — wait for every started task before touching
        // output/, so a lingering write cannot race validation or the next
        // attempt's cleanup.
        run_future_blocking(fs.settle())?;
        outcome?;
        read_validated_agent_output(SummaryHarness::Native, workdir, output, b"", b"")
    }

    fn drive_agent(&self, agent: Agent, prompt: &str) -> Result<(), SummaryError> {
        let run_timeout = self.command_timeout;
        let prompt = prompt.to_owned();
        let future = async move { agent.prompt(prompt).await };
        let outcome = run_future_blocking(async { timeout(run_timeout, future).await })?;
        match outcome {
            Ok(Ok(response)) => {
                debug!(
                    output_len = response.output.len(),
                    usage = ?response.usage,
                    "native summary agent completed"
                );
                Ok(())
            }
            Ok(Err(err)) => Err(summary_engine_error(format!(
                "native summary agent failed: {err}"
            ))),
            Err(_) => Err(summary_engine_error(format!(
                "native summary agent timed out after {}s",
                run_timeout.as_secs()
            ))),
        }
    }
}

/// Run `future` on the ambient tokio runtime from a sync context, or on a
/// throwaway current-thread runtime when there is none (unit tests).
fn run_future_blocking<F, T>(future: F) -> Result<T, SummaryError>
where
    F: std::future::Future<Output = T> + Send,
    T: Send,
{
    match Handle::try_current() {
        Ok(handle) => Ok(block_in_place(|| handle.block_on(future))),
        Err(_) => Ok(tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|err| summary_engine_error(format!("failed to start agent runtime: {err}")))?
            .block_on(future)),
    }
}

/// Only workspaces materialized by `AgentWorkspaceBuilder` may be handed to
/// the agent: `input/` and `output/` must exist. (CLI-harness config markers
/// are irrelevant here — the tool surface is compiled in.)
fn require_native_agent_workdir(workdir: Option<&Path>) -> Result<&Path, SummaryError> {
    let workdir = workdir
        .ok_or_else(|| summary_engine_error("summary harness native: workdir not provided"))?;
    if !workdir.join(AGENT_INPUT_DIR).is_dir() || !workdir.join(AGENT_OUTPUT_DIR).is_dir() {
        return Err(summary_engine_error(
            "summary harness native: workdir missing expected agent workspace directories (input/, output/)",
        ));
    }
    Ok(workdir)
}

impl ClaudeSummaryClient for NativeAgentSummaryClient {
    fn supports_transcript_correction(&self) -> bool {
        // The GEC correction prompt does not instruct the agent to write
        // `output/` (it predates the file contract), so native correction is
        // disabled alongside the CLI harnesses for now.
        false
    }

    fn supports_untrusted_agent_workspace(&self) -> bool {
        self.allow_unsafe_agent_harness
    }

    fn summarize(&self, prompt: &str, workdir: Option<&Path>) -> Result<String, SummaryError> {
        self.summarize_with_output_contract(prompt, workdir, SUMMARY_OUTPUT_CONTRACT)
    }

    fn summarize_with_output_contract(
        &self,
        prompt: &str,
        workdir: Option<&Path>,
        output: AgentOutputContract,
    ) -> Result<String, SummaryError> {
        if !self.supports_untrusted_agent_workspace() {
            return Err(summary_engine_error(
                "refusing to run native summary agent over untrusted transcript/context data without SUMMARY_ALLOW_UNSAFE_AGENT_HARNESS=true",
            ));
        }
        let workdir = require_native_agent_workdir(workdir)?;
        let model = self.completion_model()?;
        self.summarize_with_model(&model, prompt, workdir, output)
    }
}

impl NativeAgentSummaryClient {
    /// `summarize` with the completion model supplied — tests inject a
    /// scripted mock instead of a live provider.
    fn summarize_with_model(
        &self,
        model: &DynModel<Completion>,
        prompt: &str,
        workdir: &Path,
        output: AgentOutputContract,
    ) -> Result<String, SummaryError> {
        // Retried attempts start from a clean conversation rather than
        // compounding a broken transcript, and each attempt must produce
        // its own output file — a leftover from a failed attempt is deleted
        // before the next run so it cannot be mistaken for fresh output.
        retry_with_backoff(self.retry_policy, |_| {
            remove_stale_agent_output(workdir, output)?;
            self.run_agent_attempt(model.clone(), prompt, workdir, output)
        })
    }
}

/// Runtime-selected summary client: the CLI harnesses, the native agent, or
/// a placeholder for a native harness selected while summaries are disabled
/// (provider credentials are not collected in that state).
#[derive(Debug)]
pub enum SummaryClient {
    Cli(HarnessCliSummaryClient),
    Native(NativeAgentSummaryClient),
    /// Configured `SUMMARY_HARNESS=native` without provider credentials —
    /// only possible when the summary runtime is disabled, so every summary
    /// method reports a configuration error rather than running.
    Disabled,
}

const DISABLED_SUMMARY_ERROR: &str = "summary harness `native` is selected but provider credentials are missing (expected only when summaries are disabled)";

impl SummaryClient {
    /// Full-transcript LLM correction needs a boundary other than argv; the
    /// CLI harnesses are argv-limited. The native agent could lift that,
    /// but the correction prompt still says "output only the transcript"
    /// rather than naming the output file — keep it disabled until the
    /// prompt is rewritten for the file contract.
    pub fn can_run_llm_transcript_correction(&self) -> bool {
        match self {
            Self::Cli(client) => client.can_run_llm_transcript_correction(),
            Self::Native(_) | Self::Disabled => false,
        }
    }
}

impl ClaudeSummaryClient for SummaryClient {
    fn supports_transcript_correction(&self) -> bool {
        match self {
            Self::Cli(client) => client.supports_transcript_correction(),
            Self::Native(client) => client.supports_transcript_correction(),
            Self::Disabled => false,
        }
    }

    fn supports_untrusted_agent_workspace(&self) -> bool {
        match self {
            Self::Cli(client) => client.supports_untrusted_agent_workspace(),
            Self::Native(client) => client.supports_untrusted_agent_workspace(),
            Self::Disabled => false,
        }
    }

    fn summarize(&self, prompt: &str, workdir: Option<&Path>) -> Result<String, SummaryError> {
        match self {
            Self::Cli(client) => client.summarize(prompt, workdir),
            Self::Native(client) => client.summarize(prompt, workdir),
            Self::Disabled => Err(summary_engine_error(DISABLED_SUMMARY_ERROR)),
        }
    }

    fn summarize_with_output_contract(
        &self,
        prompt: &str,
        workdir: Option<&Path>,
        output: AgentOutputContract,
    ) -> Result<String, SummaryError> {
        match self {
            Self::Cli(client) => client.summarize_with_output_contract(prompt, workdir, output),
            Self::Native(client) => client.summarize_with_output_contract(prompt, workdir, output),
            Self::Disabled => Err(summary_engine_error(DISABLED_SUMMARY_ERROR)),
        }
    }
}

/// Everything `build_summary_client` needs, lifted out of the argument
/// list: the harness picks which variant is constructed; `provider` and
/// `api_key` are required for `SummaryHarness::Native` when summaries run
/// (the config layer enforces that — a missing pair here means summaries
/// are disabled).
pub struct SummaryClientConfig {
    pub harness: SummaryHarness,
    pub command_path: String,
    pub model: String,
    pub provider: Option<SummaryProvider>,
    /// Provider credential. Kept out of Debug output.
    pub api_key: Option<String>,
    pub allow_unsafe_agent_harness: bool,
    pub retry_policy: RetryPolicy,
    pub command_timeout: Duration,
}

impl Debug for SummaryClientConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SummaryClientConfig")
            .field("harness", &self.harness)
            .field("command_path", &self.command_path)
            .field("model", &self.model)
            .field("provider", &self.provider)
            .field("api_key", &self.api_key.as_ref().map(|_| "[redacted]"))
            .field(
                "allow_unsafe_agent_harness",
                &self.allow_unsafe_agent_harness,
            )
            .field("retry_policy", &self.retry_policy)
            .field("command_timeout", &self.command_timeout)
            .finish()
    }
}

/// Build the summary client for the configured harness.
pub fn build_summary_client(config: SummaryClientConfig) -> Result<SummaryClient, SummaryError> {
    match config.harness {
        SummaryHarness::Native => match (config.provider, config.api_key) {
            (Some(provider), Some(api_key)) if !api_key.trim().is_empty() => {
                Ok(SummaryClient::Native(NativeAgentSummaryClient {
                    provider,
                    model: config.model,
                    api_key: Secret::from(api_key),
                    allow_unsafe_agent_harness: config.allow_unsafe_agent_harness,
                    retry_policy: config.retry_policy,
                    command_timeout: config.command_timeout,
                    max_agent_turns: DEFAULT_MAX_AGENT_TURNS,
                }))
            }
            // Config validation requires provider+key for native whenever a
            // role runs summaries, so reaching here means the runtime is
            // disabled — start cleanly and error only if a summary runs.
            _ => Ok(SummaryClient::Disabled),
        },
        _ => Ok(SummaryClient::Cli(HarnessCliSummaryClient {
            harness: config.harness,
            command_path: config.command_path,
            model: config.model,
            allow_unsafe_agent_harness: config.allow_unsafe_agent_harness,
            retry_policy: config.retry_policy,
            command_timeout: config.command_timeout,
        })),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rig_core::test_utils::{MockCompletionModel, MockTurn};
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::SystemTime;

    static WORKDIR_COUNTER: AtomicU64 = AtomicU64::new(0);

    /// A fresh workspace shaped like what `AgentWorkspaceBuilder` produces:
    /// `input/` and `output/` directories only (CLI marker files are not
    /// required by the native harness).
    fn fresh_workdir() -> PathBuf {
        let unique = format!(
            "discord_transcript_agent_test_{}_{}_{}",
            std::process::id(),
            WORKDIR_COUNTER.fetch_add(1, Ordering::Relaxed),
            SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        let root = std::env::temp_dir().join(unique);
        fs::create_dir_all(root.join(AGENT_INPUT_DIR)).unwrap();
        fs::create_dir_all(root.join(AGENT_OUTPUT_DIR)).unwrap();
        root
    }

    #[test]
    fn tool_fs_lists_and_reads_inputs() {
        let root = fresh_workdir();
        fs::create_dir_all(root.join("input/transcript")).unwrap();
        fs::write(root.join("input/transcript/t.md"), "hello world").unwrap();
        fs::write(root.join("input/context.md"), "ctx").unwrap();

        let fs = AgentToolFs::new(&root).unwrap();
        let listing = fs.list_inputs().unwrap();
        let files = listing["files"].as_array().unwrap();
        assert_eq!(files.len(), 2);
        assert!(
            files
                .iter()
                .any(|f| f.as_str() == Some("input/transcript/t.md"))
        );

        assert_eq!(
            fs.read_input("input/context.md", 0, u64::MAX).unwrap(),
            "ctx"
        );
        // Paging: byte range snapped to char boundaries.
        assert_eq!(fs.read_input("input/context.md", 1, 2).unwrap(), "tx");
    }

    #[test]
    fn tool_fs_read_pages_multibyte_content() {
        let root = fresh_workdir();
        fs::write(root.join("input/t.md"), "あいうえお").unwrap();
        let fs = AgentToolFs::new(&root).unwrap();
        // "あ" is 3 bytes; offset 1 snaps back to 0.
        let page = fs.read_input("input/t.md", 1, 3).unwrap();
        assert_eq!(page, "あ");
    }

    #[test]
    fn tool_fs_read_snaps_limit_forward_to_char_boundary() {
        let root = fresh_workdir();
        fs::write(root.join("input/t.md"), "あいうえお").unwrap();
        let fs = AgentToolFs::new(&root).unwrap();
        // limit=1 cuts inside "あ"; the page extends to the char boundary
        // instead of returning an empty page before EOF.
        assert_eq!(fs.read_input("input/t.md", 0, 1).unwrap(), "あ");
    }

    #[test]
    fn tool_fs_read_at_eof_returns_empty_page() {
        let root = fresh_workdir();
        fs::write(root.join("input/t.md"), "abc").unwrap();
        let fs = AgentToolFs::new(&root).unwrap();
        // Paging to exactly the end — or past it — returns an empty page
        // so the model can read until the file runs out.
        assert_eq!(fs.read_input("input/t.md", 3, 10).unwrap(), "");
        assert_eq!(fs.read_input("input/t.md", 100, 10).unwrap(), "");
        fs::write(root.join("input/empty.md"), "").unwrap();
        assert_eq!(fs.read_input("input/empty.md", 0, 10).unwrap(), "");
    }

    #[test]
    fn tool_fs_read_rejects_non_utf8_input() {
        let root = fresh_workdir();
        fs::write(root.join("input/b.bin"), [0xff, 0xfe, 0xfd]).unwrap();
        let fs = AgentToolFs::new(&root).unwrap();
        assert!(fs.read_input("input/b.bin", 0, u64::MAX).is_err());
    }

    #[test]
    fn tool_fs_rejects_paths_outside_their_root() {
        let root = fresh_workdir();
        fs::write(root.join("input/t.md"), "x").unwrap();
        let fs = AgentToolFs::new(&root).unwrap();

        for bad in [
            "../etc/passwd",
            "input/../output/x.md",
            "/etc/passwd",
            "input",
            "output/evil.md",
        ] {
            assert!(
                fs.read_input(bad, 0, u64::MAX).is_err(),
                "read_input should reject {bad}"
            );
        }
        for bad in ["../x.md", "input/evil.md", "/abs/x.md", "output"] {
            assert!(
                fs.write_output(bad, "x").is_err(),
                "write_output should reject {bad}"
            );
        }
    }

    #[test]
    fn tool_fs_write_respects_size_cap() {
        let root = fresh_workdir();
        let fs = AgentToolFs::new(&root).unwrap();
        let oversized = "x".repeat((MAX_TOOL_WRITE_BYTES + 1) as usize);
        assert!(fs.write_output("output/big.md", &oversized).is_err());
        assert!(fs.write_output("output/small.md", "ok").is_ok());
    }

    fn test_client() -> NativeAgentSummaryClient {
        NativeAgentSummaryClient {
            provider: SummaryProvider::OpenCodeGo,
            model: "test-model".to_owned(),
            api_key: Secret::from("test-key"),
            allow_unsafe_agent_harness: true,
            retry_policy: RetryPolicy {
                max_attempts: 1,
                initial_delay: Duration::from_millis(1),
                backoff_multiplier: 1,
                max_delay: Duration::from_millis(1),
            },
            command_timeout: Duration::from_secs(60),
            max_agent_turns: 8,
        }
    }

    #[test]
    fn agent_writes_contract_output_via_tools() {
        let root = fresh_workdir();
        fs::write(root.join("input/transcript.md"), "meeting transcript").unwrap();

        let model = MockCompletionModel::from_turns([
            MockTurn::tool_call("c1", "list_input_files", json!({})),
            MockTurn::tool_call(
                "c2",
                "read_input_file",
                json!({ "path": "input/transcript.md" }),
            ),
            MockTurn::tool_call(
                "c3",
                "write_output_file",
                json!({ "path": "output/summary.md", "contents": "# Summary\nhello" }),
            ),
            MockTurn::text("done"),
        ])
        .erase();

        let client = test_client();
        let out = client
            .run_agent_attempt(
                model,
                "summarize the inputs",
                &root,
                SUMMARY_OUTPUT_CONTRACT,
            )
            .unwrap();
        assert_eq!(out, "# Summary\nhello");
        assert_eq!(
            fs::read_to_string(root.join("output/summary.md")).unwrap(),
            "# Summary\nhello"
        );
    }

    #[test]
    fn agent_cannot_write_outside_output() {
        let root = fresh_workdir();
        let model = MockCompletionModel::from_turns([
            MockTurn::tool_call(
                "c1",
                "write_output_file",
                json!({ "path": "input/evil.md", "contents": "nope" }),
            ),
            MockTurn::tool_call(
                "c2",
                "write_output_file",
                json!({ "path": "../escape.md", "contents": "nope" }),
            ),
            MockTurn::tool_call(
                "c3",
                "write_output_file",
                json!({ "path": "output/summary.md", "contents": "ok" }),
            ),
            MockTurn::text("done"),
        ])
        .erase();

        let client = test_client();
        let out = client
            .run_agent_attempt(model, "prompt", &root, SUMMARY_OUTPUT_CONTRACT)
            .unwrap();
        assert_eq!(out, "ok");
        assert!(!root.join("input/evil.md").exists());
        assert!(!root.parent().unwrap().join("escape.md").exists());
    }

    #[test]
    fn summarize_requires_unsafe_opt_in() {
        let root = fresh_workdir();
        let mut client = test_client();
        client.allow_unsafe_agent_harness = false;
        let err = client
            .summarize_with_output_contract("prompt", Some(&root), SUMMARY_OUTPUT_CONTRACT)
            .unwrap_err();
        assert!(
            err.to_string()
                .contains("SUMMARY_ALLOW_UNSAFE_AGENT_HARNESS")
        );
    }

    #[test]
    fn summarize_rejects_workdir_without_dirs() {
        let dir = std::env::temp_dir().join(format!(
            "discord_transcript_agent_test_nodirs_{}_{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&dir).unwrap();
        let client = test_client();
        let err = client
            .summarize_with_output_contract("prompt", Some(&dir), SUMMARY_OUTPUT_CONTRACT)
            .unwrap_err();
        assert!(err.to_string().contains("input/"));
    }

    #[test]
    fn missing_contract_output_is_an_error() {
        let root = fresh_workdir();
        let model = MockCompletionModel::from_turns([MockTurn::text("no tools")]).erase();
        let client = test_client();
        let err = client
            .run_agent_attempt(model, "prompt", &root, SUMMARY_OUTPUT_CONTRACT)
            .unwrap_err();
        assert!(
            err.to_string().contains("summary.md") || err.to_string().contains("output"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn retry_deletes_stale_output_between_attempts() {
        let root = fresh_workdir();
        // First attempt writes output then fails; second succeeds without
        // writing — if the stale file survived, the result would wrongly
        // come back Ok("stale").
        let model = MockCompletionModel::from_turns([
            MockTurn::tool_call(
                "c1",
                "write_output_file",
                json!({ "path": "output/summary.md", "contents": "stale" }),
            ),
            MockTurn::error("attempt 1 boom"),
            MockTurn::text("done without writing"),
        ])
        .erase();
        let mut client = test_client();
        client.retry_policy.max_attempts = 2;
        let err = client
            .summarize_with_model(&model, "prompt", &root, SUMMARY_OUTPUT_CONTRACT)
            .unwrap_err();
        assert!(
            err.to_string().contains("summary.md") || err.to_string().contains("output"),
            "unexpected error: {err}"
        );
        assert!(!root.join("output/summary.md").exists());
    }

    #[test]
    fn opencode_go_route_selects_wire_by_model_id() {
        assert_eq!(
            opencode_go_route("grok-code-fast-1"),
            CompletionRoute::Responses
        );
        assert_eq!(opencode_go_route("gpt-5-codex"), CompletionRoute::Responses);
        assert_eq!(
            opencode_go_route("GLM-4.6"),
            CompletionRoute::ChatCompletions
        );
        assert_eq!(
            opencode_go_route("kimi-k2"),
            CompletionRoute::ChatCompletions
        );
    }

    #[test]
    fn build_summary_client_dispatches_on_harness() {
        let native = build_summary_client(SummaryClientConfig {
            harness: SummaryHarness::Native,
            command_path: String::new(),
            model: "model".to_owned(),
            provider: Some(SummaryProvider::OpenCodeGo),
            api_key: Some("key".to_owned()),
            allow_unsafe_agent_harness: true,
            retry_policy: RetryPolicy::default(),
            command_timeout: Duration::from_secs(1),
        })
        .unwrap();
        assert!(matches!(native, SummaryClient::Native(_)));

        // Native without provider/key (summaries disabled) yields the
        // disabled client instead of a startup failure.
        let disabled = build_summary_client(SummaryClientConfig {
            harness: SummaryHarness::Native,
            command_path: String::new(),
            model: "model".to_owned(),
            provider: None,
            api_key: Some("key".to_owned()),
            allow_unsafe_agent_harness: true,
            retry_policy: RetryPolicy::default(),
            command_timeout: Duration::from_secs(1),
        })
        .unwrap();
        assert!(matches!(disabled, SummaryClient::Disabled));
        assert!(disabled.summarize("prompt", None).is_err());

        let cli = build_summary_client(SummaryClientConfig {
            harness: SummaryHarness::Claude,
            command_path: "/bin/claude".to_owned(),
            model: "haiku".to_owned(),
            provider: None,
            api_key: None,
            allow_unsafe_agent_harness: true,
            retry_policy: RetryPolicy::default(),
            command_timeout: Duration::from_secs(1),
        })
        .unwrap();
        assert!(matches!(cli, SummaryClient::Cli(_)));
    }

    #[test]
    fn summary_client_config_debug_redacts_api_key() {
        let config = SummaryClientConfig {
            harness: SummaryHarness::Native,
            command_path: String::new(),
            model: "model".to_owned(),
            provider: Some(SummaryProvider::OpenCodeGo),
            api_key: Some("super-secret-key".to_owned()),
            allow_unsafe_agent_harness: true,
            retry_policy: RetryPolicy::default(),
            command_timeout: Duration::from_secs(1),
        };
        let rendered = format!("{config:?}");
        assert!(rendered.contains("[redacted]"));
        assert!(!rendered.contains("super-secret-key"));
    }

    #[test]
    fn tool_fs_settle_waits_for_started_tool_tasks() {
        let root = fresh_workdir();
        let fs = Arc::new(AgentToolFs::new(&root).unwrap());
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            use std::sync::atomic::AtomicBool;
            let done = Arc::new(AtomicBool::new(false));
            let flag = Arc::clone(&done);
            let task_fs = Arc::clone(&fs);
            let tool = tokio::spawn(async move {
                let _ = task_fs
                    .spawn_tool(move || {
                        std::thread::sleep(std::time::Duration::from_millis(50));
                        flag.store(true, Ordering::Relaxed);
                        Ok(())
                    })
                    .await;
            });
            // Let the tool task start, then drop its future the way an
            // agent timeout does. The blocking body keeps running.
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            tool.abort();
            fs.settle().await;
            assert!(done.load(Ordering::Relaxed));
        });
    }

    #[test]
    fn tool_fs_drop_joins_started_tool_tasks() {
        let root = fresh_workdir();
        let fs = Arc::new(AgentToolFs::new(&root).unwrap());
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let done = {
            use std::sync::atomic::AtomicBool;
            let done = Arc::new(AtomicBool::new(false));
            rt.block_on(async {
                let flag = Arc::clone(&done);
                let task_fs = Arc::clone(&fs);
                let tool = tokio::spawn(async move {
                    let _ = task_fs
                        .spawn_tool(move || {
                            std::thread::sleep(std::time::Duration::from_millis(50));
                            flag.store(true, Ordering::Relaxed);
                            Ok(())
                        })
                        .await;
                });
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                tool.abort();
                let _ = tool.await; // ensure the aborted future released its Arc<AgentToolFs>
            });
            done
        };
        // Dropping the fs while a tool task still runs must wait for it —
        // and not hang (the task holds only a ToolFsInner snapshot).
        drop(fs);
        assert!(done.load(Ordering::Relaxed));
    }
}
