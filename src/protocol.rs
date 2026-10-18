//! NDJSON stdio protocol between a host and the agent, with a
//! `control_request`/`control_response` permission round trip before each tool call.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// A single tool call requested by the model: shared between the DeepSeek client
/// (which produces it) and the outbound protocol (which reports it to the host).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub input: Value,
}

// ---------- Inbound: host -> agent ----------

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Inbound {
    User(UserEnvelope),
    ControlResponse(ControlResponseEnvelope),
    ControlCancelRequest(CancelRequestEnvelope),
}

#[derive(Debug, Deserialize)]
pub struct UserEnvelope {
    pub message: UserMessage,
    #[serde(default)]
    pub session_id: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct UserMessage {
    pub role: String,
    pub content: String,
}

#[derive(Debug, Deserialize)]
pub struct CancelRequestEnvelope {
    pub request_id: String,
}

#[derive(Debug, Deserialize)]
pub struct ControlResponseEnvelope {
    pub request_id: String,
    #[serde(flatten)]
    pub result: ControlResult,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "subtype", rename_all = "snake_case")]
pub enum ControlResult {
    Success { decision: PermissionDecision },
    Error { error: String },
}

#[derive(Debug, Deserialize)]
#[serde(tag = "behavior", rename_all = "snake_case")]
pub enum PermissionDecision {
    Allow {
        #[serde(default)]
        updated_input: Option<Value>,
        /// `None` means this call only.
        #[serde(default)]
        remember: Option<RememberScope>,
    },
    Deny {
        #[serde(default)]
        message: Option<String>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RememberScope {
    Session,
}

// ---------- Outbound: agent -> host ----------

#[derive(Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Outbound {
    /// Streaming text; the following `Assistant` message repeats it in full.
    TextDelta(TextDeltaEnvelope),
    Assistant(AssistantEnvelope),
    ControlRequest(ControlRequestEnvelope),
    ToolResult(ToolResultEnvelope),
}

#[derive(Debug, Serialize)]
pub struct TextDeltaEnvelope {
    pub text: String,
}

#[derive(Debug, Serialize)]
pub struct AssistantEnvelope {
    pub message: AssistantMessage,
}

#[derive(Debug, Serialize)]
pub struct AssistantMessage {
    pub content: Vec<ContentBlock>,
    pub stop_reason: StopReason,
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StopReason {
    EndTurn,
    ToolUse,
}

#[derive(Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentBlock {
    Text {
        text: String,
    },
    ToolUse {
        id: String,
        name: String,
        input: Value,
    },
}

#[derive(Debug, Serialize)]
pub struct ControlRequestEnvelope {
    pub request_id: String,
    pub subtype: ControlRequestSubtype,
    pub tool_name: String,
    pub input: Value,
    pub tool_use_id: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ControlRequestSubtype {
    CanUseTool,
}

#[derive(Debug, Serialize)]
pub struct ToolResultEnvelope {
    pub tool_use_id: String,
    pub content: String,
    pub is_error: bool,
}

impl Outbound {
    pub fn text_delta(text: String) -> Self {
        Outbound::TextDelta(TextDeltaEnvelope { text })
    }

    pub fn assistant_end_turn(text: String) -> Self {
        Outbound::Assistant(AssistantEnvelope {
            message: AssistantMessage {
                content: vec![ContentBlock::Text { text }],
                stop_reason: StopReason::EndTurn,
            },
        })
    }

    pub fn assistant_tool_use(text: Option<String>, calls: &[ToolCall]) -> Self {
        let mut content = Vec::new();
        if let Some(text) = text
            && !text.is_empty()
        {
            content.push(ContentBlock::Text { text });
        }
        for call in calls {
            content.push(ContentBlock::ToolUse {
                id: call.id.clone(),
                name: call.name.clone(),
                input: call.input.clone(),
            });
        }
        Outbound::Assistant(AssistantEnvelope {
            message: AssistantMessage {
                content,
                stop_reason: StopReason::ToolUse,
            },
        })
    }

    pub fn control_request(
        request_id: String,
        tool_name: String,
        input: Value,
        tool_use_id: String,
    ) -> Self {
        Outbound::ControlRequest(ControlRequestEnvelope {
            request_id,
            subtype: ControlRequestSubtype::CanUseTool,
            tool_name,
            input,
            tool_use_id,
        })
    }

    pub fn tool_result(tool_use_id: String, content: String, is_error: bool) -> Self {
        Outbound::ToolResult(ToolResultEnvelope {
            tool_use_id,
            content,
            is_error,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_user_message() {
        let line =
            r#"{"type":"user","message":{"role":"user","content":"hello"},"session_id":"s1"}"#;
        let msg: Inbound = serde_json::from_str(line).unwrap();
        match msg {
            Inbound::User(env) => {
                assert_eq!(env.message.content, "hello");
                assert_eq!(env.session_id.as_deref(), Some("s1"));
            }
            _ => panic!("expected User variant"),
        }
    }

    #[test]
    fn parses_control_response_allow() {
        let line = r#"{"type":"control_response","request_id":"req_1","subtype":"success","decision":{"behavior":"allow"}}"#;
        let msg: Inbound = serde_json::from_str(line).unwrap();
        match msg {
            Inbound::ControlResponse(env) => {
                assert_eq!(env.request_id, "req_1");
                match env.result {
                    ControlResult::Success {
                        decision:
                            PermissionDecision::Allow {
                                updated_input,
                                remember,
                            },
                    } => {
                        assert!(updated_input.is_none());
                        assert!(remember.is_none());
                    }
                    other => panic!("unexpected result: {other:?}"),
                }
            }
            _ => panic!("expected ControlResponse variant"),
        }
    }

    #[test]
    fn parses_control_response_allow_with_remember_session() {
        let line = r#"{"type":"control_response","request_id":"req_1","subtype":"success","decision":{"behavior":"allow","remember":"session"}}"#;
        let msg: Inbound = serde_json::from_str(line).unwrap();
        match msg {
            Inbound::ControlResponse(env) => match env.result {
                ControlResult::Success {
                    decision: PermissionDecision::Allow { remember, .. },
                } => assert_eq!(remember, Some(RememberScope::Session)),
                other => panic!("unexpected result: {other:?}"),
            },
            _ => panic!("expected ControlResponse variant"),
        }
    }

    #[test]
    fn parses_control_response_deny() {
        let line = r#"{"type":"control_response","request_id":"req_2","subtype":"success","decision":{"behavior":"deny","message":"no"}}"#;
        let msg: Inbound = serde_json::from_str(line).unwrap();
        match msg {
            Inbound::ControlResponse(env) => match env.result {
                ControlResult::Success {
                    decision: PermissionDecision::Deny { message },
                } => assert_eq!(message.as_deref(), Some("no")),
                other => panic!("unexpected result: {other:?}"),
            },
            _ => panic!("expected ControlResponse variant"),
        }
    }

    #[test]
    fn parses_control_cancel_request() {
        let line = r#"{"type":"control_cancel_request","request_id":"req_3"}"#;
        let msg: Inbound = serde_json::from_str(line).unwrap();
        match msg {
            Inbound::ControlCancelRequest(env) => assert_eq!(env.request_id, "req_3"),
            _ => panic!("expected ControlCancelRequest variant"),
        }
    }

    #[test]
    fn serializes_assistant_end_turn() {
        let out = Outbound::assistant_end_turn("hi there".to_string());
        let json = serde_json::to_string(&out).unwrap();
        assert!(json.contains(r#""type":"assistant""#));
        assert!(json.contains(r#""stop_reason":"end_turn""#));
        assert!(json.contains("hi there"));
    }

    #[test]
    fn serializes_text_delta() {
        let out = Outbound::text_delta("he".to_string());
        let json = serde_json::to_string(&out).unwrap();
        assert_eq!(json, r#"{"type":"text_delta","text":"he"}"#);
    }

    #[test]
    fn serializes_control_request() {
        let out = Outbound::control_request(
            "req_1".to_string(),
            "bash".to_string(),
            serde_json::json!({"command": "ls"}),
            "tool_1".to_string(),
        );
        let json = serde_json::to_string(&out).unwrap();
        assert!(json.contains(r#""type":"control_request""#));
        assert!(json.contains(r#""subtype":"can_use_tool""#));
        assert!(json.contains(r#""tool_name":"bash""#));
    }
}
