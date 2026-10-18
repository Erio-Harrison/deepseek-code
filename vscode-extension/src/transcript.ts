// Transcript model plus the message contract between the extension and the webview.

/** "acceptEdits" auto-approves sandboxed `write_file`; `bash` and escalations always prompt. */
export type Mode = "manual" | "acceptEdits";

export type Item =
  | { kind: "user"; text: string }
  | { kind: "assistant"; text: string; streaming: boolean }
  | { kind: "tool_use"; name: string; input: unknown }
  | { kind: "tool_result"; content: string; isError: boolean }
  | {
      kind: "permission";
      requestId: string;
      toolName: string;
      input: unknown;
      status: "pending" | "allowed" | "denied";
      hasDiff: boolean;
      /** True when `input.escalate === true`: this call wants to skip the sandbox. */
      escalated: boolean;
      /** True when a mode auto-approved this without the user clicking anything. */
      auto: boolean;
    }
  | { kind: "notice"; text: string; isError: boolean };

export type SessionStatus = "idle" | "running" | "awaiting" | "stopped";

export interface SessionInfo {
  id: string;
  title: string;
  status: SessionStatus;
}

export type ToWebview =
  | { type: "state"; sessions: SessionInfo[]; activeId: string; items: Item[] }
  | { type: "sessions"; sessions: SessionInfo[]; activeId: string }
  | { type: "patch"; sessionId: string; index: number; item: Item }
  | { type: "append"; sessionId: string; index: number; text: string }
  | { type: "config"; mode: Mode; model: string; models: string[] };

export type FromWebview =
  | { type: "ready" }
  | { type: "send"; text: string }
  | { type: "newSession" }
  | { type: "switchSession"; id: string }
  | { type: "closeSession"; id: string }
  | { type: "stop" }
  | { type: "permission"; requestId: string; allow: boolean; remember?: "session" }
  | { type: "showDiff"; requestId: string }
  | { type: "setMode"; mode: Mode }
  | { type: "setModel"; model: string };
