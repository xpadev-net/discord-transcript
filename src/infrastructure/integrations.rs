use crate::application::summary::{AgentOutputContract, SummaryError};
use crate::bootstrap::config::SummaryHarness;
use crate::infrastructure::asr::{
    WhisperClient, WhisperInferenceRequest, WhisperParseError, WhisperTranscriptionResult,
    parse_whisper_response,
};
use crate::infrastructure::retry::{RetryPolicy, retry_with_backoff_if};
use crate::infrastructure::storage_s3::RecordingObjectStore;
use crate::infrastructure::workspace::AGENT_OUTPUT_DIR;
use futures_util::StreamExt;
use std::fmt::{Display, Formatter};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::Path;
use std::process::{Command, Output, Stdio};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};
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

/// Whisper adapter that can forward trusted S3 audio without materializing
/// the object in memory or in the local workspace. Local-file requests keep
/// using the existing curl adapter.
#[derive(Clone)]
pub struct S3StreamingWhisperClient {
    local: CommandWhisperClient,
    objects: Option<RecordingObjectStore>,
    http: reqwest::Client,
}

impl std::fmt::Debug for S3StreamingWhisperClient {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("S3StreamingWhisperClient")
            .field(
                "whisper_endpoint",
                &sanitize_whisper_endpoint_for_log(&self.local.endpoint),
            )
            .field("s3_configured", &self.objects.is_some())
            .finish_non_exhaustive()
    }
}

impl S3StreamingWhisperClient {
    pub fn new(
        local: CommandWhisperClient,
        objects: Option<RecordingObjectStore>,
    ) -> Result<Self, WhisperParseError> {
        let http = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(5))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|_| {
                WhisperParseError::InvalidJson(
                    "failed to initialize streaming Whisper HTTP client".to_owned(),
                )
            })?;
        Ok(Self {
            local,
            objects,
            http,
        })
    }

    fn infer_s3(
        &self,
        request: &crate::application::asr::AsrRequest,
        key: &str,
    ) -> Result<WhisperTranscriptionResult, WhisperParseError> {
        let objects = self.objects.as_ref().ok_or_else(|| {
            WhisperParseError::InvalidAudioSource(
                "S3 audio source requires configured object storage".to_owned(),
            )
        })?;
        // The request contains an object key, never a caller-supplied URL.
        // Keep the short-lived signed URL out of diagnostics and error text.
        let source_url = objects.presigned_get(key);
        let client = self.http.clone();
        let whisper_endpoint = whisper_inference_endpoint(&self.local.endpoint);
        let language = request.language.clone();
        let prompt =
            compose_whisper_prompt(self.local.prompt.as_deref(), request.prompt.as_deref());
        let beam_size = self.local.beam_size;
        let suppress_non_speech = self.local.suppress_non_speech;
        let vad = self.local.vad;
        let temperature = self.local.temperature;
        run_async_blocking(async move {
            stream_s3_audio_to_whisper(
                &client,
                &source_url,
                &whisper_endpoint,
                WhisperFormOptions {
                    language: language.as_deref(),
                    prompt: prompt.as_deref(),
                    beam_size,
                    suppress_non_speech,
                    vad,
                    temperature,
                },
                self.local.command_timeout,
            )
            .await
        })
    }
}

impl crate::application::asr::AsrPort for S3StreamingWhisperClient {
    fn infer(
        &self,
        request: &crate::application::asr::AsrRequest,
    ) -> Result<WhisperTranscriptionResult, WhisperParseError> {
        match &request.source {
            crate::application::asr::AudioSource::LocalFile(_) => {
                crate::application::asr::AsrPort::infer(&self.local, request)
            }
            crate::application::asr::AudioSource::S3Object(reference) => retry_with_backoff_if(
                self.local.retry_policy,
                |_| self.infer_s3(request, reference.key()),
                WhisperParseError::is_retriable,
            ),
        }
    }
}

struct WhisperFormOptions<'a> {
    language: Option<&'a str>,
    prompt: Option<&'a str>,
    beam_size: u32,
    suppress_non_speech: bool,
    vad: bool,
    temperature: f32,
}

async fn stream_s3_audio_to_whisper(
    client: &reqwest::Client,
    source_url: &str,
    whisper_endpoint: &str,
    options: WhisperFormOptions<'_>,
    timeout: Duration,
) -> Result<WhisperTranscriptionResult, WhisperParseError> {
    let source = client
        .get(source_url)
        .timeout(timeout)
        .send()
        .await
        .map_err(|_| WhisperParseError::InvalidJson("S3 audio GET transport failure".to_owned()))?;
    if !source.status().is_success() {
        return Err(WhisperParseError::InvalidJson(format!(
            "S3 audio GET failed: status={}",
            source.status().as_u16()
        )));
    }
    let content_length = source.content_length().ok_or_else(|| {
        WhisperParseError::InvalidAudioSource(
            "S3 audio GET response omitted Content-Length".to_owned(),
        )
    })?;
    let (source_done_tx, mut source_done_rx) = tokio::sync::oneshot::channel();
    let source_failed = std::sync::Arc::new(AtomicBool::new(false));
    let source_failed_in_stream = source_failed.clone();
    let source_stream = futures_util::stream::unfold(
        (
            source.bytes_stream(),
            Some(source_done_tx),
            source_failed_in_stream,
        ),
        |(mut bytes, done, failed)| async move {
            match bytes.next().await {
                Some(Ok(chunk)) => Some((Ok(chunk), (bytes, done, failed))),
                Some(Err(_)) => {
                    failed.store(true, Ordering::Release);
                    Some((
                        Err(std::io::Error::other("S3 audio GET body read failure")),
                        (bytes, done, failed),
                    ))
                }
                None => {
                    if let Some(done) = done {
                        let _ = done.send(());
                    }
                    None
                }
            }
        },
    );
    let source_body = reqwest::Body::wrap_stream(source_stream);
    let file = reqwest::multipart::Part::stream_with_length(source_body, content_length)
        .file_name("audio.wav")
        .mime_str("audio/wav")
        .map_err(|_| {
            WhisperParseError::InvalidAudioSource("failed to build audio multipart part".to_owned())
        })?;
    let mut form = reqwest::multipart::Form::new()
        .part("file", file)
        .text("response_format", "verbose_json")
        .text("beam_size", options.beam_size.to_string())
        .text(
            "suppress_non_speech",
            options.suppress_non_speech.to_string(),
        )
        .text("vad", options.vad.to_string())
        .text("temperature", options.temperature.to_string());
    if let Some(language) = options.language {
        form = form.text("language", language.to_owned());
    }
    if let Some(prompt) = options.prompt {
        form = form.text("prompt", prompt.to_owned());
    }

    let post = client.post(whisper_endpoint).multipart(form).send();
    tokio::pin!(post);
    let (response, whisper_deadline) = tokio::select! {
        transfer = tokio::time::timeout(timeout, &mut source_done_rx) => match transfer {
            Err(_) => return Err(WhisperParseError::InvalidJson("S3 audio GET body timed out".to_owned())),
            Ok(Err(_)) if source_failed.load(Ordering::Acquire) => return Err(WhisperParseError::InvalidJson("S3 audio GET body read failure".to_owned())),
            Ok(Err(_)) => return Err(WhisperParseError::InvalidJson("S3 audio GET body stream ended unexpectedly".to_owned())),
            Ok(Ok(())) => {
                let deadline = tokio::time::Instant::now() + timeout;
                let response = tokio::time::timeout_at(deadline, &mut post).await
                    .map_err(|_| WhisperParseError::InvalidJson("Whisper streaming POST timed out".to_owned()))?
                    .map_err(|_| WhisperParseError::InvalidJson("Whisper streaming POST transport failure".to_owned()))?;
                (response, deadline)
            },
        },
        result = &mut post => {
            let response = result.map_err(|_| {
                if source_failed.load(Ordering::Acquire) {
                    WhisperParseError::InvalidJson("S3 audio GET body read failure".to_owned())
                } else {
                    WhisperParseError::InvalidJson("Whisper streaming POST transport failure".to_owned())
                }
            })?;
            (response, tokio::time::Instant::now() + timeout)
        },
    };
    if !response.status().is_success() {
        return Err(WhisperParseError::InvalidJson(format!(
            "Whisper streaming POST failed: status={}",
            response.status().as_u16()
        )));
    }
    let mut response_stream = response.bytes_stream();
    let mut body = Vec::new();
    loop {
        let chunk = tokio::time::timeout_at(whisper_deadline, response_stream.next())
            .await
            .map_err(|_| {
                WhisperParseError::InvalidJson(
                    "Whisper streaming POST response timed out".to_owned(),
                )
            })?;
        let Some(chunk) = chunk else {
            break;
        };
        let chunk = chunk.map_err(|_| {
            WhisperParseError::InvalidJson("Whisper streaming response body read failed".to_owned())
        })?;
        if body.len().saturating_add(chunk.len()) > DEFAULT_COMMAND_OUTPUT_STREAM_MAX_BYTES as usize
        {
            return Err(WhisperParseError::InvalidJson(
                "malformed successful whisper response: response body exceeded size limit"
                    .to_owned(),
            ));
        }
        body.extend_from_slice(&chunk);
    }
    parse_successful_whisper_body(body)
}

fn run_async_blocking<F>(future: F) -> F::Output
where
    F: std::future::Future + Send,
    F::Output: Send,
{
    let run = || {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("streaming ASR runtime must build")
            .block_on(future)
    };
    if tokio::runtime::Handle::try_current().is_ok() {
        std::thread::scope(|scope| match scope.spawn(run).join() {
            Ok(output) => output,
            Err(payload) => std::panic::resume_unwind(payload),
        })
    } else {
        run()
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

    // Flag the name when a sensitive word appears between two word
    // boundaries: run start/end, non-alphanumeric separators, lower→upper
    // (camelCase) starts, and acronym→word boundaries (an uppercase run's
    // last letter before a lowercase, e.g. `APISecret` → `API` + `Secret`).
    // A match may span inner boundaries it does not need (`SEcret` splits as
    // `S` + `Ecret`, yet its lowercase form is still `secret`), so
    // `accessToken`, `clientSecret`, `clientAPISecret`, and `clientSEcret`
    // all hit even though the normalized name has no separator.
    const SENSITIVE_WORDS: &[&str] = &[
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
        "key",
    ];
    let chars = name.chars().collect::<Vec<_>>();
    let len = chars.len();
    let mut boundary = vec![false; len + 1];
    boundary[0] = true;
    boundary[len] = true;
    for i in 1..len {
        let ch = chars[i];
        let prev = chars[i - 1];
        if !ch.is_ascii_alphanumeric() || !prev.is_ascii_alphanumeric() {
            boundary[i] = true;
            continue;
        }
        if ch.is_ascii_uppercase()
            && (!prev.is_ascii_uppercase()
                || chars
                    .get(i + 1)
                    .is_some_and(|next| next.is_ascii_lowercase()))
        {
            boundary[i] = true;
        }
    }
    let lower = chars
        .iter()
        .map(|ch| ch.to_ascii_lowercase())
        .collect::<Vec<_>>();
    for start in 0..len {
        if !boundary[start] {
            continue;
        }
        for word in SENSITIVE_WORDS {
            let end = start + word.len();
            if end <= len && boundary[end] && lower[start..end].iter().copied().eq(word.chars()) {
                return true;
            }
        }
    }
    false
}

fn quote_log_arg(part: &str) -> String {
    if part.contains(char::is_whitespace) || part.contains('"') || part.contains('\\') {
        format!("\"{}\"", part.replace('\\', "\\\\").replace('"', "\\\""))
    } else {
        part.to_owned()
    }
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

pub(crate) fn remove_stale_agent_output(
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

pub(crate) fn read_validated_agent_output(
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
        CommandOutputReadError, CommandOutputStream, CommandWhisperClient, IntegrationError,
        S3StreamingWhisperClient, create_temp_output_file, read_temp_output_file,
        run_command_with_timeout, run_command_with_timeout_and_output_limit,
        sanitize_whisper_endpoint_for_log,
    };
    use crate::application::asr::{AsrError, AsrRequest, AudioSource, S3AudioObjectReference};
    use crate::infrastructure::asr::{WhisperClient, WhisperInferenceRequest, WhisperParseError};
    use crate::infrastructure::retry::RetryPolicy;
    use crate::infrastructure::s3::{ObjectStore, S3Error};
    use crate::infrastructure::storage_s3::RecordingObjectStore;
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::process::Command;
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    #[derive(Debug)]
    struct PresignedUrlStore(String);

    impl ObjectStore for PresignedUrlStore {
        fn put_object(&self, _: &str, _: &[u8], _: &str) -> Result<(), S3Error> {
            Ok(())
        }

        fn head_object(&self, _: &str) -> Result<bool, S3Error> {
            Ok(true)
        }

        fn list_keys(&self, _: &str) -> Result<Vec<String>, S3Error> {
            Ok(Vec::new())
        }

        fn delete_keys(&self, _: &[String]) -> Result<(), S3Error> {
            Ok(())
        }

        fn presigned_get_url(&self, _key: &str, _ttl: Duration) -> String {
            self.0.clone()
        }

        fn endpoint_label(&self) -> String {
            "test".to_owned()
        }
    }

    fn test_s3_whisper_client(
        s3_url: String,
        whisper_endpoint: String,
    ) -> S3StreamingWhisperClient {
        let objects = RecordingObjectStore::new(
            Arc::new(PresignedUrlStore(s3_url)),
            String::new(),
            60,
            std::env::temp_dir(),
        );
        S3StreamingWhisperClient::new(
            CommandWhisperClient {
                endpoint: whisper_endpoint,
                curl_bin: "unused-for-s3-test".to_owned(),
                retry_policy: RetryPolicy {
                    max_attempts: 1,
                    ..RetryPolicy::default()
                },
                beam_size: 3,
                suppress_non_speech: true,
                prompt: Some("configured prompt".to_owned()),
                vad: true,
                temperature: 0.2,
                command_timeout: Duration::from_secs(5),
            },
            Some(objects),
        )
        .expect("streaming client should initialize")
    }

    fn read_http_request(reader: &mut BufReader<TcpStream>) -> (String, Vec<u8>) {
        let mut headers = String::new();
        loop {
            let mut line = String::new();
            reader.read_line(&mut line).expect("read HTTP header");
            if line.is_empty() {
                break;
            }
            let done = line == "\r\n";
            headers.push_str(&line);
            if done {
                break;
            }
        }
        let content_length = headers.lines().find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse::<usize>().ok())
                .flatten()
        });
        let mut body = vec![0; content_length.unwrap_or_default()];
        reader.read_exact(&mut body).expect("read HTTP body");
        (headers, body)
    }

    fn s3_audio_request() -> AsrRequest {
        AsrRequest {
            source: AudioSource::S3Object(
                S3AudioObjectReference::new(
                    "meetings/meeting-1/audio/alice.wav",
                    Some("alice".to_owned()),
                    Some(1_250),
                )
                .unwrap(),
            ),
            language: Some("ja".to_owned()),
            prompt: Some("meeting prompt".to_owned()),
        }
    }

    #[test]
    fn s3_audio_streams_into_whisper_multipart_without_exposing_signed_url() {
        let s3_listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let s3_address = s3_listener.local_addr().unwrap();
        let audio = (0..1024 * 128)
            .map(|index| (index % 251) as u8)
            .collect::<Vec<_>>();
        let s3_audio = audio.clone();
        let s3_server = std::thread::spawn(move || {
            let (stream, _) = s3_listener.accept().unwrap();
            let mut request = BufReader::new(stream);
            let (headers, _) = read_http_request(&mut request);
            assert!(headers.starts_with("GET /audio.wav?X-Amz-Signature=private-signature "));
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                s3_audio.len()
            );
            request.get_mut().write_all(response.as_bytes()).unwrap();
            request.get_mut().write_all(&s3_audio).unwrap();
        });

        let whisper_listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let whisper_address = whisper_listener.local_addr().unwrap();
        let whisper_server = std::thread::spawn(move || {
            let (stream, _) = whisper_listener.accept().unwrap();
            let mut request = BufReader::new(stream);
            let (headers, body) = read_http_request(&mut request);
            assert!(headers.starts_with("POST /inference "));
            assert!(headers.to_ascii_lowercase().contains("content-length:"));
            assert!(
                String::from_utf8_lossy(&body).contains("name=\"file\"; filename=\"audio.wav\"")
            );
            assert!(String::from_utf8_lossy(&body).contains("name=\"language\"\r\n\r\nja"));
            assert!(String::from_utf8_lossy(&body).contains("configured prompt\nmeeting prompt"));
            assert!(body.windows(audio.len()).any(|window| window == audio));
            let response_body = r#"{"text":"ok","segments":[{"speaker":"alice","start":0.0,"end":1.0,"text":"ok"}]}"#;
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                response_body.len(),
                response_body
            );
            request.get_mut().write_all(response.as_bytes()).unwrap();
        });

        let client = test_s3_whisper_client(
            format!("http://{s3_address}/audio.wav?X-Amz-Signature=private-signature"),
            format!("http://{whisper_address}"),
        );
        assert!(!format!("{client:?}").contains("private-signature"));
        let debug_client = test_s3_whisper_client(
            format!("http://{s3_address}/audio.wav"),
            format!("http://user:password@{whisper_address}/?api_key=secret-token"),
        );
        let debug_output = format!("{debug_client:?}");
        assert!(!debug_output.contains("user:password"));
        assert!(!debug_output.contains("secret-token"));
        assert!(debug_output.contains("REDACTED"));
        assert!(!format!("{:?}", s3_audio_request()).contains("meeting prompt"));
        let result = crate::application::asr::AsrPort::infer(&client, &s3_audio_request())
            .expect("S3 audio should be forwarded to Whisper");
        s3_server.join().unwrap();
        whisper_server.join().unwrap();

        assert_eq!(result.text, "ok");
        assert_eq!(result.segments[0].speaker_id, "alice");
    }

    #[test]
    fn s3_get_failure_is_reported_without_calling_whisper_or_leaking_signature() {
        let s3_listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let s3_address = s3_listener.local_addr().unwrap();
        let s3_server = std::thread::spawn(move || {
            let (stream, _) = s3_listener.accept().unwrap();
            let mut request = BufReader::new(stream);
            let _ = read_http_request(&mut request);
            request
                .get_mut()
                .write_all(
                    b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                )
                .unwrap();
        });
        let whisper_listener = TcpListener::bind("127.0.0.1:0").unwrap();
        whisper_listener.set_nonblocking(true).unwrap();
        let whisper_address = whisper_listener.local_addr().unwrap();
        let client = test_s3_whisper_client(
            format!("http://{s3_address}/audio.wav?X-Amz-Signature=private-signature"),
            format!("http://{whisper_address}"),
        );

        let error = crate::application::asr::AsrPort::infer(&client, &s3_audio_request())
            .expect_err("missing source object must fail");
        s3_server.join().unwrap();
        assert!(
            matches!(error, AsrError::InvalidJson(ref message) if message.contains("S3 audio GET failed: status=404"))
        );
        assert!(!error.to_string().contains("private-signature"));
        assert!(matches!(
            whisper_listener.accept(),
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock
        ));
    }

    #[test]
    fn s3_body_read_failure_is_not_reported_as_whisper_transport_failure() {
        let s3_listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let s3_address = s3_listener.local_addr().unwrap();
        let s3_server = std::thread::spawn(move || {
            let (stream, _) = s3_listener.accept().unwrap();
            let mut request = BufReader::new(stream);
            let _ = read_http_request(&mut request);
            request
                .get_mut()
                .write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\nConnection: close\r\n\r\nshort",
                )
                .unwrap();
        });
        let whisper_listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let whisper_address = whisper_listener.local_addr().unwrap();
        let client = test_s3_whisper_client(
            format!("http://{s3_address}/audio.wav?X-Amz-Signature=private-signature"),
            format!("http://{whisper_address}"),
        );

        let error = crate::application::asr::AsrPort::infer(&client, &s3_audio_request())
            .expect_err("truncated S3 body must fail");
        s3_server.join().unwrap();
        assert!(
            matches!(error, AsrError::InvalidJson(ref message) if message.contains("S3 audio GET body read failure"))
        );
        assert!(!error.to_string().contains("Whisper streaming POST"));
    }

    #[test]
    fn command_whisper_adapter_rejects_s3_sources_without_treating_keys_as_urls() {
        let client = CommandWhisperClient {
            endpoint: "http://localhost".to_owned(),
            curl_bin: "must-not-run".to_owned(),
            retry_policy: RetryPolicy::default(),
            beam_size: 1,
            suppress_non_speech: false,
            prompt: None,
            vad: false,
            temperature: 0.0,
            command_timeout: Duration::from_secs(1),
        };
        let request = AsrRequest {
            source: AudioSource::S3Object(
                S3AudioObjectReference::new(
                    "meetings/meeting-1/audio/alice.wav",
                    Some("alice".to_owned()),
                    Some(1_250),
                )
                .unwrap(),
            ),
            language: None,
            prompt: None,
        };

        assert!(matches!(
            crate::application::asr::AsrPort::infer(&client, &request),
            Err(AsrError::InvalidAudioSource(_))
        ));
    }

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
    fn whisper_endpoint_redaction_catches_camel_case_secret_params() {
        let rendered = sanitize_whisper_endpoint_for_log(
            "https://whisper.example.test/inference?accessToken=tok-1&clientSecret=sec-2&apiKey=key-3&sessionToken=s-4&awsSecretAccessKey=k-5&clientAPISecret=sec-6&clientSEcret=sec-7&debug=true&designMode=on",
        );

        assert_eq!(
            rendered,
            "https://whisper.example.test/inference?accessToken=REDACTED&clientSecret=REDACTED&apiKey=REDACTED&sessionToken=REDACTED&awsSecretAccessKey=REDACTED&clientAPISecret=REDACTED&clientSEcret=REDACTED&debug=true&designMode=on"
        );
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
