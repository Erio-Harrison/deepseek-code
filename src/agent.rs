//! The agent loop: user turn, model call, permission-gated tool calls, repeat.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use tokio::io::AsyncWrite;
use tokio::sync::mpsc;

use crate::deepseek::{ChatMessage, DeepSeekClient};
use crate::ndjson;
use crate::protocol::{
    ControlResult, Outbound, PermissionDecision, RememberScope, ToolCall, UserMessage,
};
use crate::settings::SandboxGrants;
use crate::stdio_transport::{PendingRequests, register_pending};
use crate::tools::ToolRegistry;

const SYSTEM_PROMPT: &str = "You are a coding assistant with access to tools for reading files, \
writing files, and running shell commands. Use them when they help you answer the user's request. \
Always briefly explain what you're about to do before invoking a tool that changes files or runs \
a command. \
\n\n`bash` and `write_file` run inside a sandbox restricted to this project's directory, with no \
network access. If a call fails with something that looks like a sandbox restriction — \
'Operation not permitted', a DNS/network failure, or a command that should exist but isn't found — \
you may retry the exact same call with `escalate: true` and a one-sentence `reason`, which asks the \
user to approve running it outside the sandbox. Only do this after a sandboxed attempt has actually \
failed, never speculatively, and don't retry the same failure a second time after the user denies it.";

pub struct Agent<W> {
    client: DeepSeekClient,
    tools: ToolRegistry,
    writer: W,
    pending: PendingRequests,
    grants: Arc<Mutex<SandboxGrants>>,
    next_request_id: AtomicU64,
    conversation: Vec<ChatMessage>,
}

impl<W> Agent<W>
where
    W: AsyncWrite + Unpin,
{
    pub fn new(
        client: DeepSeekClient,
        tools: ToolRegistry,
        writer: W,
        pending: PendingRequests,
        grants: Arc<Mutex<SandboxGrants>>,
    ) -> Self {
        Self {
            client,
            tools,
            writer,
            pending,
            grants,
            next_request_id: AtomicU64::new(1),
            conversation: vec![ChatMessage::system(SYSTEM_PROMPT)],
        }
    }

    /// Runs until the host closes stdin (user_rx is exhausted).
    pub async fn run(&mut self, mut user_rx: mpsc::Receiver<UserMessage>) -> anyhow::Result<()> {
        while let Some(user_msg) = user_rx.recv().await {
            self.conversation.push(ChatMessage::user(user_msg.content));
            self.run_turn().await?;
        }
        Ok(())
    }

    /// Drives one user turn to completion, including any number of tool round trips.
    async fn run_turn(&mut self) -> anyhow::Result<()> {
        loop {
            let tool_defs = self.tools.to_tool_definitions();
            let turn = self
                .client
                .complete(&self.conversation, &tool_defs, async |text| {
                    ndjson::write_line(&mut self.writer, &Outbound::text_delta(text.to_string()))
                        .await
                })
                .await?;

            if turn.tool_calls.is_empty() {
                let text = turn.text.unwrap_or_default();
                self.conversation
                    .push(ChatMessage::assistant_text(text.clone()));
                ndjson::write_line(&mut self.writer, &Outbound::assistant_end_turn(text)).await?;
                return Ok(());
            }

            ndjson::write_line(
                &mut self.writer,
                &Outbound::assistant_tool_use(turn.text.clone(), &turn.tool_calls),
            )
            .await?;
            self.conversation.push(ChatMessage::assistant_tool_calls(
                turn.text,
                &turn.tool_calls,
            )?);

            for call in &turn.tool_calls {
                let (content, is_error) = self.run_tool_call(call).await?;
                ndjson::write_line(
                    &mut self.writer,
                    &Outbound::tool_result(call.id.clone(), content.clone(), is_error),
                )
                .await?;
                self.conversation
                    .push(ChatMessage::tool_result(call.id.clone(), content));
            }
        }
    }

    /// A denied or failed call becomes an `is_error` result for the model, not an `Err`.
    async fn run_tool_call(&mut self, call: &ToolCall) -> anyhow::Result<(String, bool)> {
        let request_id = format!(
            "req_{}",
            self.next_request_id.fetch_add(1, Ordering::Relaxed)
        );
        let rx = register_pending(&self.pending, request_id.clone());

        ndjson::write_line(
            &mut self.writer,
            &Outbound::control_request(
                request_id,
                call.name.clone(),
                call.input.clone(),
                call.id.clone(),
            ),
        )
        .await?;

        let decision = match rx.await {
            Ok(ControlResult::Success { decision }) => decision,
            Ok(ControlResult::Error { error }) => {
                return Ok((format!("permission request errored: {error}"), true));
            }
            Err(_) => PermissionDecision::Deny {
                message: Some("stdin closed while awaiting permission".to_string()),
            },
        };

        match decision {
            PermissionDecision::Allow { remember, .. } => {
                if remember == Some(RememberScope::Session) {
                    self.apply_session_grant(call);
                }
                match self.tools.dispatch(call).await {
                    Ok(output) => Ok((output, false)),
                    Err(err) => Ok((err.to_string(), true)),
                }
            }
            PermissionDecision::Deny { message } => {
                let reason = message.unwrap_or_else(|| "denied by host".to_string());
                Ok((format!("tool call denied: {reason}"), true))
            }
        }
    }

    /// In-memory only. `write_file` grants the target's directory; `bash` has no
    /// narrower scope, so it leaves the sandbox for the session.
    fn apply_session_grant(&self, call: &ToolCall) {
        let mut grants = self.grants.lock().unwrap();
        match call.name.as_str() {
            "bash" => grants.unsandboxed_bash = true,
            "write_file" => {
                if let Some(path) = call.input.get("path").and_then(|v| v.as_str())
                    && let Some(dir) = write_grant_dir(path)
                {
                    grants.extra_writable.insert(dir);
                }
            }
            _ => {}
        }
    }
}

/// The target's parent directory, absolute. A bare file name (parent `""`) is
/// in the workspace, which is writable already.
fn write_grant_dir(path: &str) -> Option<PathBuf> {
    let parent = Path::new(path).parent()?;
    if parent.as_os_str().is_empty() {
        return None;
    }
    if parent.is_absolute() {
        return Some(parent.to_path_buf());
    }
    std::env::current_dir().ok().map(|cwd| cwd.join(parent))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_grant_dir_is_the_parent_directory() {
        assert_eq!(
            write_grant_dir("/outside/dir/file.txt"),
            Some(PathBuf::from("/outside/dir"))
        );
    }

    #[test]
    fn write_grant_dir_resolves_relative_parents_against_the_workspace() {
        let cwd = std::env::current_dir().unwrap();
        assert_eq!(write_grant_dir("sub/dir/x.txt"), Some(cwd.join("sub/dir")));
        assert_eq!(
            write_grant_dir("../escape.txt"),
            Some(cwd.join(".."))
        );
    }

    #[test]
    fn write_grant_dir_grants_nothing_for_a_bare_file_name() {
        assert_eq!(write_grant_dir("bare.txt"), None);
        assert_eq!(write_grant_dir(""), None);
    }
}
