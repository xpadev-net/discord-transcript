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
use std::io::ErrorKind;
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
        })
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
        let bytes = fs::read(&canonical).map_err(|err| {
            ToolExecutionError::other(format!("failed to read {relative}: {err}"))
        })?;
        let text = String::from_utf8(bytes).map_err(|_| {
            ToolExecutionError::invalid_args(format!("input file is not valid UTF-8: {relative}"))
        })?;
        let limit = limit.min(self.max_read_bytes) as usize;
        let mut start = offset.min(text.len() as u64) as usize;
        while start > 0 && !text.is_char_boundary(start) {
            start -= 1;
        }
        let mut end = (start + limit).min(text.len());
        while end > start && !text.is_char_boundary(end) {
            end -= 1;
        }
        Ok(text[start..end].to_owned())
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
                Box::pin(async move { fs.list_inputs().map(ToolOutput::json) })
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
                    fs.read_input(
                        &args.path,
                        args.offset.unwrap_or(0),
                        args.limit.unwrap_or(u64::MAX),
                    )
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
                    fs.write_output(&args.path, &args.contents)
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
}
