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
//! This module carries only the tool surface; the rig client that drives
//! the loop lands in a follow-up.

use std::fs;
use std::io::{ErrorKind, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use rig_core::tool::{DynamicTool, ToolExecutionError, ToolOutput};
use serde_json::{Value, json};

use crate::application::summary::SummaryError;
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

/// Filesystem surface the workspace tools expose to the model: reads and
/// listings under `input/`, writes under `output/`. All paths go through
/// `validate_agent_relative_path` and are re-checked against the
/// canonicalized workspace root so symlinked components cannot escape.
pub struct AgentToolFs {
    root: PathBuf,
    max_read_bytes: u64,
    max_write_bytes: u64,
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
            root,
            max_read_bytes: MAX_TOOL_READ_BYTES,
            max_write_bytes: MAX_TOOL_WRITE_BYTES,
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

    /// Resolve a model-supplied relative path to an absolute path under
    /// `root`, refusing anything that does not start with `required_first`.
    pub fn resolve(
        &self,
        relative: &str,
        required_first: &str,
    ) -> Result<PathBuf, ToolExecutionError> {
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

    pub fn list_inputs(&self) -> Result<Value, ToolExecutionError> {
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

    pub fn read_input(
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

    pub fn write_output(&self, relative: &str, contents: &str) -> Result<u64, ToolExecutionError> {
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

impl Drop for AgentToolFs {
    /// Join every started tool task before the fs goes away. A
    /// `spawn_blocking` task cannot be cancelled once running — dropping
    /// the fs (end of an attempt, workspace teardown) without this wait
    /// would let a timed-out write land on the workspace afterwards.
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
                    let inner = Arc::clone(&fs);
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
                    let inner = Arc::clone(&fs);
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
                    let inner = Arc::clone(&fs);
                    fs.spawn_tool(move || inner.write_output(&args.path, &args.contents))
                        .await
                        .map(|written| ToolOutput::text(format!("wrote {written} bytes")))
                })
            },
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
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
}
