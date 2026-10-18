pub mod bash;
pub mod read_file;
pub mod write_file;

use async_trait::async_trait;
use serde_json::Value;
use std::collections::HashMap;
use thiserror::Error;

use crate::deepseek::{FunctionDefinition, ToolDefinition};
use crate::protocol::ToolCall;
use crate::settings::SandboxGrants;

#[derive(Debug, Error)]
pub enum ToolError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("invalid input: {0}")]
    InvalidInput(String),
    #[error("command timed out after {0:?}")]
    Timeout(std::time::Duration),
    #[error("sandbox error: {0}")]
    Sandbox(String),
}

/// The `agent_executor` preset plus session grants, with the host's `HOME` and
/// `PATH` (and no other host variables). Missing grant directories are skipped,
/// since nanosandbox refuses to build around them.
#[cfg(target_os = "macos")]
pub(crate) fn agent_sandbox_builder(
    workspace: &std::path::Path,
    grants: &SandboxGrants,
) -> nanosandbox::SandboxBuilder {
    let mut builder = nanosandbox::Sandbox::agent_executor(workspace);
    for path in &grants.extra_writable {
        if path.is_dir() {
            builder = builder.writable(path.clone());
        } else {
            tracing::warn!(path = %path.display(), "skipping sandbox write grant: not a directory");
        }
    }
    if let Ok(home) = std::env::var("HOME") {
        builder = builder.env("HOME", home);
    }
    if let Ok(path) = std::env::var("PATH") {
        builder = builder.env("PATH", path);
    }
    builder
}

#[async_trait]
pub trait Tool: Send + Sync {
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    fn parameters_schema(&self) -> Value;
    async fn call(&self, input: Value) -> Result<String, ToolError>;
}

#[derive(Default)]
pub struct ToolRegistry {
    tools: HashMap<String, Box<dyn Tool>>,
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&mut self, tool: Box<dyn Tool>) {
        self.tools.insert(tool.name().to_string(), tool);
    }

    pub fn to_tool_definitions(&self) -> Vec<ToolDefinition> {
        self.tools
            .values()
            .map(|t| ToolDefinition {
                r#type: "function".to_string(),
                function: FunctionDefinition {
                    name: t.name().to_string(),
                    description: t.description().to_string(),
                    parameters: t.parameters_schema(),
                },
            })
            .collect()
    }

    pub async fn dispatch(&self, call: &ToolCall) -> Result<String, ToolError> {
        match self.tools.get(&call.name) {
            Some(tool) => tool.call(call.input.clone()).await,
            None => Err(ToolError::InvalidInput(format!(
                "unknown tool: {}",
                call.name
            ))),
        }
    }
}

/// A directory the sandbox can't write wherever the checkout lives (the
/// checkout's parent may be under /tmp, which is writable). Removed on drop.
#[cfg(all(test, target_os = "macos"))]
pub(crate) struct OutsideDir(pub std::path::PathBuf);

#[cfg(all(test, target_os = "macos"))]
impl OutsideDir {
    pub fn new(name: &str) -> Self {
        let home = std::env::var("HOME").unwrap();
        let dir = std::path::PathBuf::from(home)
            .join(format!(".deepseek-code-test-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }
}

#[cfg(all(test, target_os = "macos"))]
impl Drop for OutsideDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::bash::BashTool;
    use crate::tools::read_file::ReadFileTool;
    use crate::tools::write_file::WriteFileTool;

    #[test]
    fn schemas_are_valid_json_objects() {
        let grants: std::sync::Arc<std::sync::Mutex<SandboxGrants>> =
            std::sync::Arc::new(std::sync::Mutex::new(SandboxGrants::default()));
        for tool in [
            Box::new(ReadFileTool) as Box<dyn Tool>,
            Box::new(WriteFileTool::new(grants.clone())) as Box<dyn Tool>,
            Box::new(BashTool::new(grants)) as Box<dyn Tool>,
        ] {
            let schema = tool.parameters_schema();
            assert_eq!(schema["type"], "object");
            assert!(schema["properties"].is_object());
        }
    }

    #[tokio::test]
    async fn dispatch_returns_error_for_unknown_tool() {
        let registry = ToolRegistry::new();
        let call = ToolCall {
            id: "1".to_string(),
            name: "does_not_exist".to_string(),
            input: serde_json::json!({}),
        };
        let err = registry.dispatch(&call).await.unwrap_err();
        assert!(matches!(err, ToolError::InvalidInput(_)));
    }
}
