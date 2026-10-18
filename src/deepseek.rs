//! DeepSeek chat completions client (OpenAI-compatible SSE + function calling).
//! Tool-call arguments arrive in fragments and are assembled before returning.

use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;

use crate::protocol::ToolCall;

const API_URL: &str = "https://api.deepseek.com/chat/completions";

#[derive(Clone, Debug, Serialize)]
#[serde(tag = "role", rename_all = "snake_case")]
pub enum ChatMessage {
    System {
        content: String,
    },
    User {
        content: String,
    },
    Assistant {
        #[serde(skip_serializing_if = "Option::is_none")]
        content: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        tool_calls: Option<Vec<OutgoingToolCall>>,
    },
    Tool {
        tool_call_id: String,
        content: String,
    },
}

impl ChatMessage {
    pub fn system(content: impl Into<String>) -> Self {
        ChatMessage::System {
            content: content.into(),
        }
    }

    pub fn user(content: impl Into<String>) -> Self {
        ChatMessage::User {
            content: content.into(),
        }
    }

    pub fn assistant_text(content: impl Into<String>) -> Self {
        ChatMessage::Assistant {
            content: Some(content.into()),
            tool_calls: None,
        }
    }

    pub fn assistant_tool_calls(text: Option<String>, calls: &[ToolCall]) -> anyhow::Result<Self> {
        let tool_calls = calls
            .iter()
            .map(|c| {
                Ok(OutgoingToolCall {
                    id: c.id.clone(),
                    r#type: "function".to_string(),
                    function: OutgoingFunctionCall {
                        name: c.name.clone(),
                        arguments: serde_json::to_string(&c.input)?,
                    },
                })
            })
            .collect::<anyhow::Result<Vec<_>>>()?;
        Ok(ChatMessage::Assistant {
            content: text,
            tool_calls: Some(tool_calls),
        })
    }

    pub fn tool_result(tool_call_id: impl Into<String>, content: impl Into<String>) -> Self {
        ChatMessage::Tool {
            tool_call_id: tool_call_id.into(),
            content: content.into(),
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct OutgoingToolCall {
    pub id: String,
    pub r#type: String,
    pub function: OutgoingFunctionCall,
}

#[derive(Clone, Debug, Serialize)]
pub struct OutgoingFunctionCall {
    pub name: String,
    /// JSON-encoded string, per the OpenAI-compatible wire format (not a raw object).
    pub arguments: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct ToolDefinition {
    pub r#type: String,
    pub function: FunctionDefinition,
}

#[derive(Clone, Debug, Serialize)]
pub struct FunctionDefinition {
    pub name: String,
    pub description: String,
    pub parameters: Value,
}

#[derive(Serialize)]
struct ChatCompletionRequest<'a> {
    model: &'a str,
    messages: &'a [ChatMessage],
    #[serde(skip_serializing_if = "<[_]>::is_empty")]
    tools: &'a [ToolDefinition],
    tool_choice: &'a str,
    stream: bool,
}

#[derive(Deserialize)]
struct ChatCompletionChunk {
    #[serde(default)]
    choices: Vec<ChunkChoice>,
}

#[derive(Deserialize)]
struct ChunkChoice {
    #[serde(default)]
    delta: ChunkDelta,
    #[serde(default)]
    finish_reason: Option<String>,
}

#[derive(Deserialize, Default)]
struct ChunkDelta {
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    tool_calls: Option<Vec<ChunkToolCall>>,
}

#[derive(Deserialize)]
struct ChunkToolCall {
    index: usize,
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    function: Option<ChunkFunctionCall>,
}

#[derive(Deserialize, Default)]
struct ChunkFunctionCall {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    arguments: Option<String>,
}

#[derive(Default)]
struct PartialToolCall {
    id: String,
    name: String,
    arguments: String,
}

pub struct AssistantTurn {
    pub text: Option<String>,
    pub tool_calls: Vec<ToolCall>,
}

pub struct DeepSeekClient {
    http: reqwest::Client,
    api_key: String,
    model: String,
}

impl DeepSeekClient {
    pub fn new(api_key: String, model: String) -> Self {
        Self {
            http: reqwest::Client::new(),
            api_key,
            model,
        }
    }

    /// `on_text` is awaited with each text fragment as it arrives, before the
    /// stream finishes; the full text is still returned in `AssistantTurn`.
    pub async fn complete(
        &self,
        messages: &[ChatMessage],
        tools: &[ToolDefinition],
        mut on_text: impl AsyncFnMut(&str) -> anyhow::Result<()>,
    ) -> anyhow::Result<AssistantTurn> {
        let body = ChatCompletionRequest {
            model: &self.model,
            messages,
            tools,
            tool_choice: "auto",
            stream: true,
        };

        let response = self
            .http
            .post(API_URL)
            .bearer_auth(&self.api_key)
            .json(&body)
            .send()
            .await?
            .error_for_status()?;

        let mut byte_stream = response.bytes_stream();
        let mut buf = String::new();
        let mut text = String::new();
        let mut partials: HashMap<usize, PartialToolCall> = HashMap::new();

        while let Some(chunk) = byte_stream.next().await {
            let chunk = chunk?;
            buf.push_str(&String::from_utf8_lossy(&chunk));

            while let Some(pos) = buf.find("\n\n") {
                let frame: String = buf.drain(..pos + 2).collect();
                for line in frame.lines() {
                    let Some(data) = line.strip_prefix("data: ") else {
                        continue;
                    };
                    if data == "[DONE]" {
                        continue;
                    }
                    let parsed: ChatCompletionChunk = serde_json::from_str(data)?;
                    for choice in parsed.choices {
                        if let Some(content) = choice.delta.content
                            && !content.is_empty()
                        {
                            on_text(&content).await?;
                            text.push_str(&content);
                        }
                        if let Some(tool_calls) = choice.delta.tool_calls {
                            for tc in tool_calls {
                                let entry = partials.entry(tc.index).or_default();
                                if let Some(id) = tc.id {
                                    entry.id = id;
                                }
                                if let Some(func) = tc.function {
                                    if let Some(name) = func.name {
                                        entry.name = name;
                                    }
                                    if let Some(args) = func.arguments {
                                        entry.arguments.push_str(&args);
                                    }
                                }
                            }
                        }
                        let _ = choice.finish_reason; // observed but not needed: presence/absence of tool_calls already tells us what happened
                    }
                }
            }
        }

        let mut indices: Vec<usize> = partials.keys().copied().collect();
        indices.sort_unstable();
        let mut tool_calls = Vec::with_capacity(indices.len());
        for idx in indices {
            let p = partials.remove(&idx).unwrap();
            let input: Value = if p.arguments.trim().is_empty() {
                Value::Object(Default::default())
            } else {
                serde_json::from_str(&p.arguments)?
            };
            tool_calls.push(ToolCall {
                id: p.id,
                name: p.name,
                input,
            });
        }

        Ok(AssistantTurn {
            text: if text.is_empty() { None } else { Some(text) },
            tool_calls,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn client() -> DeepSeekClient {
        let key = std::env::var("DEEPSEEK_API_KEY")
            .expect("DEEPSEEK_API_KEY must be set for ignored network tests");
        DeepSeekClient::new(key, "deepseek-chat".to_string())
    }

    #[tokio::test]
    #[ignore = "hits the live DeepSeek API; run with `cargo test -- --ignored`"]
    async fn plain_completion_returns_text_and_no_tool_calls() {
        let client = client();
        let messages = vec![ChatMessage::user("Say the single word: pong")];
        let mut deltas = String::new();
        let turn = client
            .complete(&messages, &[], async |t| {
                deltas.push_str(t);
                Ok(())
            })
            .await
            .unwrap();
        assert!(turn.text.is_some());
        assert!(turn.tool_calls.is_empty());
        assert_eq!(Some(deltas), turn.text);
    }

    #[tokio::test]
    #[ignore = "hits the live DeepSeek API; run with `cargo test -- --ignored`"]
    async fn tool_forcing_completion_reconstructs_tool_call_from_sse_deltas() {
        let client = client();
        let tools = vec![ToolDefinition {
            r#type: "function".to_string(),
            function: FunctionDefinition {
                name: "get_weather".to_string(),
                description: "Get the weather for a city".to_string(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": { "city": { "type": "string" } },
                    "required": ["city"]
                }),
            },
        }];
        let messages = vec![ChatMessage::user(
            "Call get_weather for the city of Hangzhou. You must use the tool, don't answer directly.",
        )];
        let turn = client
            .complete(&messages, &tools, async |_| Ok(()))
            .await
            .unwrap();
        assert_eq!(turn.tool_calls.len(), 1);
        assert_eq!(turn.tool_calls[0].name, "get_weather");
        assert_eq!(turn.tool_calls[0].input["city"], "Hangzhou");
    }
}
