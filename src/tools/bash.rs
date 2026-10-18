use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use super::{Tool, ToolError};
use crate::settings::SandboxGrants;

const TIMEOUT: Duration = Duration::from_secs(120);

/// Per stream; everything a command prints becomes model context.
#[cfg(target_os = "macos")]
const MAX_OUTPUT: u64 = 1 << 20;

pub struct BashTool {
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    grants: Arc<Mutex<SandboxGrants>>,
}

impl BashTool {
    pub fn new(grants: Arc<Mutex<SandboxGrants>>) -> Self {
        Self { grants }
    }
}

#[derive(Deserialize)]
struct BashInput {
    command: String,
    /// Run outside the sandbox. `reason` is only for the host's prompt.
    #[serde(default)]
    escalate: bool,
}

#[async_trait]
impl Tool for BashTool {
    fn name(&self) -> &str {
        "bash"
    }

    fn description(&self) -> &str {
        "Run a shell command via `bash -c` and return its combined stdout/stderr output. \
         Times out after 120 seconds. Runs inside a sandbox restricted to this project's \
         directory with no network access; if a command fails with what looks like a \
         permission or network restriction, you may retry it with `escalate: true` and a \
         `reason` to run it outside the sandbox instead."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "command": { "type": "string", "description": "Shell command to execute" },
                "escalate": {
                    "type": "boolean",
                    "description": "Set true to retry this exact command outside the sandbox, \
                        after a sandboxed attempt already failed with what looks like a \
                        permission/network restriction. The host will prompt the user to \
                        approve; don't set this speculatively on a first attempt."
                },
                "reason": {
                    "type": "string",
                    "description": "One sentence explaining why this needs to run outside the \
                        sandbox. Required when escalate is true."
                }
            },
            "required": ["command"]
        })
    }

    async fn call(&self, input: Value) -> Result<String, ToolError> {
        let input: BashInput =
            serde_json::from_value(input).map_err(|e| ToolError::InvalidInput(e.to_string()))?;

        #[cfg(target_os = "macos")]
        {
            let unsandboxed = input.escalate || self.grants.lock().unwrap().unsandboxed_bash;
            if unsandboxed {
                run_unsandboxed(input.command).await
            } else {
                run_sandboxed(input.command, &self.grants).await
            }
        }
        #[cfg(not(target_os = "macos"))]
        {
            run_unsandboxed(input.command).await
        }
    }
}

/// No isolation. Used off macOS, and for escalated calls.
async fn run_unsandboxed(command: String) -> Result<String, ToolError> {
    let child = tokio::process::Command::new("bash")
        .arg("-c")
        .arg(&command)
        .output();
    let output = tokio::time::timeout(TIMEOUT, child)
        .await
        .map_err(|_| ToolError::Timeout(TIMEOUT))??;

    let mut result = String::from_utf8_lossy(&output.stdout).into_owned();
    result.push_str(&String::from_utf8_lossy(&output.stderr));
    if let Some(code) = output.status.code()
        && code != 0
    {
        result.push_str(&format!("\n[exit code: {code}]"));
    }
    Ok(result)
}

/// Workspace-scoped, no network. `Sandbox::run` blocks, hence `spawn_blocking`.
#[cfg(target_os = "macos")]
async fn run_sandboxed(
    command: String,
    grants: &Arc<Mutex<SandboxGrants>>,
) -> Result<String, ToolError> {
    let workspace = std::env::current_dir()?;
    let grants = grants.lock().unwrap().clone();

    let result = tokio::task::spawn_blocking(move || {
        let sandbox = super::agent_sandbox_builder(&workspace, &grants)
            .wall_time_limit(TIMEOUT)
            .max_output(MAX_OUTPUT)
            .no_network()
            .build()
            .map_err(|e| ToolError::Sandbox(e.to_string()))?;
        sandbox
            .run("bash", &["-c", &command])
            .map_err(|e| ToolError::Sandbox(e.to_string()))
    })
    .await
    .map_err(|e| ToolError::Sandbox(format!("sandbox task panicked: {e}")))??;

    if result.killed_by_timeout {
        return Err(ToolError::Timeout(TIMEOUT));
    }

    let failure_reason = result.failure_reason();
    let mut output = result.stdout;
    output.push_str(&result.stderr);
    if result.output_truncated {
        output.push_str(&format!(
            "\n[output truncated: more than {} bytes on stdout or stderr were dropped]",
            MAX_OUTPUT
        ));
    }
    if result.killed_by_oom || result.killed_by_tmp_limit || result.killed_by_cpu_limit {
        output.push_str(&format!(
            "\n[killed by the sandbox: {}]",
            failure_reason.unwrap_or_default()
        ));
    } else if let Some(signal) = result.signal {
        output.push_str(&format!("\n[killed by signal: {signal}]"));
    } else if result.exit_code != 0 {
        output.push_str(&format!("\n[exit code: {}]", result.exit_code));
    }
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tool() -> BashTool {
        BashTool::new(Arc::new(Mutex::new(SandboxGrants::default())))
    }

    #[tokio::test]
    async fn runs_command_and_captures_stdout() {
        let result = tool().call(json!({ "command": "echo hello" })).await.unwrap();
        assert_eq!(result.trim(), "hello");
    }

    #[tokio::test]
    async fn non_zero_exit_reports_code_but_is_not_an_error() {
        let result = tool().call(json!({ "command": "exit 3" })).await.unwrap();
        assert!(result.contains("[exit code: 3]"));
    }

    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn sandbox_sees_the_hosts_path() {
        let host_path = std::env::var("PATH").unwrap();
        let result = tool().call(json!({ "command": "echo \"$PATH\"" })).await.unwrap();
        assert_eq!(result.trim(), host_path);
    }

    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn sandbox_does_not_see_other_host_variables() {
        unsafe { std::env::set_var("DEEPSEEK_CODE_TEST_SECRET", "s3cret") };
        let result = tool()
            .call(json!({ "command": "echo \"[${DEEPSEEK_CODE_TEST_SECRET:-unset}]\"" }))
            .await
            .unwrap();
        assert_eq!(result.trim(), "[unset]");
    }

    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn a_grant_on_a_missing_directory_does_not_break_the_sandbox() {
        let grants = Arc::new(Mutex::new(SandboxGrants {
            extra_writable: [std::path::PathBuf::from("/nonexistent/deepseek_code/grant")]
                .into_iter()
                .collect(),
            ..Default::default()
        }));
        let result = BashTool::new(grants)
            .call(json!({ "command": "echo still-ok" }))
            .await
            .unwrap();
        assert_eq!(result.trim(), "still-ok");
    }

    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn oversized_output_is_truncated_and_labelled() {
        let result = tool()
            .call(json!({ "command": "head -c 3000000 /dev/zero | tr '\\0' x" }))
            .await
            .unwrap();
        assert!(result.len() < 2_000_000, "{}", result.len());
        assert!(result.contains("[output truncated"), "{}", &result[result.len() - 120..]);
    }

    // Outside-workspace write restriction is macOS-specific (only nanosandbox
    // enforces it); non-macOS runs everything unsandboxed already.
    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn escalate_bypasses_the_sandbox_write_restriction() {
        let outside = crate::tools::OutsideDir::new("bash-escalate");
        let target = outside.0.join("x.txt");
        let cmd = format!("echo hi > {}", target.display());

        let blocked = tool().call(json!({ "command": cmd })).await.unwrap();
        assert!(blocked.contains("Operation not permitted"), "{blocked}");
        assert!(!target.exists());

        let allowed = tool()
            .call(json!({ "command": cmd, "escalate": true }))
            .await
            .unwrap();
        assert!(!allowed.contains("Operation not permitted"), "{allowed}");
        assert!(target.exists());
    }

    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn unsandboxed_bash_grant_bypasses_sandbox_without_the_escalate_flag() {
        let outside = crate::tools::OutsideDir::new("bash-grant");
        let target = outside.0.join("x.txt");
        let cmd = format!("echo hi > {}", target.display());

        let grants = Arc::new(Mutex::new(SandboxGrants {
            unsandboxed_bash: true,
            ..Default::default()
        }));
        let result = BashTool::new(grants)
            .call(json!({ "command": cmd }))
            .await
            .unwrap();

        assert!(!result.contains("Operation not permitted"), "{result}");
        assert!(target.exists());
    }
}
