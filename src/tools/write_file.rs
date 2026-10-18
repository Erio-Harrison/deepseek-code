use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};
#[cfg(target_os = "macos")]
use std::time::Duration;

use super::{Tool, ToolError};
use crate::settings::SandboxGrants;

#[cfg(target_os = "macos")]
const TIMEOUT: Duration = Duration::from_secs(30);

pub struct WriteFileTool {
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    grants: Arc<Mutex<SandboxGrants>>,
}

impl WriteFileTool {
    pub fn new(grants: Arc<Mutex<SandboxGrants>>) -> Self {
        Self { grants }
    }
}

#[derive(Deserialize)]
struct WriteFileInput {
    path: String,
    content: String,
    /// Write outside the sandbox. `reason` is only for the host's prompt.
    #[serde(default)]
    escalate: bool,
}

#[async_trait]
impl Tool for WriteFileTool {
    fn name(&self) -> &str {
        "write_file"
    }

    fn description(&self) -> &str {
        "Overwrite a file at the given path with the given content, creating it (and its \
         parent directories) if it doesn't exist. Restricted to this project's directory; if \
         a write fails with what looks like a permission restriction, you may retry it with \
         `escalate: true` and a `reason` to write outside that boundary instead."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "Path to the file to write" },
                "content": { "type": "string", "description": "Full new contents of the file" },
                "escalate": {
                    "type": "boolean",
                    "description": "Set true to retry this exact write outside the sandbox, \
                        after a sandboxed attempt already failed with what looks like a \
                        permission restriction. The host will prompt the user to approve; \
                        don't set this speculatively on a first attempt."
                },
                "reason": {
                    "type": "string",
                    "description": "One sentence explaining why this needs to write outside \
                        the sandbox. Required when escalate is true."
                }
            },
            "required": ["path", "content"]
        })
    }

    async fn call(&self, input: Value) -> Result<String, ToolError> {
        let input: WriteFileInput =
            serde_json::from_value(input).map_err(|e| ToolError::InvalidInput(e.to_string()))?;

        #[cfg(target_os = "macos")]
        {
            if input.escalate {
                write_unsandboxed(input.path, input.content).await
            } else {
                write_sandboxed(input.path, input.content, &self.grants).await
            }
        }
        #[cfg(not(target_os = "macos"))]
        {
            write_unsandboxed(input.path, input.content).await
        }
    }
}

/// No isolation. Used off macOS, and for escalated calls.
async fn write_unsandboxed(path: String, content: String) -> Result<String, ToolError> {
    if let Some(parent) = std::path::Path::new(&path).parent()
        && !parent.as_os_str().is_empty()
    {
        tokio::fs::create_dir_all(parent).await?;
    }
    tokio::fs::write(&path, &content).await?;
    Ok(format!("wrote {} bytes to {}", content.len(), path))
}

/// Writes outside the workspace are refused by the OS sandbox. `mkdir`/`tee`
/// avoid shell-quoting `path`.
#[cfg(target_os = "macos")]
async fn write_sandboxed(
    path: String,
    content: String,
    grants: &Arc<Mutex<SandboxGrants>>,
) -> Result<String, ToolError> {
    let workspace = std::env::current_dir()?;
    let grants = grants.lock().unwrap().clone();
    let byte_len = content.len();
    let parent = std::path::Path::new(&path)
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .map(|p| p.to_path_buf());

    let path_for_sandbox = path.clone();
    let result = tokio::task::spawn_blocking(move || {
        let sandbox = super::agent_sandbox_builder(&workspace, &grants)
            .wall_time_limit(TIMEOUT)
            .no_network()
            .build()
            .map_err(|e| ToolError::Sandbox(e.to_string()))?;

        if let Some(parent) = &parent {
            let parent_str = parent.to_string_lossy().into_owned();
            let mkdir = sandbox
                .run("mkdir", &["-p", "--", &parent_str])
                .map_err(|e| ToolError::Sandbox(e.to_string()))?;
            if mkdir.exit_code != 0 {
                return Err(ToolError::Sandbox(format!(
                    "mkdir -p {parent_str} failed: {}",
                    mkdir.stderr
                )));
            }
        }

        sandbox
            .run_with_input(
                "tee",
                &["--", path_for_sandbox.as_str()],
                Some(content.as_bytes()),
            )
            .map_err(|e| ToolError::Sandbox(e.to_string()))
    })
    .await
    .map_err(|e| ToolError::Sandbox(format!("sandbox task panicked: {e}")))??;

    if result.killed_by_timeout {
        return Err(ToolError::Timeout(TIMEOUT));
    }
    if result.exit_code != 0 {
        return Err(ToolError::Sandbox(if result.stderr.is_empty() {
            format!("tee exited with code {}", result.exit_code)
        } else {
            result.stderr
        }));
    }

    Ok(format!("wrote {byte_len} bytes to {path}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tool() -> WriteFileTool {
        WriteFileTool::new(Arc::new(Mutex::new(SandboxGrants::default())))
    }

    #[tokio::test]
    async fn writes_and_creates_parent_dirs() {
        let dir =
            std::env::temp_dir().join(format!("deepseek_code_test_write_{}", std::process::id()));
        let path = dir.join("nested/out.txt");

        tool()
            .call(json!({ "path": path.to_str().unwrap(), "content": "abc" }))
            .await
            .unwrap();

        let written = tokio::fs::read_to_string(&path).await.unwrap();
        assert_eq!(written, "abc");

        tokio::fs::remove_dir_all(&dir).await.unwrap();
    }

    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn writes_relative_paths_that_start_with_a_dash() {
        let dir = format!("-deepseek_code_dash_test_{}", std::process::id());
        let path = format!("{dir}/-a.txt");

        let result = tool().call(json!({ "path": path, "content": "x" })).await;
        assert!(result.is_ok(), "{result:?}");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "x");

        std::fs::remove_dir_all(&dir).unwrap();
    }

    // Outside-workspace write restriction is macOS-specific (only nanosandbox
    // enforces it); non-macOS runs everything unsandboxed already.
    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn escalate_bypasses_the_sandbox_write_restriction() {
        let outside = crate::tools::OutsideDir::new("write-escalate");
        let target = outside.0.join("x.txt");

        let blocked = tool()
            .call(json!({ "path": target.to_str().unwrap(), "content": "x" }))
            .await;
        assert!(blocked.is_err(), "{blocked:?}");
        assert!(!target.exists());

        let allowed = tool()
            .call(json!({ "path": target.to_str().unwrap(), "content": "x", "escalate": true }))
            .await;
        assert!(allowed.is_ok(), "{allowed:?}");
        assert!(target.exists());
    }

    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn session_grant_allows_writes_without_the_escalate_flag() {
        let outside = crate::tools::OutsideDir::new("write-grant");
        let target = outside.0.join("x.txt");
        let grants = Arc::new(Mutex::new(SandboxGrants {
            extra_writable: [outside.0.clone()].into_iter().collect(),
            ..Default::default()
        }));

        let result = WriteFileTool::new(grants)
            .call(json!({ "path": target.to_str().unwrap(), "content": "x" }))
            .await;
        assert!(result.is_ok(), "{result:?}");
        assert!(target.exists());
    }
}
