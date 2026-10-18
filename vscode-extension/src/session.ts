import { AgentHandlers, AgentProcess } from "./agentProcess";
import { AssistantMessage, ControlRequest, PermissionDecision } from "./protocol";
import { Item, Mode, SessionStatus } from "./transcript";

export type SessionEvent =
  | { kind: "patch"; index: number; item: Item }
  | { kind: "append"; index: number; text: string }
  | { kind: "meta" };

export interface SessionHost {
  /** Starts an agent process, or returns undefined after telling the user why not. */
  launch(handlers: AgentHandlers): Promise<AgentProcess | undefined>;
  /** Opens a diff for a proposed file write; false if the input isn't a write. */
  showDiff(
    sessionId: string,
    requestId: string,
    input: unknown,
    preserveFocus: boolean,
  ): Promise<boolean>;
  onEvent(session: Session, event: SessionEvent): void;
  requestAttention(): void;
  log(text: string): void;
  /** Current auto-approval mode, read fresh on every permission request. */
  mode(): Mode;
}

const DEFAULT_TITLE = "New chat";

/** One conversation, backed by one agent process; stopping it drops the history. */
export class Session {
  title = DEFAULT_TITLE;
  status: SessionStatus = "idle";
  readonly items: Item[] = [];

  private agent: AgentProcess | undefined;
  private stopping = false;
  private readonly decisions = new Map<string, (d: PermissionDecision) => void>();
  private readonly permissionIndex = new Map<string, number>();
  private readonly permissionInputs = new Map<string, unknown>();

  constructor(
    readonly id: string,
    private readonly host: SessionHost,
  ) {}

  get busy(): boolean {
    return this.status === "running" || this.status === "awaiting";
  }

  async send(text: string): Promise<void> {
    if (this.busy || text.trim() === "") {
      return;
    }
    this.push({ kind: "user", text });
    if (this.title === DEFAULT_TITLE) {
      this.title = text.length > 24 ? `${text.slice(0, 24)}…` : text;
    }
    this.setStatus("running");

    try {
      if (!this.agent) {
        this.stopping = false;
        this.agent = await this.host.launch(this.handlers());
        if (!this.agent) {
          this.notice("The agent could not be started.", true);
          this.setStatus("idle");
          return;
        }
      }
      await this.agent.ask(text);
      this.setStatus("idle");
    } catch (err) {
      // handleExit already reported a dead process and set "stopped".
      if (this.status !== "stopped") {
        this.notice(err instanceof Error ? err.message : String(err), true);
        this.setStatus("idle");
      }
    }
  }

  /** Kills the agent. The core has no cancel yet, so this also drops the context. */
  stop(): void {
    if (this.agent) {
      this.stopping = true;
      this.agent.dispose();
    }
  }

  dispose(): void {
    this.stopping = true;
    this.denyAllPending("session closed");
    this.agent?.dispose();
    this.agent = undefined;
  }

  resolvePermission(requestId: string, allow: boolean, remember?: "session"): void {
    const resolve = this.decisions.get(requestId);
    const index = this.permissionIndex.get(requestId);
    if (!resolve || index === undefined) {
      return;
    }
    this.decisions.delete(requestId);
    const item = this.items[index];
    if (item.kind === "permission") {
      item.status = allow ? "allowed" : "denied";
      this.emit({ kind: "patch", index, item });
    }
    if (this.status === "awaiting") {
      this.setStatus("running");
    }
    resolve(
      allow
        ? { behavior: "allow", remember }
        : { behavior: "deny", message: "denied by user" },
    );
  }

  async reopenDiff(requestId: string): Promise<void> {
    await this.host.showDiff(
      this.id,
      requestId,
      this.permissionInputs.get(requestId),
      false,
    );
  }

  private handlers(): AgentHandlers {
    return {
      onTextDelta: (text) => this.appendAssistant(text),
      onAssistant: (msg) => this.finishAssistant(msg),
      onToolResult: (msg) =>
        this.push({
          kind: "tool_result",
          content: msg.content,
          isError: msg.is_error,
        }),
      onControlRequest: (req) => this.requestPermission(req),
      onStderr: (text) => this.host.log(text),
      onExit: (code, signal, error) => this.handleExit(code, signal, error),
    };
  }

  private appendAssistant(text: string): void {
    const index = this.items.length - 1;
    const last = this.items[index];
    if (last?.kind === "assistant" && last.streaming) {
      last.text += text;
      this.emit({ kind: "append", index, text });
    } else {
      this.push({ kind: "assistant", text, streaming: true });
    }
  }

  // The final message carries the full text, so it also covers a core that
  // doesn't stream: with no streaming bubble open it just adds one.
  private finishAssistant(msg: AssistantMessage): void {
    let text = "";
    for (const block of msg.message.content) {
      if (block.type === "text") {
        text += block.text;
      }
    }
    const index = this.items.length - 1;
    const last = this.items[index];
    if (last?.kind === "assistant" && last.streaming) {
      last.text = text;
      last.streaming = false;
      this.emit({ kind: "patch", index, item: last });
    } else if (text !== "") {
      this.push({ kind: "assistant", text, streaming: false });
    }
    for (const block of msg.message.content) {
      if (block.type === "tool_use") {
        this.push({ kind: "tool_use", name: block.name, input: block.input });
      }
    }
  }

  private requestPermission(req: ControlRequest): Promise<PermissionDecision> {
    const escalated = isEscalated(req.input);
    const hasDiff = req.tool_name === "write_file" && isWriteInput(req.input);
    // "acceptEdits" only ever auto-approves an in-sandbox write_file: bash and
    // any escalated (sandbox-skipping) call always prompt, in every mode.
    const auto =
      !escalated && req.tool_name === "write_file" && this.host.mode() === "acceptEdits";

    this.permissionIndex.set(req.request_id, this.items.length);
    this.permissionInputs.set(req.request_id, req.input);
    this.push({
      kind: "permission",
      requestId: req.request_id,
      toolName: req.tool_name,
      input: req.input,
      status: auto ? "allowed" : "pending",
      hasDiff,
      escalated,
      auto,
    });
    if (hasDiff) {
      void this.host.showDiff(this.id, req.request_id, req.input, true);
    }
    if (auto) {
      return Promise.resolve({ behavior: "allow" });
    }

    this.setStatus("awaiting");
    this.host.requestAttention();
    return new Promise((resolve) => this.decisions.set(req.request_id, resolve));
  }

  private handleExit(
    code: number | null,
    signal: NodeJS.Signals | null,
    error?: Error,
  ): void {
    this.agent = undefined;
    this.denyAllPending("agent exited");
    this.notice(
      this.stopping
        ? "Stopped. The next message starts a new conversation."
        : error
          ? `The agent process failed: ${error.message}`
          : `The agent exited unexpectedly (code ${code}, signal ${signal}).`,
      !this.stopping,
    );
    this.stopping = false;
    this.setStatus("stopped");
  }

  private denyAllPending(reason: string): void {
    for (const [requestId, resolve] of this.decisions) {
      const index = this.permissionIndex.get(requestId);
      const item = index === undefined ? undefined : this.items[index];
      if (index !== undefined && item?.kind === "permission") {
        item.status = "denied";
        this.emit({ kind: "patch", index, item });
      }
      resolve({ behavior: "deny", message: reason });
    }
    this.decisions.clear();
  }

  private notice(text: string, isError: boolean): void {
    this.push({ kind: "notice", text, isError });
  }

  private push(item: Item): void {
    this.items.push(item);
    this.emit({ kind: "patch", index: this.items.length - 1, item });
  }

  private setStatus(status: SessionStatus): void {
    this.status = status;
    this.emit({ kind: "meta" });
  }

  private emit(event: SessionEvent): void {
    this.host.onEvent(this, event);
  }
}

function isEscalated(input: unknown): boolean {
  return (
    typeof input === "object" &&
    input !== null &&
    (input as { escalate?: unknown }).escalate === true
  );
}

export function isWriteInput(
  value: unknown,
): value is { path: string; content: string } {
  return (
    typeof value === "object" &&
    value !== null &&
    typeof (value as { path?: unknown }).path === "string" &&
    typeof (value as { content?: unknown }).content === "string"
  );
}
