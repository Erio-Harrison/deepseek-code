// Mirrors the wire types in ../../src/protocol.rs.

export type ContentBlock =
  | { type: "text"; text: string }
  | { type: "tool_use"; id: string; name: string; input: unknown };

export type StopReason = "end_turn" | "tool_use";

export interface TextDelta {
  type: "text_delta";
  text: string;
}

export interface AssistantMessage {
  type: "assistant";
  message: { content: ContentBlock[]; stop_reason: StopReason };
}

export interface ControlRequest {
  type: "control_request";
  request_id: string;
  subtype: "can_use_tool";
  tool_name: string;
  input: unknown;
  tool_use_id: string;
}

export interface ToolResult {
  type: "tool_result";
  tool_use_id: string;
  content: string;
  is_error: boolean;
}

export type AgentMessage =
  | TextDelta
  | AssistantMessage
  | ControlRequest
  | ToolResult;

export type PermissionDecision =
  | { behavior: "allow"; remember?: "session" }
  | { behavior: "deny"; message?: string };
