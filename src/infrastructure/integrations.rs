use crate::application::summary::{ClaudeSummaryClient, SummaryError};
use crate::bootstrap::config::SummaryHarness;
use crate::infrastructure::asr::{
    WhisperClient, WhisperInferenceRequest, WhisperParseError, WhisperTranscriptionResult,
    parse_whisper_response,
};
use crate::infrastructure::retry::{RetryPolicy, retry_with_backoff};
use std::fmt::{Display, Formatter};
use std::path::Path;
use std::process::Command;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IntegrationError {
    Io(String),
    NonZeroExit { code: i32, stderr: String },
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
            Self::InvalidUtf8 => write!(f, "invalid utf8 output from command"),
            Self::Parse(err) => write!(f, "parse error: {err}"),
        }
    }
}

impl std::error::Error for IntegrationError {}

#[derive(Debug, Clone, PartialEq)]
pub struct CommandWhisperClient {
    pub endpoint: String,
    pub curl_bin: String,
    pub retry_policy: RetryPolicy,
    pub beam_size: u32,
    pub suppress_non_speech: bool,
    pub prompt: Option<String>,
    pub vad: bool,
    pub temperature: f32,
}

impl WhisperClient for CommandWhisperClient {
    fn infer(
        &self,
        request: &WhisperInferenceRequest,
    ) -> Result<WhisperTranscriptionResult, WhisperParseError> {
        retry_with_backoff(self.retry_policy, |_| {
            let mut cmd = Command::new(&self.curl_bin);
            cmd.arg("-sS")
                .arg("-X")
                .arg("POST")
                .arg(format!("{}/inference", self.endpoint.trim_end_matches('/')))
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
            if let Some(p) = &self.prompt {
                cmd.arg("--form-string").arg(format!("prompt={p}"));
            }
            cmd.arg("-F").arg(format!("vad={}", self.vad));
            cmd.arg("-F")
                .arg(format!("temperature={}", self.temperature));

            let output = cmd
                .output()
                .map_err(|err| WhisperParseError::InvalidJson(err.to_string()))?;
            if !output.status.success() {
                return Err(WhisperParseError::InvalidJson(format!(
                    "whisper command failed: status={:?}, stderr={}",
                    output.status.code(),
                    String::from_utf8_lossy(&output.stderr)
                )));
            }

            let body = String::from_utf8(output.stdout)
                .map_err(|err| WhisperParseError::InvalidJson(err.to_string()))?;
            parse_whisper_response(&body).map_err(|err| {
                let preview: String = body.chars().take(200).collect();
                WhisperParseError::InvalidJson(format!(
                    "{err} (response body preview: {preview:?})"
                ))
            })
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HarnessCliSummaryClient {
    pub harness: SummaryHarness,
    pub command_path: String,
    pub model: String,
    pub retry_policy: RetryPolicy,
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
    let redacted =
        regex::Regex::new(r"(?i)(sk-[a-zA-Z0-9\-_]{8,}|key-[a-zA-Z0-9]{8,}|bearer\s+\S{8,})")
            .map(|re| re.replace_all(&collapsed, "[REDACTED]").into_owned())
            .unwrap_or(collapsed);

    if redacted.len() <= SANITIZE_MAX_LEN {
        return redacted;
    }
    let mut truncated: String = redacted.chars().take(SANITIZE_MAX_LEN).collect();
    let omitted = redacted.len() - truncated.len();
    let _ = write!(truncated, "... ({omitted} bytes omitted)");
    truncated
}

impl HarnessCliSummaryClient {
    /// Full-transcript correction sends one large prompt; argv-based CLIs risk `ARG_MAX` / hangs.
    pub fn can_run_llm_transcript_correction(&self) -> bool {
        matches!(self.harness, SummaryHarness::Claude)
    }
}

impl ClaudeSummaryClient for HarnessCliSummaryClient {
    fn summarize(&self, prompt: &str, workdir: Option<&Path>) -> Result<String, SummaryError> {
        retry_with_backoff(self.retry_policy, |_| match self.harness {
            SummaryHarness::Claude => summarize_claude_stdin(self, prompt, workdir),
            SummaryHarness::OpenCode => summarize_opencode_argv(self, prompt, workdir),
            SummaryHarness::CursorAgent => summarize_cursor_argv(self, prompt, workdir),
        })
    }
}

fn summarize_claude_stdin(
    client: &HarnessCliSummaryClient,
    prompt: &str,
    workdir: Option<&Path>,
) -> Result<String, SummaryError> {
    use std::io::Write;
    let mut command = Command::new(&client.command_path);
    command
        .arg("--model")
        .arg(&client.model)
        .arg("-p")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    if let Some(dir) = workdir {
        command.current_dir(dir);
    }
    let mut child = command
        .spawn()
        .map_err(|err| SummaryError::SummaryEngine(err.to_string()))?;

    match child.stdin.take() {
        Some(mut stdin) => {
            if let Err(err) = stdin.write_all(prompt.as_bytes()) {
                let _ = child.kill();
                let _ = child.wait();
                return Err(SummaryError::SummaryEngine(err.to_string()));
            }
            drop(stdin);
        }
        None => {
            let _ = child.kill();
            let _ = child.wait();
            return Err(SummaryError::SummaryEngine(
                "stdin pipe unexpectedly unavailable".to_owned(),
            ));
        }
    }

    let output = child
        .wait_with_output()
        .map_err(|err| SummaryError::SummaryEngine(err.to_string()))?;

    if !output.status.success() {
        return Err(summary_command_failed(
            client.harness,
            &output.status,
            &output.stderr,
            &output.stdout,
        ));
    }

    String::from_utf8(output.stdout).map_err(|err| SummaryError::SummaryEngine(err.to_string()))
}

fn summarize_opencode_argv(
    client: &HarnessCliSummaryClient,
    prompt: &str,
    workdir: Option<&Path>,
) -> Result<String, SummaryError> {
    let mut command = Command::new(&client.command_path);
    command
        .arg("run")
        .arg("--model")
        .arg(&client.model)
        .arg(prompt)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    if let Some(dir) = workdir {
        command.current_dir(dir);
    }
    let output = command
        .output()
        .map_err(|err| SummaryError::SummaryEngine(err.to_string()))?;
    if !output.status.success() {
        return Err(summary_command_failed(
            client.harness,
            &output.status,
            &output.stderr,
            &output.stdout,
        ));
    }
    String::from_utf8(output.stdout).map_err(|err| SummaryError::SummaryEngine(err.to_string()))
}

fn summarize_cursor_argv(
    client: &HarnessCliSummaryClient,
    prompt: &str,
    workdir: Option<&Path>,
) -> Result<String, SummaryError> {
    let mut command = Command::new(&client.command_path);
    command.arg("-p").arg("--output-format").arg("text");
    if !client.model.trim().is_empty() {
        command.arg("--model").arg(&client.model);
    }
    command
        .arg(prompt)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    if let Some(dir) = workdir {
        command.current_dir(dir);
    }
    let output = command
        .output()
        .map_err(|err| SummaryError::SummaryEngine(err.to_string()))?;
    if !output.status.success() {
        return Err(summary_command_failed(
            client.harness,
            &output.status,
            &output.stderr,
            &output.stdout,
        ));
    }
    String::from_utf8(output.stdout).map_err(|err| SummaryError::SummaryEngine(err.to_string()))
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
