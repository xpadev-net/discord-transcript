use crate::application::summary::{ClaudeSummaryClient, SummaryError};
use crate::bootstrap::config::SummaryHarness;
use crate::infrastructure::asr::{
    WhisperClient, WhisperInferenceRequest, WhisperParseError, WhisperTranscriptionResult,
    parse_whisper_response,
};
use crate::infrastructure::retry::{RetryPolicy, retry_with_backoff, retry_with_backoff_if};
use std::fmt::{Display, Formatter};
use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::path::Path;
use std::process::{Command, Output, Stdio};
use std::sync::OnceLock;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

pub const DEFAULT_COMMAND_TIMEOUT: Duration = Duration::from_secs(10 * 60);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IntegrationError {
    Io(String),
    NonZeroExit { code: i32, stderr: String },
    Timeout { timeout: Duration },
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
    pub command_timeout: Duration,
}

impl WhisperClient for CommandWhisperClient {
    fn infer(
        &self,
        request: &WhisperInferenceRequest,
    ) -> Result<WhisperTranscriptionResult, WhisperParseError> {
        let output = retry_with_backoff_if(self.retry_policy, |_| {
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

            run_command_with_timeout(&mut cmd, None, self.command_timeout)
                .map_err(|err| WhisperParseError::InvalidJson(err.to_string()))
                .and_then(|output| {
                    if output.status.success() {
                        Ok(output)
                    } else {
                        Err(WhisperParseError::InvalidJson(format!(
                            "whisper command failed: status={:?}, stderr={}",
                            output.status.code(),
                            sanitize_output(&output.stderr)
                        )))
                    }
                })
        }, |err| err.is_retriable())?;

        let body = String::from_utf8(output.stdout)
            .map_err(|err| WhisperParseError::InvalidJson(err.to_string()))?;
        parse_whisper_response(&body).map_err(|err| match err {
            WhisperParseError::InvalidJson(message) => WhisperParseError::InvalidJson(format!(
                "{message} (response body length: {} bytes)",
                body.len()
            )),
            other => other,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HarnessCliSummaryClient {
    pub harness: SummaryHarness,
    pub command_path: String,
    pub model: String,
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
    let mut command = Command::new(&client.command_path);
    command.arg("--model").arg(&client.model).arg("-p");
    if let Some(dir) = workdir {
        command.current_dir(dir);
    }
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
        .stdin(std::process::Stdio::null());
    if let Some(dir) = workdir {
        command.current_dir(dir);
    }
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
    String::from_utf8(output.stdout).map_err(|err| SummaryError::SummaryEngine(err.to_string()))
}

fn summarize_cursor_argv(
    client: &HarnessCliSummaryClient,
    prompt: &str,
    workdir: Option<&Path>,
) -> Result<String, SummaryError> {
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
    if let Some(dir) = workdir {
        command.current_dir(dir);
    }
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

fn summary_integration_error(err: IntegrationError) -> SummaryError {
    SummaryError::SummaryEngine(err.to_string())
}

pub fn run_command_with_timeout(
    command: &mut Command,
    stdin_input: Option<&[u8]>,
    timeout: Duration,
) -> Result<Output, IntegrationError> {
    let temp_paths = CommandOutputTempPaths::new();
    let stdout_file = temp_paths
        .create_stdout()
        .map_err(|err| IntegrationError::Io(err.to_string()))?;
    let stderr_file = match temp_paths.create_stderr() {
        Ok(file) => file,
        Err(err) => {
            temp_paths.cleanup();
            return Err(IntegrationError::Io(err.to_string()));
        }
    };
    let stdin_file = match stdin_input {
        Some(input) => match temp_paths.create_stdin(input) {
            Ok(file) => Some(file),
            Err(err) => {
                temp_paths.cleanup();
                return Err(IntegrationError::Io(err.to_string()));
            }
        },
        None => None,
    };

    let stdout_clone = match stdout_file.try_clone() {
        Ok(file) => file,
        Err(err) => {
            temp_paths.cleanup();
            return Err(IntegrationError::Io(err.to_string()));
        }
    };
    let stderr_clone = match stderr_file.try_clone() {
        Ok(file) => file,
        Err(err) => {
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
            temp_paths.cleanup();
            return Err(IntegrationError::Io(err.to_string()));
        }
    };

    let started = Instant::now();
    let status = loop {
        match child.try_wait() {
            Err(err) => {
                kill_child_process_group(&mut child);
                let _ = child.wait();
                temp_paths.cleanup();
                return Err(IntegrationError::Io(err.to_string()));
            }
            Ok(Some(status)) => break status,
            Ok(None) if started.elapsed() >= timeout => {
                kill_child_process_group(&mut child);
                let _ = child.wait();
                temp_paths.cleanup();
                return Err(IntegrationError::Timeout { timeout });
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(20)),
        }
    };

    drop(stdout_file);
    drop(stderr_file);
    let stdout = match temp_paths.read_stdout() {
        Ok(stdout) => stdout,
        Err(err) => {
            temp_paths.cleanup();
            return Err(IntegrationError::Io(err.to_string()));
        }
    };
    let stderr = match temp_paths.read_stderr() {
        Ok(stderr) => stderr,
        Err(err) => {
            temp_paths.cleanup();
            return Err(IntegrationError::Io(err.to_string()));
        }
    };
    temp_paths.cleanup();

    Ok(Output {
        status,
        stdout,
        stderr,
    })
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

    fn read_stdout(&self) -> std::io::Result<Vec<u8>> {
        read_temp_output_file(&self.stdout)
    }

    fn read_stderr(&self) -> std::io::Result<Vec<u8>> {
        read_temp_output_file(&self.stderr)
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

fn read_temp_output_file(path: &Path) -> std::io::Result<Vec<u8>> {
    let mut file = File::open(path)?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)?;
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::{
        CommandWhisperClient, IntegrationError, create_temp_output_file, run_command_with_timeout,
    };
    use crate::infrastructure::asr::{WhisperClient, WhisperInferenceRequest, WhisperParseError};
    use crate::infrastructure::retry::RetryPolicy;
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
    fn whisper_nonzero_exit_still_retries_command() {
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
