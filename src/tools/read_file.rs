use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};

use super::{Tool, ToolError};

pub struct ReadFileTool;

#[derive(Deserialize)]
struct ReadFileInput {
    path: String,
}

#[async_trait]
impl Tool for ReadFileTool {
    fn name(&self) -> &str {
        "read_file"
    }

    fn description(&self) -> &str {
        "Read the full contents of a UTF-8 text file at the given path."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "Path to the file to read" }
            },
            "required": ["path"]
        })
    }

    // Not sandboxed: the host's permission prompt is the only gate.
    async fn call(&self, input: Value) -> Result<String, ToolError> {
        let input: ReadFileInput =
            serde_json::from_value(input).map_err(|e| ToolError::InvalidInput(e.to_string()))?;
        let content = tokio::fs::read_to_string(&input.path).await?;
        Ok(content)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn reads_existing_file() {
        let dir = std::env::temp_dir();
        let path = dir.join(format!(
            "deepseek_code_test_read_{}.txt",
            std::process::id()
        ));
        tokio::fs::write(&path, "hello world").await.unwrap();

        let tool = ReadFileTool;
        let result = tool
            .call(json!({ "path": path.to_str().unwrap() }))
            .await
            .unwrap();
        assert_eq!(result, "hello world");

        tokio::fs::remove_file(&path).await.unwrap();
    }

    #[tokio::test]
    async fn missing_file_is_io_error() {
        let tool = ReadFileTool;
        let err = tool
            .call(json!({ "path": "/nonexistent/path/xyz" }))
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::Io(_)));
    }
}
