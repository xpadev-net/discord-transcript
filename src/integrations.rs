use crate::asr::{
    WhisperClient, WhisperInferenceRequest, WhisperParseError, WhisperTranscriptionResult,
    parse_whisper_response,
};
use crate::retry::{RetryPolicy, retry_with_backoff};
use crate::summary::{ClaudeSummaryClient, SummaryError};
use std::fmt::{Display, Formatter};
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandWhisperClient {
    pub endpoint: String,
    pub curl_bin: String,
    pub retry_policy: RetryPolicy,
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
                .arg(format!("audio=@{}", request.audio_path));

            if let Some(language) = &request.language {
                cmd.arg("-F").arg(format!("language={language}"));
            }

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
            parse_whisper_response(&body)
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClaudeCliSummaryClient {
    pub command_path: String,
    pub retry_policy: RetryPolicy,
}

impl ClaudeSummaryClient for ClaudeCliSummaryClient {
    fn summarize(&self, prompt: &str) -> Result<String, SummaryError> {
        retry_with_backoff(self.retry_policy, |_| {
            use std::io::Write;
            let mut child = Command::new(&self.command_path)
                .arg("-p")
                .arg("-")
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .spawn()
                .map_err(|err| SummaryError::SummaryEngine(err.to_string()))?;

            if let Some(mut stdin) = child.stdin.take()
                && let Err(err) = stdin.write_all(prompt.as_bytes())
            {
                let _ = child.kill();
                let _ = child.wait();
                return Err(SummaryError::SummaryEngine(err.to_string()));
            }

            let output = child
                .wait_with_output()
                .map_err(|err| SummaryError::SummaryEngine(err.to_string()))?;

            if !output.status.success() {
                return Err(SummaryError::SummaryEngine(format!(
                    "claude command failed: status={:?}, stderr={}",
                    output.status.code(),
                    String::from_utf8_lossy(&output.stderr)
                )));
            }

            String::from_utf8(output.stdout)
                .map_err(|err| SummaryError::SummaryEngine(err.to_string()))
        })
    }
}
