use crate::application::summary::{
    AgentOutputContract, ClaudeSummaryClient, SUMMARY_OUTPUT_CONTRACT, SummaryError,
};
use crate::bootstrap::config::SummaryHarness;
use crate::infrastructure::asr::{
    WhisperClient, WhisperInferenceRequest, WhisperParseError, WhisperTranscriptionResult,
    parse_whisper_response,
};
use crate::infrastructure::retry::{RetryPolicy, retry_with_backoff, retry_with_backoff_if};
use crate::infrastructure::workspace::{
    AGENT_CURSOR_CONFIG_FILENAME, AGENT_CURSOR_DIR, AGENT_INPUT_DIR, AGENT_OUTPUT_DIR,
};
use std::fmt::{Display, Formatter};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::Path;
use std::process::{Command, Output, Stdio};
use std::sync::OnceLock;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

pub const DEFAULT_COMMAND_TIMEOUT: Duration = Duration::from_secs(10 * 60);
const DEFAULT_COMMAND_OUTPUT_STREAM_MAX_BYTES: u64 = 16 * 1024 * 1024;
const COMMAND_OUTPUT_POLL_INTERVAL: Duration = Duration::from_millis(20);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandOutputStream {
    Stdout,
    Stderr,
}

impl Display for CommandOutputStream {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Stdout => write!(f, "stdout"),
            Self::Stderr => write!(f, "stderr"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IntegrationError {
    Io(String),
    NonZeroExit {
        code: i32,
        stderr: String,
    },
    Timeout {
        timeout: Duration,
    },
    OutputTooLarge {
        stream: CommandOutputStream,
        limit: u64,
    },
    InvalidUtf8,
    Parse(String),
}

impl Display for IntegrationError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(err) => write!(f, "io error: {err}"),
            Self::NonZeroExit { code, stderr } => {
                write!(f, "command exited with code {code}: {stderr}")
            }
            Self::Timeout { timeout } => {
                write!(f, "command timed out after {} seconds", timeout.as_secs())
            }
            Self::OutputTooLarge { stream, limit } => {
                write!(f, "command {stream} exceeded {limit} bytes")
            }
            Self::InvalidUtf8 => write!(f, "invalid utf8 output from command"),
            Self::Parse(err) => write!(f, "parse error: {err}"),
        }
    }
}

impl std::error::Error for IntegrationError {}

#[derive(Clone, PartialEq)]
pub struct CommandWhisperClient {
    pub endpoint: String,
    pub curl_bin: String,
    pub retry_policy: RetryPolicy,
    pub beam_size: u32,
    pub suppress_non_speech: bool,
    pub prompt: Option<String>,
    pub vad: bool,
    pub temperature: f32,
    pub command_timeout: Duration,
}

impl std::fmt::Debug for CommandWhisperClient {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CommandWhisperClient")
            .field("endpoint", &self.endpoint)
            .field("curl_bin", &self.curl_bin)
            .field("retry_policy", &self.retry_policy)
            .field("beam_size", &self.beam_size)
            .field("suppress_non_speech", &self.suppress_non_speech)
            .field("prompt", &self.prompt.as_ref().map(|_| "[REDACTED]"))
            .field("vad", &self.vad)
            .field("temperature", &self.temperature)
            .field("command_timeout", &self.command_timeout)
            .finish()
    }
}

impl WhisperClient for CommandWhisperClient {
    fn infer(
        &self,
        request: &WhisperInferenceRequest,
    ) -> Result<WhisperTranscriptionResult, WhisperParseError> {
        retry_with_backoff_if(
            self.retry_policy,
            |_| {
                let effective_prompt =
                    compose_whisper_prompt(self.prompt.as_deref(), request.prompt.as_deref());
                let command_for_log =
                    render_whisper_command_for_log(self, request, effective_prompt.as_deref());
                let mut cmd = Command::new(&self.curl_bin);
                // Keep this argument order in sync with `render_whisper_command_for_log`.
                cmd.arg("-sS")
                    .arg("-w")
                    .arg(r"\n%{http_code}")
                    .arg("-X")
                    .arg("POST")
                    .arg(whisper_inference_endpoint(&self.endpoint))
                    .arg("-F")
                    .arg(format!("file=@{}", request.audio_path))
                    .arg("-F")
                    .arg("response_format=verbose_json");

                if let Some(language) = &request.language {
                    cmd.arg("-F").arg(format!("language={language}"));
                }
                cmd.arg("-F").arg(format!("beam_size={}", self.beam_size));
                cmd.arg("-F")
                    .arg(format!("suppress_non_speech={}", self.suppress_non_speech));
                if let Some(p) = &effective_prompt {
                    cmd.arg("--form-string").arg(format!("prompt={p}"));
                }
                cmd.arg("-F").arg(format!("vad={}", self.vad));
                cmd.arg("-F")
                    .arg(format!("temperature={}", self.temperature));

                run_whisper_command(&mut cmd, &command_for_log, self.command_timeout)
            },
            |err| err.is_retriable(),
        )
    }
}

struct WhisperCurlResponse {
    status: Option<u16>,
    body: Vec<u8>,
}

fn run_whisper_command(
    command: &mut Command,
    command_for_log: &str,
    timeout: Duration,
) -> Result<WhisperTranscriptionResult, WhisperParseError> {
    let output = run_command_with_timeout(command, None, timeout)
        .map_err(|err| whisper_command_integration_error(command_for_log, err))?;
    let response = split_curl_status_trailer(output.stdout);

    if let Some(status) = response.status {
        if !(200..=299).contains(&status) {
            return Err(whisper_http_status_error(
                command_for_log,
                output.status.code(),
                Some(status),
                &output.stderr,
                &response.body,
            ));
        }
        if !output.status.success() {
            return Err(whisper_transport_error(
                command_for_log,
                output.status.code(),
                Some(status),
                &output.stderr,
                &response.body,
            ));
        }
    } else if !output.status.success() {
        return Err(whisper_http_status_error(
            command_for_log,
            output.status.code(),
            None,
            &output.stderr,
            &response.body,
        ));
    }

    parse_successful_whisper_body(response.body)
}

fn whisper_command_integration_error(
    command_for_log: &str,
    err: IntegrationError,
) -> WhisperParseError {
    let prefix = if matches!(err, IntegrationError::OutputTooLarge { .. }) {
        "non-retriable whisper command failed before execution"
    } else {
        "whisper command failed before execution"
    };
    WhisperParseError::InvalidJson(format!("{prefix}: command={command_for_log}, error={err}"))
}

fn whisper_http_status_error(
    command_for_log: &str,
    exit_status: Option<i32>,
    http_status: Option<u16>,
    stderr: &[u8],
    body: &[u8],
) -> WhisperParseError {
    let status = http_status
        .map(|status| status.to_string())
        .unwrap_or_else(|| "unknown".to_owned());
    WhisperParseError::InvalidJson(format!(
        "whisper command failed: command={command_for_log}, exit_status={exit_status:?}, http_status={status}, stderr={}, body={}",
        sanitize_output(stderr),
        sanitize_output(body)
    ))
}

fn whisper_transport_error(
    command_for_log: &str,
    exit_status: Option<i32>,
    http_code: Option<u16>,
    stderr: &[u8],
    body: &[u8],
) -> WhisperParseError {
    let http_code = http_code
        .map(|status| status.to_string())
        .unwrap_or_else(|| "unknown".to_owned());
    WhisperParseError::InvalidJson(format!(
        "retriable whisper transport failure: command={command_for_log}, exit_status={exit_status:?}, http_code={http_code}, stderr={}, body={}",
        sanitize_output(stderr),
        sanitize_output(body)
    ))
}

fn parse_successful_whisper_body(
    body: Vec<u8>,
) -> Result<WhisperTranscriptionResult, WhisperParseError> {
    let body_len = body.len();
    let body = String::from_utf8(body).map_err(|err| {
        WhisperParseError::InvalidJson(format!(
            "malformed successful whisper response: invalid UTF-8: {err} (response body length: {body_len} bytes)"
        ))
    })?;
    parse_whisper_response(&body).map_err(|err| match err {
        WhisperParseError::InvalidJson(message) => WhisperParseError::InvalidJson(format!(
            "malformed successful whisper response: {message} (response body length: {} bytes)",
            body.len()
        )),
        other => other,
    })
}

fn split_curl_status_trailer(mut stdout: Vec<u8>) -> WhisperCurlResponse {
    let Some(newline_index) = stdout.iter().rposition(|byte| *byte == b'\n') else {
        return WhisperCurlResponse {
            status: None,
            body: stdout,
        };
    };
    let status_bytes = &stdout[newline_index + 1..];
    if status_bytes.len() != 3 || !status_bytes.iter().all(u8::is_ascii_digit) {
        return WhisperCurlResponse {
            status: None,
            body: stdout,
        };
    }

    let status = status_bytes
        .iter()
        .fold(0u16, |acc, byte| acc * 10 + u16::from(byte - b'0'));
    stdout.truncate(newline_index);
    WhisperCurlResponse {
        status: Some(status),
        body: stdout,
    }
}

fn compose_whisper_prompt(
    configured_prompt: Option<&str>,
    request_prompt: Option<&str>,
) -> Option<String> {
    let mut parts = Vec::new();
    if let Some(prompt) = configured_prompt.map(str::trim)
        && !prompt.is_empty()
    {
        parts.push(prompt.to_owned());
    }
    if let Some(prompt) = request_prompt.map(str::trim)
        && !prompt.is_empty()
    {
        parts.push(prompt.to_owned());
    }
    if parts.is_empty() {
        None
    } else {
        Some(parts.join("\n"))
    }
}

fn render_whisper_command_for_log(
    client: &CommandWhisperClient,
    request: &WhisperInferenceRequest,
    effective_prompt: Option<&str>,
) -> String {
    // Keep this redacted representation in sync with the curl arguments built in `infer`.
    let mut parts = vec![
        client.curl_bin.clone(),
        "-sS".to_owned(),
        "-w".to_owned(),
        r"\n%{http_code}".to_owned(),
        "-X".to_owned(),
        "POST".to_owned(),
        sanitize_whisper_endpoint_for_log(&whisper_inference_endpoint(&client.endpoint)),
        "-F".to_owned(),
        format!("file=@{}", request.audio_path),
        "-F".to_owned(),
        "response_format=verbose_json".to_owned(),
    ];
    if let Some(language) = &request.language {
        parts.push("-F".to_owned());
        parts.push(format!("language={language}"));
    }
    parts.push("-F".to_owned());
    parts.push(format!("beam_size={}", client.beam_size));
    parts.push("-F".to_owned());
    parts.push(format!(
        "suppress_non_speech={}",
        client.suppress_non_speech
    ));
    if effective_prompt.is_some() {
        parts.push("--form-string".to_owned());
        parts.push("prompt=[REDACTED]".to_owned());
    }
    parts.push("-F".to_owned());
    parts.push(format!("vad={}", client.vad));
    parts.push("-F".to_owned());
    parts.push(format!("temperature={}", client.temperature));
    parts
        .iter()
        .map(|part| quote_log_arg(part))
        .collect::<Vec<_>>()
        .join(" ")
}

fn whisper_inference_endpoint(endpoint: &str) -> String {
    let Ok(mut url) = reqwest::Url::parse(endpoint) else {
        return format!("{}/inference", endpoint.trim_end_matches('/'));
    };

    let path = url.path().trim_end_matches('/');
    let inference_path = if path.is_empty() {
        "/inference".to_owned()
    } else {
        format!("{path}/inference")
    };
    url.set_path(&inference_path);
    url.to_string()
}

fn sanitize_whisper_endpoint_for_log(endpoint: &str) -> String {
    let Ok(mut url) = reqwest::Url::parse(endpoint) else {
        return "[REDACTED_INVALID_ENDPOINT_URL]".to_owned();
    };

    if !url.username().is_empty() {
        let _ = url.set_username("REDACTED");
    }
    if url.password().is_some() {
        let _ = url.set_password(Some("REDACTED"));
    }

    let query_pairs = url
        .query_pairs()
        .map(|(name, value)| {
            let value = if is_sensitive_query_name(&name) {
                "REDACTED".into()
            } else {
                value
            };
            (name.into_owned(), value.into_owned())
        })
        .collect::<Vec<_>>();

    if !query_pairs.is_empty() {
        url.query_pairs_mut().clear().extend_pairs(query_pairs);
    }

    url.to_string()
}

fn is_sensitive_query_name(name: &str) -> bool {
    let normalized = name
        .to_ascii_lowercase()
        .chars()
        .map(|ch| if ch.is_ascii_alphanumeric() { ch } else { '_' })
        .collect::<String>();
    let compact = normalized.replace('_', "");
    let exact_sensitive_names = [
        "api_key",
        "apikey",
        "access_token",
        "token",
        "secret",
        "password",
        "passwd",
        "pwd",
        "credential",
        "credentials",
        "authorization",
        "auth",
        "signature",
        "sig",
        "client_secret",
        "subscription_key",
        "key",
    ];
    if exact_sensitive_names
        .iter()
        .any(|sensitive| normalized == *sensitive || compact == *sensitive)
    {
        return true;
    }

    normalized
        .split('_')
        .filter(|token| !token.is_empty())
        .any(|token| {
            matches!(
                token,
                "token"
                    | "secret"
                    | "password"
                    | "passwd"
                    | "pwd"
                    | "credential"
                    | "credentials"
                    | "authorization"
                    | "auth"
                    | "signature"
                    | "sig"
                    | "key"
            )
        })
}

fn quote_log_arg(part: &str) -> String {
    if part.contains(char::is_whitespace) || part.contains('"') || part.contains('\\') {
        format!("\"{}\"", part.replace('\\', "\\\\").replace('"', "\\\""))
    } else {
        part.to_owned()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HarnessCliSummaryClient {
    pub harness: SummaryHarness,
    pub command_path: String,
    pub model: String,
    pub allow_unsafe_agent_harness: bool,
    pub retry_policy: RetryPolicy,
    pub command_timeout: Duration,
}

const SANITIZE_MAX_LEN: usize = 500;
/// Redact values that look like API keys or tokens, collapse whitespace,
/// and truncate to a bounded length so error messages stay safe and compact.
fn sanitize_output(raw: &[u8]) -> String {
    use std::fmt::Write;

    let lossy = String::from_utf8_lossy(raw);
    // Collapse runs of whitespace (including newlines) into a single space.
    let collapsed: String = lossy.split_whitespace().collect::<Vec<_>>().join(" ");
    // Redact strings that look like API keys / bearer tokens.
    static REDACT_RE: OnceLock<regex::Regex> = OnceLock::new();
    let re = REDACT_RE.get_or_init(|| {
        regex::Regex::new(r"(?i)(sk-[a-zA-Z0-9\-_]{8,}|key-[a-zA-Z0-9]{8,}|bearer\s+\S{8,})")
            .expect("hardcoded redaction regex is valid")
    });
    let redacted = re.replace_all(&collapsed, "[REDACTED]").into_owned();
    let redacted = redact_prompt_values(&redacted);

    if redacted.len() <= SANITIZE_MAX_LEN {
        return redacted;
    }
    let mut truncated: String = redacted.chars().take(SANITIZE_MAX_LEN).collect();
    let omitted = redacted.len() - truncated.len();
    let _ = write!(truncated, "... ({omitted} bytes omitted)");
    truncated
}

fn redact_prompt_values(value: &str) -> String {
    let Some(index) = value.to_ascii_lowercase().find("prompt=") else {
        return value.to_owned();
    };
    const REDACTED_SUFFIX: &str = "prompt=[REDACTED]... (truncated after prompt)";
    let mut output = String::with_capacity(index + REDACTED_SUFFIX.len());
    output.push_str(&value[..index]);
    output.push_str(REDACTED_SUFFIX);
    output
}

impl HarnessCliSummaryClient {
    /// Full-transcript correction requires a non-agent text boundary; CLI harnesses are disabled.
    pub fn can_run_llm_transcript_correction(&self) -> bool {
        false
    }

    fn ensure_can_run_summary_harness(&self) -> Result<(), SummaryError> {
        if !self.allow_unsafe_agent_harness {
            return Err(SummaryError::SummaryEngine(format!(
                "refusing to run unsafe CLI summary harness `{}` over untrusted transcript data without SUMMARY_ALLOW_UNSAFE_AGENT_HARNESS=true",
                self.harness
            )));
        }
        Ok(())
    }
}

impl ClaudeSummaryClient for HarnessCliSummaryClient {
    fn supports_transcript_correction(&self) -> bool {
        self.can_run_llm_transcript_correction()
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
            return Err(SummaryError::SummaryEngine(format!(
                "refusing to run CLI summary harness `{}` over untrusted transcript/context data without explicit unsafe agent harness opt-in",
                self.harness
            )));
        }
        run_agent_harness_with_output_contract(self, prompt, workdir, output)
    }
}

fn run_agent_harness_with_output_contract(
    client: &HarnessCliSummaryClient,
    prompt: &str,
    workdir: Option<&Path>,
    output: AgentOutputContract,
) -> Result<String, SummaryError> {
    client.ensure_can_run_summary_harness()?;
    retry_with_backoff(client.retry_policy, |_| match client.harness {
        SummaryHarness::Claude => summarize_claude_stdin(client, prompt, workdir, output),
        SummaryHarness::OpenCode => summarize_opencode_argv(client, prompt, workdir, output),
        SummaryHarness::CursorAgent => summarize_cursor_argv(client, prompt, workdir, output),
    })
}

fn summarize_claude_stdin(
    client: &HarnessCliSummaryClient,
    prompt: &str,
    workdir: Option<&Path>,
    output_contract: AgentOutputContract,
) -> Result<String, SummaryError> {
    let workdir = require_agent_workdir(workdir)?;
    remove_stale_agent_output(workdir, output_contract)?;
    let mut command = Command::new(&client.command_path);
    command.arg("--model").arg(&client.model).arg("-p");
    scrub_agent_command_environment(&mut command);
    command.current_dir(workdir);
    let output = run_command_with_timeout(
        &mut command,
        Some(prompt.as_bytes()),
        client.command_timeout,
    )
    .map_err(summary_integration_error)?;

    if !output.status.success() {
        return Err(summary_command_failed(
            client.harness,
            &output.status,
            &output.stderr,
            &output.stdout,
        ));
    }

    read_validated_agent_output(
        client.harness,
        workdir,
        output_contract,
        &output.stderr,
        &output.stdout,
    )
}

fn summarize_opencode_argv(
    client: &HarnessCliSummaryClient,
    prompt: &str,
    workdir: Option<&Path>,
    output_contract: AgentOutputContract,
) -> Result<String, SummaryError> {
    let workdir = require_agent_workdir(workdir)?;
    remove_stale_agent_output(workdir, output_contract)?;
    let mut command = Command::new(&client.command_path);
    command
        .arg("run")
        .arg("--model")
        .arg(&client.model)
        .arg(prompt)
        .stdin(std::process::Stdio::null());
    scrub_agent_command_environment(&mut command);
    command.current_dir(workdir);
    let output = run_command_with_timeout(&mut command, None, client.command_timeout)
        .map_err(summary_integration_error)?;
    if !output.status.success() {
        return Err(summary_command_failed(
            client.harness,
            &output.status,
            &output.stderr,
            &output.stdout,
        ));
    }
    read_validated_agent_output(
        client.harness,
        workdir,
        output_contract,
        &output.stderr,
        &output.stdout,
    )
}

fn summarize_cursor_argv(
    client: &HarnessCliSummaryClient,
    prompt: &str,
    workdir: Option<&Path>,
    output_contract: AgentOutputContract,
) -> Result<String, SummaryError> {
    let workdir = require_agent_workdir(workdir)?;
    remove_stale_agent_output(workdir, output_contract)?;
    let mut command = Command::new(&client.command_path);
    command
        .arg("-p")
        .arg("--trust")
        .arg(prompt)
        .arg("--output-format")
        .arg("text");
    if !client.model.trim().is_empty() {
        command.arg("--model").arg(&client.model);
    }
    command.stdin(std::process::Stdio::null());
    scrub_agent_command_environment(&mut command);
    command.current_dir(workdir);
    let output = run_command_with_timeout(&mut command, None, client.command_timeout)
        .map_err(summary_integration_error)?;
    if !output.status.success() {
        return Err(summary_command_failed(
            client.harness,
            &output.status,
            &output.stderr,
            &output.stdout,
        ));
    }
    read_validated_agent_output(
        client.harness,
        workdir,
        output_contract,
        &output.stderr,
        &output.stdout,
    )
}

fn require_agent_workdir(workdir: Option<&Path>) -> Result<&Path, SummaryError> {
    let workdir = workdir.ok_or_else(|| {
        SummaryError::SummaryEngine("summary harness: workdir not provided".to_owned())
    })?;
    if !workdir.join(AGENT_INPUT_DIR).is_dir()
        || !workdir.join(AGENT_OUTPUT_DIR).is_dir()
        || !workdir
            .join(AGENT_CURSOR_DIR)
            .join(AGENT_CURSOR_CONFIG_FILENAME)
            .is_file()
    {
        return Err(SummaryError::SummaryEngine(
            "summary harness: workdir missing expected agent workspace markers (input/, output/, .cursor/cli.json)".to_owned(),
        ));
    }
    Ok(workdir)
}

fn agent_output_path(
    workdir: &Path,
    output_contract: AgentOutputContract,
) -> Result<std::path::PathBuf, SummaryError> {
    let relative_path = Path::new(output_contract.relative_path);
    if relative_path.is_absolute()
        || !relative_path.starts_with(AGENT_OUTPUT_DIR)
        || relative_path
            .components()
            .any(|component| matches!(component, std::path::Component::ParentDir))
    {
        return Err(SummaryError::SummaryEngine(format!(
            "invalid {} path {}: expected relative output path under {AGENT_OUTPUT_DIR}/",
            output_contract.label, output_contract.relative_path
        )));
    }
    Ok(workdir.join(relative_path))
}

fn remove_stale_agent_output(
    workdir: &Path,
    output_contract: AgentOutputContract,
) -> Result<(), SummaryError> {
    let path = agent_output_path(workdir, output_contract)?;
    match fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(SummaryError::SummaryEngine(format!(
            "failed to remove stale {} {}: {err}",
            output_contract.label, output_contract.relative_path
        ))),
    }
}

fn read_validated_agent_output(
    harness: SummaryHarness,
    workdir: &Path,
    output_contract: AgentOutputContract,
    stderr: &[u8],
    stdout: &[u8],
) -> Result<String, SummaryError> {
    let path = agent_output_path(workdir, output_contract)?;
    let metadata = fs::symlink_metadata(&path).map_err(|err| {
        agent_output_validation_failed(
            harness,
            format!(
                "missing {} file {}: {err}",
                output_contract.label, output_contract.relative_path
            ),
            output_contract,
            stderr,
            stdout,
        )
    })?;
    if !metadata.file_type().is_file() {
        return Err(agent_output_validation_failed(
            harness,
            format!(
                "invalid {} file {}: expected a regular file",
                output_contract.label, output_contract.relative_path
            ),
            output_contract,
            stderr,
            stdout,
        ));
    }
    if metadata.len() > output_contract.max_bytes {
        return Err(agent_output_validation_failed(
            harness,
            format!(
                "oversized {} file {}: {} bytes exceeds {} bytes",
                output_contract.label,
                output_contract.relative_path,
                metadata.len(),
                output_contract.max_bytes
            ),
            output_contract,
            stderr,
            stdout,
        ));
    }
    let bytes = fs::read(&path).map_err(|err| {
        agent_output_validation_failed(
            harness,
            format!(
                "failed to read {} file {}: {err}",
                output_contract.label, output_contract.relative_path
            ),
            output_contract,
            stderr,
            stdout,
        )
    })?;
    let markdown = String::from_utf8(bytes).map_err(|err| {
        agent_output_validation_failed(
            harness,
            format!(
                "invalid {} file {}: not valid UTF-8 ({err})",
                output_contract.label, output_contract.relative_path
            ),
            output_contract,
            stderr,
            stdout,
        )
    })?;
    if markdown.trim().is_empty() {
        return Err(agent_output_validation_failed(
            harness,
            format!(
                "empty {} file {}",
                output_contract.label, output_contract.relative_path
            ),
            output_contract,
            stderr,
            stdout,
        ));
    }
    Ok(markdown)
}

fn agent_output_validation_failed(
    harness: SummaryHarness,
    reason: String,
    output_contract: AgentOutputContract,
    stderr: &[u8],
    stdout: &[u8],
) -> SummaryError {
    SummaryError::SummaryEngine(format!(
        "{} validation failed (harness={harness}): {reason}; stderr={}; stdout={}",
        output_contract.label,
        sanitize_output(stderr),
        sanitize_output(stdout)
    ))
}

fn scrub_agent_command_environment(command: &mut Command) {
    for (key, _) in std::env::vars() {
        if is_sensitive_env_key(&key) {
            command.env_remove(key);
        }
    }
}

fn is_sensitive_env_key(key: &str) -> bool {
    let upper = key.to_ascii_uppercase();
    upper.contains("TOKEN")
        || upper.contains("SECRET")
        || upper.contains("PASSWORD")
        || upper.ends_with("_KEY")
        || upper.contains("API_KEY")
        || matches!(
            upper.as_str(),
            "DATABASE_URL"
                | "DISCORD_CLIENT_SECRET"
                | "WEB_SESSION_SECRET"
                | "GUILD_BOT_TOKEN_ENCRYPTION_KEY"
                | "OPERATIONAL_METRICS_BEARER_TOKEN"
        )
}

fn summary_command_failed(
    harness: SummaryHarness,
    status: &std::process::ExitStatus,
    stderr: &[u8],
    stdout: &[u8],
) -> SummaryError {
    SummaryError::SummaryEngine(format!(
        "summary command failed (harness={harness}): status={status:?}, stderr={}, stdout={}",
        sanitize_output(stderr),
        sanitize_output(stdout)
    ))
}

fn summary_integration_error(err: IntegrationError) -> SummaryError {
    SummaryError::SummaryEngine(err.to_string())
}

pub fn run_command_with_timeout(
    command: &mut Command,
    stdin_input: Option<&[u8]>,
    timeout: Duration,
) -> Result<Output, IntegrationError> {
    run_command_with_timeout_and_output_limit(
        command,
        stdin_input,
        timeout,
        DEFAULT_COMMAND_OUTPUT_STREAM_MAX_BYTES,
    )
}

fn run_command_with_timeout_and_output_limit(
    command: &mut Command,
    stdin_input: Option<&[u8]>,
    timeout: Duration,
    output_stream_max_bytes: u64,
) -> Result<Output, IntegrationError> {
    let temp_paths = CommandOutputTempPaths::new();
    let stdout_file = temp_paths
        .create_stdout()
        .map_err(|err| IntegrationError::Io(err.to_string()))?;
    let stderr_file = match temp_paths.create_stderr() {
        Ok(file) => file,
        Err(err) => {
            drop(stdout_file);
            temp_paths.cleanup();
            return Err(IntegrationError::Io(err.to_string()));
        }
    };
    let stdin_file = match stdin_input {
        Some(input) => match temp_paths.create_stdin(input) {
            Ok(file) => Some(file),
            Err(err) => {
                drop(stdout_file);
                drop(stderr_file);
                temp_paths.cleanup();
                return Err(IntegrationError::Io(err.to_string()));
            }
        },
        None => None,
    };

    let stdout_clone = match stdout_file.try_clone() {
        Ok(file) => file,
        Err(err) => {
            drop(stdin_file);
            drop(stdout_file);
            drop(stderr_file);
            temp_paths.cleanup();
            return Err(IntegrationError::Io(err.to_string()));
        }
    };
    let stderr_clone = match stderr_file.try_clone() {
        Ok(file) => file,
        Err(err) => {
            drop(stdin_file);
            drop(stdout_file);
            drop(stderr_file);
            temp_paths.cleanup();
            return Err(IntegrationError::Io(err.to_string()));
        }
    };

    command
        .stdout(Stdio::from(stdout_clone))
        .stderr(Stdio::from(stderr_clone));
    if let Some(stdin_file) = stdin_file {
        command.stdin(Stdio::from(stdin_file));
    }
    configure_child_process_group(command);

    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(err) => {
            release_command_stdio(command);
            drop(stdout_file);
            drop(stderr_file);
            temp_paths.cleanup();
            return Err(IntegrationError::Io(err.to_string()));
        }
    };
    release_command_stdio(command);

    let started = Instant::now();
    let status = loop {
        match temp_paths.output_stream_exceeding_limit(output_stream_max_bytes) {
            Ok(Some(stream)) => {
                kill_child_process_group(&mut child);
                let _ = child.wait();
                drop(stdout_file);
                drop(stderr_file);
                temp_paths.cleanup();
                return Err(IntegrationError::OutputTooLarge {
                    stream,
                    limit: output_stream_max_bytes,
                });
            }
            Ok(None) => {}
            Err(err) => {
                kill_child_process_group(&mut child);
                let _ = child.wait();
                drop(stdout_file);
                drop(stderr_file);
                temp_paths.cleanup();
                return Err(IntegrationError::Io(err.to_string()));
            }
        }
        match child.try_wait() {
            Err(err) => {
                kill_child_process_group(&mut child);
                let _ = child.wait();
                drop(stdout_file);
                drop(stderr_file);
                temp_paths.cleanup();
                return Err(IntegrationError::Io(err.to_string()));
            }
            Ok(Some(status)) => break status,
            Ok(None) if started.elapsed() >= timeout => {
                kill_child_process_group(&mut child);
                let _ = child.wait();
                drop(stdout_file);
                drop(stderr_file);
                temp_paths.cleanup();
                return Err(IntegrationError::Timeout { timeout });
            }
            Ok(None) => std::thread::sleep(COMMAND_OUTPUT_POLL_INTERVAL),
        }
    };

    drop(stdout_file);
    drop(stderr_file);
    let stdout = match temp_paths.read_stdout(output_stream_max_bytes) {
        Ok(stdout) => stdout,
        Err(CommandOutputReadError::TooLarge) => {
            kill_child_process_group(&mut child);
            let _ = child.wait();
            temp_paths.cleanup();
            return Err(IntegrationError::OutputTooLarge {
                stream: CommandOutputStream::Stdout,
                limit: output_stream_max_bytes,
            });
        }
        Err(err) => {
            temp_paths.cleanup();
            return Err(IntegrationError::Io(err.into_io_error().to_string()));
        }
    };
    let stderr = match temp_paths.read_stderr(output_stream_max_bytes) {
        Ok(stderr) => stderr,
        Err(CommandOutputReadError::TooLarge) => {
            kill_child_process_group(&mut child);
            let _ = child.wait();
            temp_paths.cleanup();
            return Err(IntegrationError::OutputTooLarge {
                stream: CommandOutputStream::Stderr,
                limit: output_stream_max_bytes,
            });
        }
        Err(err) => {
            temp_paths.cleanup();
            return Err(IntegrationError::Io(err.into_io_error().to_string()));
        }
    };
    temp_paths.cleanup();

    Ok(Output {
        status,
        stdout,
        stderr,
    })
}

fn release_command_stdio(command: &mut Command) {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
}

#[cfg(unix)]
fn configure_child_process_group(command: &mut Command) {
    use std::os::unix::process::CommandExt;

    unsafe {
        command.pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
}

#[cfg(not(unix))]
fn configure_child_process_group(_command: &mut Command) {}

#[cfg(unix)]
fn kill_child_process_group(child: &mut std::process::Child) {
    let pgid = child.id() as i32;
    unsafe {
        libc::kill(-pgid, libc::SIGKILL);
    }
    let _ = child.kill();
}

#[cfg(not(unix))]
fn kill_child_process_group(child: &mut std::process::Child) {
    let _ = child.kill();
}

struct CommandOutputTempPaths {
    stdin: std::path::PathBuf,
    stdout: std::path::PathBuf,
    stderr: std::path::PathBuf,
}

impl CommandOutputTempPaths {
    fn new() -> Self {
        use std::sync::atomic::{AtomicU64, Ordering};

        static CALL_COUNTER: AtomicU64 = AtomicU64::new(0);
        let unique = format!(
            "discord_transcript_command_{}_{}_{}",
            std::process::id(),
            CALL_COUNTER.fetch_add(1, Ordering::Relaxed),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        );
        let base = std::env::temp_dir();
        Self {
            stdin: base.join(format!("{unique}.stdin")),
            stdout: base.join(format!("{unique}.stdout")),
            stderr: base.join(format!("{unique}.stderr")),
        }
    }

    fn create_stdin(&self, input: &[u8]) -> std::io::Result<File> {
        let mut file = create_temp_output_file(&self.stdin)?;
        file.write_all(input)?;
        drop(file);
        File::open(&self.stdin)
    }

    fn create_stdout(&self) -> std::io::Result<File> {
        create_temp_output_file(&self.stdout)
    }

    fn create_stderr(&self) -> std::io::Result<File> {
        create_temp_output_file(&self.stderr)
    }

    fn read_stdout(&self, max_bytes: u64) -> Result<Vec<u8>, CommandOutputReadError> {
        read_temp_output_file(&self.stdout, max_bytes)
    }

    fn read_stderr(&self, max_bytes: u64) -> Result<Vec<u8>, CommandOutputReadError> {
        read_temp_output_file(&self.stderr, max_bytes)
    }

    fn output_stream_exceeding_limit(
        &self,
        max_bytes: u64,
    ) -> std::io::Result<Option<CommandOutputStream>> {
        for (stream, path) in [
            (CommandOutputStream::Stdout, &self.stdout),
            (CommandOutputStream::Stderr, &self.stderr),
        ] {
            if fs::metadata(path)?.len() > max_bytes {
                return Ok(Some(stream));
            }
        }
        Ok(None)
    }

    fn cleanup(&self) {
        let _ = std::fs::remove_file(&self.stdin);
        let _ = std::fs::remove_file(&self.stdout);
        let _ = std::fs::remove_file(&self.stderr);
    }
}

fn create_temp_output_file(path: &Path) -> std::io::Result<File> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)
}

enum CommandOutputReadError {
    Io(std::io::Error),
    TooLarge,
}

impl CommandOutputReadError {
    fn into_io_error(self) -> std::io::Error {
        match self {
            Self::Io(err) => err,
            Self::TooLarge => std::io::Error::other("command output exceeded byte limit"),
        }
    }
}

impl From<std::io::Error> for CommandOutputReadError {
    fn from(value: std::io::Error) -> Self {
        Self::Io(value)
    }
}

fn read_temp_output_file(path: &Path, max_bytes: u64) -> Result<Vec<u8>, CommandOutputReadError> {
    let mut file = File::open(path)?;
    let mut bytes = Vec::new();
    Read::by_ref(&mut file)
        .take(max_bytes.saturating_add(1))
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > max_bytes {
        return Err(CommandOutputReadError::TooLarge);
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::{
        AGENT_CURSOR_CONFIG_FILENAME, AGENT_CURSOR_DIR, AGENT_INPUT_DIR, AGENT_OUTPUT_DIR,
        AgentOutputContract, CommandOutputReadError, CommandOutputStream, CommandWhisperClient,
        HarnessCliSummaryClient, IntegrationError, create_temp_output_file, read_temp_output_file,
        run_agent_harness_with_output_contract, run_command_with_timeout,
        run_command_with_timeout_and_output_limit, sanitize_whisper_endpoint_for_log,
    };
    use crate::application::summary::{ClaudeSummaryClient, SUMMARY_OUTPUT_CONTRACT};
    use crate::bootstrap::config::SummaryHarness;
    use crate::infrastructure::asr::{WhisperClient, WhisperInferenceRequest, WhisperParseError};
    use crate::infrastructure::retry::RetryPolicy;
    use std::io::Write;
    use std::process::Command;
    use std::time::{Duration, Instant};

    #[test]
    fn command_runner_times_out_and_kills_hanging_command() {
        let mut command = Command::new("sh");
        command.arg("-c").arg("sleep 5");

        let started = Instant::now();
        let result = run_command_with_timeout(&mut command, None, Duration::from_millis(50));

        assert!(matches!(result, Err(IntegrationError::Timeout { .. })));
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "timeout should not wait for the child sleep to finish"
        );
    }

    #[test]
    fn command_runner_captures_stdout_stderr_and_stdin() {
        let mut command = Command::new("sh");
        command.arg("-c").arg("cat; echo err >&2");

        let output =
            run_command_with_timeout(&mut command, Some(b"hello\n"), Duration::from_secs(2))
                .expect("command should finish");

        assert!(output.status.success());
        assert_eq!(String::from_utf8(output.stdout).unwrap(), "hello\n");
        assert_eq!(String::from_utf8(output.stderr).unwrap(), "err\n");
    }

    #[test]
    fn command_runner_rejects_cap_plus_one_stdout_and_kills_command() {
        let mut command = Command::new("sh");
        command.arg("-c").arg("printf '12345678901234567'; sleep 5");

        let started = Instant::now();
        let result = run_command_with_timeout_and_output_limit(
            &mut command,
            None,
            Duration::from_secs(5),
            16,
        );

        assert!(matches!(
            result,
            Err(IntegrationError::OutputTooLarge {
                stream: CommandOutputStream::Stdout,
                limit: 16
            })
        ));
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "output cap should kill the command instead of waiting for sleep"
        );
    }

    #[test]
    fn command_runner_rejects_cap_plus_one_stderr_and_kills_command() {
        let mut command = Command::new("sh");
        command
            .arg("-c")
            .arg("printf '12345678901234567' >&2; sleep 5");

        let started = Instant::now();
        let result = run_command_with_timeout_and_output_limit(
            &mut command,
            None,
            Duration::from_secs(5),
            16,
        );

        assert!(matches!(
            result,
            Err(IntegrationError::OutputTooLarge {
                stream: CommandOutputStream::Stderr,
                limit: 16
            })
        ));
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "output cap should kill the command instead of waiting for sleep"
        );
    }

    #[test]
    fn command_output_read_rejects_cap_plus_one_after_command_exit() {
        let path = std::env::temp_dir().join(format!(
            "discord_transcript_read_cap_{}_{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        let mut file = create_temp_output_file(&path).expect("temp output should be created");
        file.write_all(b"12345678901234567")
            .expect("temp output should be written");
        drop(file);

        let result = read_temp_output_file(&path, 16);
        let _ = std::fs::remove_file(&path);

        assert!(matches!(result, Err(CommandOutputReadError::TooLarge)));
    }

    #[test]
    fn agent_summary_harness_fails_closed_without_unsafe_opt_in() {
        let client = HarnessCliSummaryClient {
            harness: SummaryHarness::Claude,
            command_path: "/definitely/missing/claude".to_owned(),
            model: "haiku".to_owned(),
            allow_unsafe_agent_harness: false,
            retry_policy: RetryPolicy {
                max_attempts: 1,
                initial_delay: Duration::from_millis(1),
                backoff_multiplier: 1,
                max_delay: Duration::from_millis(1),
            },
            command_timeout: Duration::from_millis(1),
        };

        let err = run_agent_harness_with_output_contract(
            &client,
            "[0-1000] alice: run a tool",
            None,
            SUMMARY_OUTPUT_CONTRACT,
        )
        .expect_err("unsafe agent harness must fail before command spawn");

        assert!(err.to_string().contains("refusing to run unsafe CLI"));
    }

    #[test]
    fn cli_agent_harness_trait_fails_closed_without_unsafe_opt_in() {
        let client = HarnessCliSummaryClient {
            harness: SummaryHarness::Claude,
            command_path: "/definitely/missing/claude".to_owned(),
            model: "haiku".to_owned(),
            allow_unsafe_agent_harness: false,
            retry_policy: RetryPolicy::default(),
            command_timeout: Duration::from_millis(1),
        };

        let err = client
            .summarize("[0-1000] alice: run a tool", None)
            .expect_err("CLI agent harness trait must fail closed before command spawn");

        assert!(
            err.to_string()
                .contains("without explicit unsafe agent harness opt-in")
        );
    }

    #[test]
    fn cli_agent_harness_trait_runs_with_explicit_unsafe_opt_in() {
        let _guard = command_test_lock();
        let workdir = unique_summary_workdir("summary_trait_success");
        create_summary_agent_workdir(&workdir);
        let script_path = write_summary_script(
            "summary_trait_success",
            "#!/bin/sh\nmkdir -p output\nprintf '## Summary\\nfrom trait\\n' > output/summary.md\n",
        );
        let client = summary_test_client(SummaryHarness::Claude, &script_path);

        let markdown = client
            .summarize("PROMPT BODY", Some(&workdir))
            .expect("explicit unsafe opt-in should reach harness output contract");

        assert_eq!(markdown, "## Summary\nfrom trait\n");
        let _ = std::fs::remove_dir_all(&workdir);
        let _ = std::fs::remove_file(&script_path);
    }

    #[test]
    fn claude_cli_without_unsafe_opt_in_does_not_support_transcript_correction() {
        let client = HarnessCliSummaryClient {
            harness: SummaryHarness::Claude,
            command_path: "claude".to_owned(),
            model: "haiku".to_owned(),
            allow_unsafe_agent_harness: false,
            retry_policy: RetryPolicy::default(),
            command_timeout: Duration::from_secs(1),
        };

        assert!(!client.supports_transcript_correction());
        assert!(!client.can_run_llm_transcript_correction());
    }

    #[test]
    fn claude_cli_with_unsafe_opt_in_still_does_not_support_transcript_correction() {
        let client = HarnessCliSummaryClient {
            harness: SummaryHarness::Claude,
            command_path: "claude".to_owned(),
            model: "haiku".to_owned(),
            allow_unsafe_agent_harness: true,
            retry_policy: RetryPolicy::default(),
            command_timeout: Duration::from_secs(1),
        };

        assert!(!client.supports_transcript_correction());
        assert!(!client.can_run_llm_transcript_correction());
    }

    #[test]
    fn cursor_agent_does_not_support_transcript_correction() {
        let client = HarnessCliSummaryClient {
            harness: SummaryHarness::CursorAgent,
            command_path: "cursor-agent".to_owned(),
            model: String::new(),
            allow_unsafe_agent_harness: true,
            retry_policy: RetryPolicy::default(),
            command_timeout: Duration::from_secs(1),
        };

        assert!(!client.supports_transcript_correction());
        assert!(!client.can_run_llm_transcript_correction());
    }

    #[test]
    fn cli_agent_harnesses_require_unsafe_opt_in_for_untrusted_agent_workspaces() {
        for harness in [
            SummaryHarness::Claude,
            SummaryHarness::OpenCode,
            SummaryHarness::CursorAgent,
        ] {
            let mut client = HarnessCliSummaryClient {
                harness,
                command_path: "agent-cli".to_owned(),
                model: "model-a".to_owned(),
                allow_unsafe_agent_harness: false,
                retry_policy: RetryPolicy::default(),
                command_timeout: Duration::from_secs(1),
            };

            assert!(!client.supports_untrusted_agent_workspace());
            client.allow_unsafe_agent_harness = true;
            assert!(client.supports_untrusted_agent_workspace());
        }
    }

    #[test]
    fn summary_harnesses_read_output_file_and_treat_stdout_as_diagnostic() {
        let _guard = command_test_lock();
        for harness in [
            SummaryHarness::Claude,
            SummaryHarness::OpenCode,
            SummaryHarness::CursorAgent,
        ] {
            let workdir = unique_summary_workdir("summary_file_success");
            create_summary_agent_workdir(&workdir);
            let log_path = workdir.join("command.log");
            let stdin_path = workdir.join("stdin.log");
            let script_path = write_summary_script(
                "summary_success",
                &format!(
                    "#!/bin/sh\npwd > '{}'\nprintf '%s\\n' \"$@\" >> '{}'\ncat > '{}'\nprintf 'stdout summary must be ignored\\n'\nmkdir -p output\nprintf '## Summary\\nfrom file\\n' > output/summary.md\n",
                    log_path.display(),
                    log_path.display(),
                    stdin_path.display()
                ),
            );
            let client = summary_test_client(harness, &script_path);

            let markdown = run_agent_harness_with_output_contract(
                &client,
                "PROMPT BODY",
                Some(&workdir),
                SUMMARY_OUTPUT_CONTRACT,
            )
            .expect("summary should be read from output file");

            assert_eq!(markdown, "## Summary\nfrom file\n");
            let log = std::fs::read_to_string(&log_path).expect("log should exist");
            let lines = log.lines().collect::<Vec<_>>();
            assert_eq!(
                lines[0],
                std::fs::canonicalize(&workdir)
                    .expect("workdir should canonicalize")
                    .to_string_lossy()
            );
            match harness {
                SummaryHarness::Claude => {
                    assert_eq!(lines[1..], ["--model", "model-a", "-p"]);
                    assert_eq!(
                        std::fs::read_to_string(&stdin_path).expect("stdin should be captured"),
                        "PROMPT BODY"
                    );
                }
                SummaryHarness::OpenCode => {
                    assert_eq!(lines[1..], ["run", "--model", "model-a", "PROMPT BODY"]);
                    assert_eq!(
                        std::fs::read_to_string(&stdin_path).expect("stdin should be empty"),
                        ""
                    );
                }
                SummaryHarness::CursorAgent => {
                    assert_eq!(
                        lines[1..],
                        [
                            "-p",
                            "--trust",
                            "PROMPT BODY",
                            "--output-format",
                            "text",
                            "--model",
                            "model-a"
                        ]
                    );
                    assert_eq!(
                        std::fs::read_to_string(&stdin_path).expect("stdin should be empty"),
                        ""
                    );
                }
            }

            let _ = std::fs::remove_dir_all(&workdir);
            let _ = std::fs::remove_file(&script_path);
        }
    }

    #[test]
    fn summary_harness_can_read_non_summary_output_contract() {
        let _guard = command_test_lock();
        let workdir = unique_summary_workdir("ai_memory_file_success");
        create_summary_agent_workdir(&workdir);
        let script_path = write_summary_script(
            "ai_memory_success",
            "#!/bin/sh\nprintf '{\"memory_notes\":[{\"title\":\"stdout must be ignored\"}]}'\nmkdir -p output\nprintf '{\"memory_notes\":[]}' > output/ai_memory_candidates.json\n",
        );
        let client = summary_test_client(SummaryHarness::Claude, &script_path);

        let json = run_agent_harness_with_output_contract(
            &client,
            "PROMPT BODY",
            Some(&workdir),
            AgentOutputContract::new(
                "output/ai_memory_candidates.json",
                "AI memory candidate output",
                1024,
            ),
        )
        .expect("AI memory candidates should be read from output file");

        assert_eq!(json, "{\"memory_notes\":[]}");
        let _ = std::fs::remove_dir_all(&workdir);
        let _ = std::fs::remove_file(&script_path);
    }

    #[test]
    fn summary_harness_missing_output_file_fails_with_sanitized_diagnostics() {
        let _guard = command_test_lock();
        let workdir = unique_summary_workdir("summary_missing_output");
        create_summary_agent_workdir(&workdir);
        let script_path = write_summary_script(
            "summary_missing",
            "#!/bin/sh\necho 'bearer sk-secret123456789' >&2\necho 'stdout fallback summary'\n",
        );
        let client = summary_test_client(SummaryHarness::Claude, &script_path);

        let err = run_agent_harness_with_output_contract(
            &client,
            "PROMPT BODY",
            Some(&workdir),
            SUMMARY_OUTPUT_CONTRACT,
        )
        .expect_err("stdout must not be accepted as summary content");

        let message = err.to_string();
        assert!(message.contains("missing summary output file output/summary.md"));
        assert!(message.contains("stdout fallback summary"));
        assert!(!message.contains("sk-secret123456789"));
        assert!(!message.contains(&workdir.to_string_lossy().to_string()));
        let _ = std::fs::remove_dir_all(&workdir);
        let _ = std::fs::remove_file(&script_path);
    }

    #[test]
    fn summary_harness_diagnostics_are_bounded_and_redacted() {
        let _guard = command_test_lock();
        let workdir = unique_summary_workdir("summary_bounded_diagnostics");
        create_summary_agent_workdir(&workdir);
        let script_path = write_summary_script(
            "summary_bounded_diagnostics",
            "#!/bin/sh\npython3 - <<'PY'\nimport sys\nsys.stdout.write('stdout-start sk-secret123456789 ' + ('x' * 2000))\nsys.stderr.write('stderr-start bearer abcdefghijklmnop ' + ('y' * 2000))\nPY\n",
        );
        let client = summary_test_client(SummaryHarness::Claude, &script_path);

        let err = run_agent_harness_with_output_contract(
            &client,
            "PROMPT BODY",
            Some(&workdir),
            SUMMARY_OUTPUT_CONTRACT,
        )
        .expect_err("missing output should fail with diagnostics only");

        let message = err.to_string();
        assert!(message.contains("stdout-start"));
        assert!(message.contains("stderr-start"));
        assert!(message.contains("bytes omitted"));
        assert!(!message.contains("sk-secret123456789"));
        assert!(!message.contains("bearer abcdefghijklmnop"));
        assert!(
            message.len() < 1_400,
            "diagnostics should stay bounded, got {} bytes: {message}",
            message.len()
        );
        let _ = std::fs::remove_dir_all(&workdir);
        let _ = std::fs::remove_file(&script_path);
    }

    #[test]
    fn summary_harness_rejects_empty_oversized_and_invalid_output_files() {
        let _guard = command_test_lock();
        for (label, script, expected) in [
            (
                "empty",
                "#!/bin/sh\nmkdir -p output\nprintf '  \\n' > output/summary.md\n",
                "empty summary output file output/summary.md",
            ),
            (
                "oversized",
                "#!/bin/sh\nmkdir -p output\npython3 -c 'import sys; sys.stdout.buffer.write(b\"x\" * 1048577)' > output/summary.md\n",
                "oversized summary output file output/summary.md",
            ),
            (
                "invalid_utf8",
                "#!/bin/sh\nmkdir -p output\nprintf '\\377' > output/summary.md\n",
                "not valid UTF-8",
            ),
        ] {
            let workdir = unique_summary_workdir(label);
            create_summary_agent_workdir(&workdir);
            let script_path = write_summary_script(label, script);
            let client = summary_test_client(SummaryHarness::OpenCode, &script_path);

            let err = run_agent_harness_with_output_contract(
                &client,
                "PROMPT BODY",
                Some(&workdir),
                SUMMARY_OUTPUT_CONTRACT,
            )
            .expect_err("invalid output should fail");

            assert!(
                err.to_string().contains(expected),
                "unexpected error for {label}: {err}"
            );
            let _ = std::fs::remove_dir_all(&workdir);
            let _ = std::fs::remove_file(&script_path);
        }
    }

    #[test]
    fn summary_harness_nonzero_exit_does_not_accept_output_file() {
        let _guard = command_test_lock();
        let workdir = unique_summary_workdir("summary_nonzero");
        create_summary_agent_workdir(&workdir);
        let script_path = write_summary_script(
            "summary_nonzero",
            "#!/bin/sh\nmkdir -p output\nprintf '## Summary\\nshould fail\\n' > output/summary.md\necho diagnostic >&2\nexit 7\n",
        );
        let client = summary_test_client(SummaryHarness::CursorAgent, &script_path);

        let err = run_agent_harness_with_output_contract(
            &client,
            "PROMPT BODY",
            Some(&workdir),
            SUMMARY_OUTPUT_CONTRACT,
        )
        .expect_err("nonzero command status should fail even with an output file");

        assert!(err.to_string().contains("summary command failed"));
        assert!(!err.to_string().contains("should fail"));
        let _ = std::fs::remove_dir_all(&workdir);
        let _ = std::fs::remove_file(&script_path);
    }

    #[test]
    fn summary_harness_removes_stale_output_before_retry() {
        let _guard = command_test_lock();
        let workdir = unique_summary_workdir("summary_retry_stale");
        create_summary_agent_workdir(&workdir);
        std::fs::write(workdir.join("output/summary.md"), "## Summary\nstale\n")
            .expect("stale output");
        let attempt_path = workdir.join("attempts");
        let script_path = write_summary_script(
            "summary_retry",
            &format!(
                "#!/bin/sh\nattempts=$(cat '{}' 2>/dev/null || printf 0)\nattempts=$((attempts + 1))\nprintf '%s' \"$attempts\" > '{}'\nif [ \"$attempts\" -eq 1 ]; then exit 7; fi\nmkdir -p output\nprintf '## Summary\\nfresh\\n' > output/summary.md\n",
                attempt_path.display(),
                attempt_path.display()
            ),
        );
        let mut client = summary_test_client(SummaryHarness::Claude, &script_path);
        client.retry_policy.max_attempts = 2;

        let markdown = run_agent_harness_with_output_contract(
            &client,
            "PROMPT BODY",
            Some(&workdir),
            SUMMARY_OUTPUT_CONTRACT,
        )
        .expect("second attempt should succeed with fresh output");

        assert_eq!(markdown, "## Summary\nfresh\n");
        assert_eq!(
            std::fs::read_to_string(&attempt_path).expect("attempts"),
            "2"
        );
        let _ = std::fs::remove_dir_all(&workdir);
        let _ = std::fs::remove_file(&script_path);
    }

    #[test]
    fn summary_harness_rejects_non_agent_workdir_before_spawning_cursor_trust() {
        let _guard = command_test_lock();
        let workdir = unique_summary_workdir("summary_non_agent_workdir");
        std::fs::create_dir_all(&workdir).expect("workdir");
        let marker_path = workdir.join("spawned");
        let script_path = write_summary_script(
            "summary_non_agent_workdir",
            &format!(
                "#!/bin/sh\nprintf spawned > '{}'\nmkdir -p output\nprintf '## Summary\\nunsafe\\n' > output/summary.md\n",
                marker_path.display()
            ),
        );
        let client = summary_test_client(SummaryHarness::CursorAgent, &script_path);

        let err = run_agent_harness_with_output_contract(
            &client,
            "PROMPT BODY",
            Some(&workdir),
            SUMMARY_OUTPUT_CONTRACT,
        )
        .expect_err("non-agent workdir must fail before --trust spawn");

        assert!(
            err.to_string()
                .contains("missing expected agent workspace markers")
        );
        assert!(
            !marker_path.exists(),
            "cursor command must not spawn with --trust outside generated workspace"
        );
        let _ = std::fs::remove_dir_all(&workdir);
        let _ = std::fs::remove_file(&script_path);
    }

    #[test]
    fn command_runner_times_out_when_child_does_not_read_large_stdin() {
        let mut command = Command::new("sh");
        command.arg("-c").arg("sleep 5");
        let input = vec![b'x'; 2 * 1024 * 1024];

        let started = Instant::now();
        let result =
            run_command_with_timeout(&mut command, Some(&input), Duration::from_millis(50));

        assert!(matches!(result, Err(IntegrationError::Timeout { .. })));
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "stdin write must participate in the command timeout"
        );
    }

    #[test]
    fn command_runner_does_not_block_when_descendant_keeps_stdin_open() {
        let mut command = Command::new("sh");
        command.arg("-c").arg("(sleep 5) & exit 0");
        let input = vec![b'x'; 2 * 1024 * 1024];

        let started = Instant::now();
        let output = run_command_with_timeout(&mut command, Some(&input), Duration::from_secs(2))
            .expect("parent shell should exit");

        assert!(output.status.success());
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "runner should not wait on stdin after the parent exits"
        );
    }

    #[cfg(unix)]
    #[test]
    fn command_runner_kills_descendants_on_timeout() {
        let marker = format!(
            "discord_transcript_descendant_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        let mut command = Command::new("sh");
        command.arg("-c").arg(format!(
            "sh -c 'sleep 5' {marker} & while :; do sleep 1; done"
        ));

        let result = run_command_with_timeout(&mut command, None, Duration::from_millis(50));
        assert!(matches!(result, Err(IntegrationError::Timeout { .. })));
        std::thread::sleep(Duration::from_millis(100));

        assert!(
            !process_command_line_contains(&marker),
            "timed-out command descendant survived process-group cleanup"
        );
    }

    #[cfg(unix)]
    #[test]
    fn command_temp_files_are_owner_only() {
        use std::os::unix::fs::PermissionsExt;

        let path = std::env::temp_dir().join(format!(
            "discord_transcript_mode_test_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let file = create_temp_output_file(&path).expect("temp file should be created");
        drop(file);

        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        let _ = std::fs::remove_file(&path);

        assert_eq!(mode, 0o600);
    }

    #[test]
    fn whisper_nonzero_stderr_is_sanitized() {
        let script_path = std::env::temp_dir().join(format!(
            "discord_transcript_curl_fail_{}_{}.sh",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        std::fs::write(
            &script_path,
            "#!/bin/sh\necho 'bearer sk-secret123456789 token' >&2\nexit 7\n",
        )
        .expect("script should be written");
        make_executable(&script_path);

        let client = CommandWhisperClient {
            endpoint: "http://localhost".to_owned(),
            curl_bin: script_path.to_string_lossy().to_string(),
            retry_policy: RetryPolicy {
                max_attempts: 1,
                initial_delay: Duration::from_millis(1),
                backoff_multiplier: 1,
                max_delay: Duration::from_millis(1),
            },
            beam_size: 1,
            suppress_non_speech: false,
            prompt: None,
            vad: false,
            temperature: 0.0,
            command_timeout: Duration::from_secs(1),
        };
        let request = WhisperInferenceRequest {
            audio_path: "audio.wav".to_owned(),
            language: None,
            prompt: None,
        };

        let err = client.infer(&request).expect_err("command should fail");
        let _ = std::fs::remove_file(&script_path);

        let message = match err {
            WhisperParseError::InvalidJson(message) => message,
            other => panic!("unexpected error: {other}"),
        };
        assert!(!message.contains("sk-secret123456789"));
    }

    #[test]
    fn whisper_command_redacts_endpoint_credentials_in_errors() {
        let script_path = std::env::temp_dir().join(format!(
            "discord_transcript_curl_endpoint_leak_{}_{}.sh",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        std::fs::write(&script_path, "#!/bin/sh\nexit 7\n").expect("script should be written");
        make_executable(&script_path);

        let client = CommandWhisperClient {
            endpoint:
                "https://user:password@example.test?api_key=secret&bearer_token=tok-secret&key=raw-key&x=1"
                    .to_owned(),
            curl_bin: script_path.to_string_lossy().to_string(),
            retry_policy: RetryPolicy {
                max_attempts: 1,
                initial_delay: Duration::from_millis(1),
                backoff_multiplier: 1,
                max_delay: Duration::from_millis(1),
            },
            beam_size: 1,
            suppress_non_speech: false,
            prompt: None,
            vad: false,
            temperature: 0.0,
            command_timeout: Duration::from_secs(1),
        };
        let request = WhisperInferenceRequest {
            audio_path: "audio.wav".to_owned(),
            language: None,
            prompt: None,
        };

        let err = client.infer(&request).expect_err("command should fail");
        let _ = std::fs::remove_file(&script_path);

        let message = match err {
            WhisperParseError::InvalidJson(message) => message,
            other => panic!("unexpected error: {other}"),
        };
        assert!(!message.contains("user:password"));
        assert!(!message.contains("api_key=secret"));
        assert!(!message.contains("bearer_token=tok-secret"));
        assert!(!message.contains("key=raw-key"));
        assert!(message.contains("https://REDACTED:REDACTED@example.test/"));
        assert!(message.contains("api_key=REDACTED"));
        assert!(message.contains("bearer_token=REDACTED"));
        assert!(message.contains("key=REDACTED"));
        assert!(message.contains("x=1"));
    }

    #[test]
    fn whisper_endpoint_redaction_preserves_normal_endpoint_context() {
        let rendered = sanitize_whisper_endpoint_for_log(
            "https://whisper.example.test/base/path/inference?debug=true&design_mode=true&assign=owner&x=1",
        );

        assert_eq!(
            rendered,
            "https://whisper.example.test/base/path/inference?debug=true&design_mode=true&assign=owner&x=1"
        );
    }

    #[test]
    fn whisper_command_uses_real_endpoint_when_log_endpoint_is_redacted() {
        let args_path = std::env::temp_dir().join(format!(
            "discord_transcript_whisper_endpoint_args_{}_{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        let script_path = std::env::temp_dir().join(format!(
            "discord_transcript_curl_endpoint_capture_{}_{}.sh",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        std::fs::write(
            &script_path,
            format!(
                "#!/bin/sh\nprintf '%s\\n' \"$@\" > '{}'\nprintf '{{\"text\":\"ok\",\"segments\":[{{\"speaker\":\"alice\",\"start\":0.0,\"end\":1.0,\"text\":\"hello\"}}]}}'\n",
                args_path.display()
            ),
        )
        .expect("script should be written");
        make_executable(&script_path);

        let client = CommandWhisperClient {
            endpoint: "https://user:password@example.test/v1?api_key=secret&x=1".to_owned(),
            curl_bin: script_path.to_string_lossy().to_string(),
            retry_policy: RetryPolicy {
                max_attempts: 1,
                initial_delay: Duration::from_millis(1),
                backoff_multiplier: 1,
                max_delay: Duration::from_millis(1),
            },
            beam_size: 1,
            suppress_non_speech: false,
            prompt: None,
            vad: false,
            temperature: 0.0,
            command_timeout: Duration::from_secs(5),
        };
        let request = WhisperInferenceRequest {
            audio_path: "audio.wav".to_owned(),
            language: None,
            prompt: None,
        };

        client.infer(&request).expect("command should succeed");
        let args = std::fs::read_to_string(&args_path).expect("args should be captured");
        let _ = std::fs::remove_file(&script_path);
        let _ = std::fs::remove_file(&args_path);

        assert!(
            args.contains("https://user:password@example.test/v1/inference?api_key=secret&x=1")
        );
    }

    #[test]
    fn whisper_command_combines_configured_and_request_prompt() {
        let args_path = std::env::temp_dir().join(format!(
            "discord_transcript_whisper_args_{}_{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        let script_path = std::env::temp_dir().join(format!(
            "discord_transcript_curl_capture_{}_{}.sh",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        std::fs::write(
            &script_path,
            format!(
                "#!/bin/sh\nprintf '%s\\n' \"$@\" > '{}'\nprintf '{{\"text\":\"ok\",\"segments\":[{{\"speaker\":\"alice\",\"start\":0.0,\"end\":1.0,\"text\":\"hello\"}}]}}'\n",
                args_path.display()
            ),
        )
        .expect("script should be written");
        make_executable(&script_path);

        let client = CommandWhisperClient {
            endpoint: "http://localhost".to_owned(),
            curl_bin: script_path.to_string_lossy().to_string(),
            retry_policy: RetryPolicy {
                max_attempts: 1,
                initial_delay: Duration::from_millis(1),
                backoff_multiplier: 1,
                max_delay: Duration::from_millis(1),
            },
            beam_size: 1,
            suppress_non_speech: false,
            prompt: Some("共通用語: Kubernetes".to_owned()),
            vad: false,
            temperature: 0.0,
            command_timeout: Duration::from_secs(5),
        };
        let request = WhisperInferenceRequest {
            audio_path: "audio.wav".to_owned(),
            language: Some("ja".to_owned()),
            prompt: Some("Meeting title: 朝会\nSpeaker ID: 山田太郎".to_owned()),
        };

        client.infer(&request).expect("command should succeed");
        let args = std::fs::read_to_string(&args_path).expect("args should be captured");
        let _ = std::fs::remove_file(&script_path);
        let _ = std::fs::remove_file(&args_path);

        let expected = "prompt=共通用語: Kubernetes\nMeeting title: 朝会\nSpeaker ID: 山田太郎";
        assert!(
            args.contains(expected),
            "combined prompt missing from argv: {args}"
        );
        assert!(
            args.find("共通用語").unwrap() < args.find("Meeting title").unwrap(),
            "configured prompt should be the prefix: {args}"
        );
    }

    #[test]
    fn whisper_command_uses_configured_prompt_as_fallback() {
        let args_path = std::env::temp_dir().join(format!(
            "discord_transcript_whisper_fallback_args_{}_{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        let script_path = std::env::temp_dir().join(format!(
            "discord_transcript_curl_fallback_{}_{}.sh",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        std::fs::write(
            &script_path,
            format!(
                "#!/bin/sh\nprintf '%s\\n' \"$@\" > '{}'\nprintf '{{\"text\":\"ok\",\"segments\":[{{\"speaker\":\"alice\",\"start\":0.0,\"end\":1.0,\"text\":\"hello\"}}]}}'\n",
                args_path.display()
            ),
        )
        .expect("script should be written");
        make_executable(&script_path);

        let client = CommandWhisperClient {
            endpoint: "http://localhost".to_owned(),
            curl_bin: script_path.to_string_lossy().to_string(),
            retry_policy: RetryPolicy {
                max_attempts: 1,
                initial_delay: Duration::from_millis(1),
                backoff_multiplier: 1,
                max_delay: Duration::from_millis(1),
            },
            beam_size: 1,
            suppress_non_speech: false,
            prompt: Some("legacy WHISPER_PROMPT".to_owned()),
            vad: false,
            temperature: 0.0,
            command_timeout: Duration::from_secs(5),
        };
        let request = WhisperInferenceRequest {
            audio_path: "audio.wav".to_owned(),
            language: None,
            prompt: None,
        };

        client.infer(&request).expect("command should succeed");
        let args = std::fs::read_to_string(&args_path).expect("args should be captured");
        let _ = std::fs::remove_file(&script_path);
        let _ = std::fs::remove_file(&args_path);

        assert!(args.contains("prompt=legacy WHISPER_PROMPT"));
    }

    #[test]
    fn whisper_prompt_is_redacted_from_errors_and_debug_output() {
        let secret_configured = "configured prompt secret";
        let secret_request = "会議固有プロンプト -- customer secret -F still secret";
        let script_path = std::env::temp_dir().join(format!(
            "discord_transcript_curl_prompt_leak_{}_{}.sh",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        std::fs::write(
            &script_path,
            "#!/bin/sh\nprintf '%s\\n' \"$@\" >&2\nexit 7\n",
        )
        .expect("script should be written");
        make_executable(&script_path);

        let client = CommandWhisperClient {
            endpoint: "http://localhost".to_owned(),
            curl_bin: script_path.to_string_lossy().to_string(),
            retry_policy: RetryPolicy {
                max_attempts: 1,
                initial_delay: Duration::from_millis(1),
                backoff_multiplier: 1,
                max_delay: Duration::from_millis(1),
            },
            beam_size: 1,
            suppress_non_speech: false,
            prompt: Some(secret_configured.to_owned()),
            vad: false,
            temperature: 0.0,
            command_timeout: Duration::from_secs(5),
        };
        let request = WhisperInferenceRequest {
            audio_path: r#"audio file "quoted"\chunk.wav"#.to_owned(),
            language: None,
            prompt: Some(secret_request.to_owned()),
        };

        let err = client.infer(&request).expect_err("command should fail");
        let _ = std::fs::remove_file(&script_path);

        let message = match err {
            WhisperParseError::InvalidJson(message) => message,
            other => panic!("unexpected error: {other}"),
        };
        assert!(!message.contains(secret_configured));
        assert!(!message.contains(secret_request));
        assert!(!message.contains("customer secret"));
        assert!(!message.contains("still secret"));
        assert!(message.contains(r#""file=@audio file \"quoted\"\\chunk.wav""#));
        assert!(message.contains("prompt=[REDACTED]"));

        let client_debug = format!("{client:?}");
        let request_debug = format!("{request:?}");
        assert!(!client_debug.contains(secret_configured));
        assert!(!request_debug.contains(secret_request));
        assert!(client_debug.contains("[REDACTED]"));
        assert!(request_debug.contains("[REDACTED]"));
    }

    #[test]
    fn whisper_parse_errors_do_not_retry_command() {
        let marker_path = std::env::temp_dir().join(format!(
            "discord_transcript_whisper_parse_retry_{}_{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        let script_path = std::env::temp_dir().join(format!(
            "discord_transcript_curl_malformed_{}_{}.sh",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        std::fs::write(
            &script_path,
            format!(
                "#!/bin/sh\nprintf x >> '{}'\nprintf '{{}}'\n",
                marker_path.display()
            ),
        )
        .expect("script should be written");
        make_executable(&script_path);

        let client = CommandWhisperClient {
            endpoint: "http://localhost".to_owned(),
            curl_bin: script_path.to_string_lossy().to_string(),
            retry_policy: RetryPolicy {
                max_attempts: 3,
                initial_delay: Duration::from_millis(1),
                backoff_multiplier: 1,
                max_delay: Duration::from_millis(1),
            },
            beam_size: 1,
            suppress_non_speech: false,
            prompt: None,
            vad: false,
            temperature: 0.0,
            command_timeout: Duration::from_secs(1),
        };
        let request = WhisperInferenceRequest {
            audio_path: "audio.wav".to_owned(),
            language: None,
            prompt: None,
        };

        let err = client
            .infer(&request)
            .expect_err("malformed response should fail");
        let attempts = std::fs::read_to_string(&marker_path)
            .expect("marker should be written")
            .len();
        let _ = std::fs::remove_file(&script_path);
        let _ = std::fs::remove_file(&marker_path);

        assert!(err.to_string().contains("missing field"));
        assert_eq!(attempts, 1);
    }

    #[test]
    fn whisper_http_503_retries_then_accepts_valid_json() {
        let _guard = command_test_lock();
        let marker_path = std::env::temp_dir().join(format!(
            "discord_transcript_whisper_http_503_retry_{}_{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        let script_path = std::env::temp_dir().join(format!(
            "discord_transcript_curl_http_503_retry_{}_{}.sh",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        std::fs::write(
            &script_path,
            format!(
                "#!/bin/sh\nattempts=$(cat '{}' 2>/dev/null || printf 0)\nattempts=$((attempts + 1))\nprintf '%s' \"$attempts\" > '{}'\nif [ \"$attempts\" -eq 1 ]; then\nprintf '{{\"error\":\"busy\"}}\\n503'\nexit 0\nfi\nprintf '{{\"text\":\"ok\",\"segments\":[{{\"speaker\":\"alice\",\"start\":0.0,\"end\":1.0,\"text\":\"hello\"}}]}}\\n200'\n",
                marker_path.display(),
                marker_path.display()
            ),
        )
        .expect("script should be written");
        make_executable(&script_path);

        let client = CommandWhisperClient {
            endpoint: "http://localhost".to_owned(),
            curl_bin: script_path.to_string_lossy().to_string(),
            retry_policy: RetryPolicy {
                max_attempts: 3,
                initial_delay: Duration::from_millis(1),
                backoff_multiplier: 1,
                max_delay: Duration::from_millis(1),
            },
            beam_size: 1,
            suppress_non_speech: false,
            prompt: None,
            vad: false,
            temperature: 0.0,
            command_timeout: Duration::from_secs(1),
        };
        let request = WhisperInferenceRequest {
            audio_path: "audio.wav".to_owned(),
            language: None,
            prompt: None,
        };

        let result = client
            .infer(&request)
            .expect("503 should retry and the second 200 response should parse");
        let attempts = std::fs::read_to_string(&marker_path).expect("marker should be written");
        let _ = std::fs::remove_file(&script_path);
        let _ = std::fs::remove_file(&marker_path);

        assert_eq!(result.text, "ok");
        assert_eq!(attempts, "2");
    }

    #[test]
    fn whisper_http_429_retries_then_accepts_valid_json() {
        let _guard = command_test_lock();
        let marker_path = std::env::temp_dir().join(format!(
            "discord_transcript_whisper_http_429_retry_{}_{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        let script_path = std::env::temp_dir().join(format!(
            "discord_transcript_curl_http_429_retry_{}_{}.sh",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        std::fs::write(
            &script_path,
            format!(
                "#!/bin/sh\nattempts=$(cat '{}' 2>/dev/null || printf 0)\nattempts=$((attempts + 1))\nprintf '%s' \"$attempts\" > '{}'\nif [ \"$attempts\" -eq 1 ]; then\nprintf '{{\"error\":\"rate limited\"}}\\n429'\nexit 0\nfi\nprintf '{{\"text\":\"ok\",\"segments\":[{{\"speaker\":\"alice\",\"start\":0.0,\"end\":1.0,\"text\":\"hello\"}}]}}\\n200'\n",
                marker_path.display(),
                marker_path.display()
            ),
        )
        .expect("script should be written");
        make_executable(&script_path);

        let client = CommandWhisperClient {
            endpoint: "http://localhost".to_owned(),
            curl_bin: script_path.to_string_lossy().to_string(),
            retry_policy: RetryPolicy {
                max_attempts: 3,
                initial_delay: Duration::from_millis(1),
                backoff_multiplier: 1,
                max_delay: Duration::from_millis(1),
            },
            beam_size: 1,
            suppress_non_speech: false,
            prompt: None,
            vad: false,
            temperature: 0.0,
            command_timeout: Duration::from_secs(1),
        };
        let request = WhisperInferenceRequest {
            audio_path: "audio.wav".to_owned(),
            language: None,
            prompt: None,
        };

        let result = client
            .infer(&request)
            .expect("429 should retry and the second 200 response should parse");
        let attempts = std::fs::read_to_string(&marker_path).expect("marker should be written");
        let _ = std::fs::remove_file(&script_path);
        let _ = std::fs::remove_file(&marker_path);

        assert_eq!(result.text, "ok");
        assert_eq!(attempts, "2");
    }

    #[test]
    fn whisper_http_400_does_not_retry_command() {
        let _guard = command_test_lock();
        let marker_path = std::env::temp_dir().join(format!(
            "discord_transcript_whisper_http_400_retry_{}_{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        let script_path = std::env::temp_dir().join(format!(
            "discord_transcript_curl_http_400_{}_{}.sh",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        std::fs::write(
            &script_path,
            format!(
                "#!/bin/sh\nprintf x >> '{}'\nprintf '{{\"error\":\"bad request\"}}\\n400'\n",
                marker_path.display()
            ),
        )
        .expect("script should be written");
        make_executable(&script_path);

        let client = CommandWhisperClient {
            endpoint: "http://localhost".to_owned(),
            curl_bin: script_path.to_string_lossy().to_string(),
            retry_policy: RetryPolicy {
                max_attempts: 3,
                initial_delay: Duration::from_millis(1),
                backoff_multiplier: 1,
                max_delay: Duration::from_millis(1),
            },
            beam_size: 1,
            suppress_non_speech: false,
            prompt: None,
            vad: false,
            temperature: 0.0,
            command_timeout: Duration::from_secs(1),
        };
        let request = WhisperInferenceRequest {
            audio_path: "audio.wav".to_owned(),
            language: None,
            prompt: None,
        };

        let err = client
            .infer(&request)
            .expect_err("400 should fail without retrying");
        let attempts = std::fs::read_to_string(&marker_path)
            .expect("marker should be written")
            .len();
        let _ = std::fs::remove_file(&script_path);
        let _ = std::fs::remove_file(&marker_path);

        assert!(err.to_string().contains("http_status=400"));
        assert_eq!(attempts, 1);
    }

    #[test]
    fn whisper_timeout_retries_then_accepts_valid_json() {
        let _guard = command_test_lock();
        let marker_path = std::env::temp_dir().join(format!(
            "discord_transcript_whisper_timeout_retry_{}_{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        let script_path = std::env::temp_dir().join(format!(
            "discord_transcript_curl_timeout_retry_{}_{}.sh",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        std::fs::write(
            &script_path,
            format!(
                "#!/bin/sh\nattempts=$(cat '{}' 2>/dev/null || printf 0)\nattempts=$((attempts + 1))\nprintf '%s' \"$attempts\" > '{}'\nif [ \"$attempts\" -eq 1 ]; then sleep 2; fi\nprintf '{{\"text\":\"ok\",\"segments\":[{{\"speaker\":\"alice\",\"start\":0.0,\"end\":1.0,\"text\":\"hello\"}}]}}\\n200'\n",
                marker_path.display(),
                marker_path.display()
            ),
        )
        .expect("script should be written");
        make_executable(&script_path);

        let client = CommandWhisperClient {
            endpoint: "http://localhost".to_owned(),
            curl_bin: script_path.to_string_lossy().to_string(),
            retry_policy: RetryPolicy {
                max_attempts: 3,
                initial_delay: Duration::from_millis(1),
                backoff_multiplier: 1,
                max_delay: Duration::from_millis(1),
            },
            beam_size: 1,
            suppress_non_speech: false,
            prompt: None,
            vad: false,
            temperature: 0.0,
            command_timeout: Duration::from_millis(500),
        };
        let request = WhisperInferenceRequest {
            audio_path: "audio.wav".to_owned(),
            language: None,
            prompt: None,
        };

        let result = client
            .infer(&request)
            .expect("timeout should retry and the second response should parse");
        let attempts = std::fs::read_to_string(&marker_path).expect("marker should be written");
        let _ = std::fs::remove_file(&script_path);
        let _ = std::fs::remove_file(&marker_path);

        assert_eq!(result.text, "ok");
        assert_eq!(attempts, "2");
    }

    #[test]
    fn whisper_malformed_http_200_json_does_not_retry_command() {
        let _guard = command_test_lock();
        let marker_path = std::env::temp_dir().join(format!(
            "discord_transcript_whisper_malformed_200_retry_{}_{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        let script_path = std::env::temp_dir().join(format!(
            "discord_transcript_curl_malformed_200_{}_{}.sh",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        std::fs::write(
            &script_path,
            format!(
                "#!/bin/sh\nprintf x >> '{}'\nprintf '{{}}\\n200'\n",
                marker_path.display()
            ),
        )
        .expect("script should be written");
        make_executable(&script_path);

        let client = CommandWhisperClient {
            endpoint: "http://localhost".to_owned(),
            curl_bin: script_path.to_string_lossy().to_string(),
            retry_policy: RetryPolicy {
                max_attempts: 3,
                initial_delay: Duration::from_millis(1),
                backoff_multiplier: 1,
                max_delay: Duration::from_millis(1),
            },
            beam_size: 1,
            suppress_non_speech: false,
            prompt: None,
            vad: false,
            temperature: 0.0,
            command_timeout: Duration::from_secs(1),
        };
        let request = WhisperInferenceRequest {
            audio_path: "audio.wav".to_owned(),
            language: None,
            prompt: None,
        };

        let err = client
            .infer(&request)
            .expect_err("malformed 200 JSON should fail without retrying");
        let attempts = std::fs::read_to_string(&marker_path)
            .expect("marker should be written")
            .len();
        let _ = std::fs::remove_file(&script_path);
        let _ = std::fs::remove_file(&marker_path);

        assert!(
            err.to_string()
                .contains("malformed successful whisper response")
        );
        assert_eq!(attempts, 1);
    }

    #[test]
    fn whisper_nonzero_exit_still_retries_command() {
        let _guard = command_test_lock();
        let marker_path = std::env::temp_dir().join(format!(
            "discord_transcript_whisper_exit_retry_{}_{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        let script_path = std::env::temp_dir().join(format!(
            "discord_transcript_curl_retry_{}_{}.sh",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        std::fs::write(
            &script_path,
            format!(
                "#!/bin/sh\nprintf x >> '{}'\nexit 7\n",
                marker_path.display()
            ),
        )
        .expect("script should be written");
        make_executable(&script_path);

        let client = CommandWhisperClient {
            endpoint: "http://localhost".to_owned(),
            curl_bin: script_path.to_string_lossy().to_string(),
            retry_policy: RetryPolicy {
                max_attempts: 3,
                initial_delay: Duration::from_millis(1),
                backoff_multiplier: 1,
                max_delay: Duration::from_millis(1),
            },
            beam_size: 1,
            suppress_non_speech: false,
            prompt: None,
            vad: false,
            temperature: 0.0,
            command_timeout: Duration::from_secs(1),
        };
        let request = WhisperInferenceRequest {
            audio_path: "audio.wav".to_owned(),
            language: None,
            prompt: None,
        };

        let err = client
            .infer(&request)
            .expect_err("non-zero command should fail after retries");
        let attempts = std::fs::read_to_string(&marker_path)
            .expect("marker should be written")
            .len();
        let _ = std::fs::remove_file(&script_path);
        let _ = std::fs::remove_file(&marker_path);

        assert!(err.to_string().contains("whisper command failed"));
        assert!(attempts > 1, "non-zero command should be retried");
    }

    #[cfg(unix)]
    fn make_executable(path: &std::path::Path) {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = std::fs::metadata(path).unwrap().permissions();
        permissions.set_mode(0o700);
        std::fs::set_permissions(path, permissions).unwrap();
    }

    #[cfg(not(unix))]
    fn make_executable(_path: &std::path::Path) {}

    fn summary_test_client(
        harness: SummaryHarness,
        script_path: &std::path::Path,
    ) -> HarnessCliSummaryClient {
        HarnessCliSummaryClient {
            harness,
            command_path: script_path.to_string_lossy().to_string(),
            model: "model-a".to_owned(),
            allow_unsafe_agent_harness: true,
            retry_policy: RetryPolicy {
                max_attempts: 1,
                initial_delay: Duration::from_millis(1),
                backoff_multiplier: 1,
                max_delay: Duration::from_millis(1),
            },
            command_timeout: Duration::from_secs(10),
        }
    }

    fn unique_summary_workdir(label: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "discord_transcript_{label}_{}_{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ))
    }

    fn write_summary_script(label: &str, body: &str) -> std::path::PathBuf {
        let script_path = std::env::temp_dir().join(format!(
            "discord_transcript_{label}_{}_{}.sh",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        std::fs::write(&script_path, body).expect("script should be written");
        make_executable(&script_path);
        script_path
    }

    fn create_summary_agent_workdir(workdir: &std::path::Path) {
        std::fs::create_dir_all(workdir.join(AGENT_INPUT_DIR)).expect("input dir");
        std::fs::create_dir_all(workdir.join(AGENT_OUTPUT_DIR)).expect("output dir");
        std::fs::create_dir_all(workdir.join(AGENT_CURSOR_DIR)).expect("cursor dir");
        std::fs::write(
            workdir
                .join(AGENT_CURSOR_DIR)
                .join(AGENT_CURSOR_CONFIG_FILENAME),
            "{}",
        )
        .expect("cursor config");
    }

    fn command_test_lock() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
        LOCK.get_or_init(|| std::sync::Mutex::new(()))
            .lock()
            .expect("command test lock should not be poisoned")
    }

    #[cfg(unix)]
    fn process_command_line_contains(marker: &str) -> bool {
        let output = Command::new("ps")
            .arg("-axo")
            .arg("command")
            .output()
            .expect("ps should run");
        let commands = String::from_utf8_lossy(&output.stdout);
        commands.lines().any(|line| {
            line.contains(marker) && !line.contains("ps -axo") && !line.contains("cargo test")
        })
    }
}
