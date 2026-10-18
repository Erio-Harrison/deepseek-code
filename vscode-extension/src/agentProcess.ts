import { ChildProcessWithoutNullStreams, spawn } from "child_process";
import {
  AgentMessage,
  AssistantMessage,
  ControlRequest,
  PermissionDecision,
  ToolResult,
} from "./protocol";

export interface AgentHandlers {
  onTextDelta(text: string): void;
  onAssistant(msg: AssistantMessage): void;
  onToolResult(msg: ToolResult): void;
  onControlRequest(req: ControlRequest): Promise<PermissionDecision>;
  onStderr(text: string): void;
  /** `error` is set when the process couldn't be spawned at all. */
  onExit(code: number | null, signal: NodeJS.Signals | null, error?: Error): void;
}

export interface AgentOptions {
  binaryPath: string;
  cwd: string;
  env: NodeJS.ProcessEnv;
}

interface PendingTurn {
  resolve: () => void;
  reject: (err: Error) => void;
}

export class AgentProcess {
  private readonly child: ChildProcessWithoutNullStreams;
  private buffer = "";
  private turn: PendingTurn | undefined;
  private exited = false;

  constructor(
    opts: AgentOptions,
    private readonly handlers: AgentHandlers,
  ) {
    this.child = spawn(opts.binaryPath, [], {
      cwd: opts.cwd,
      env: opts.env,
      stdio: ["pipe", "pipe", "pipe"],
    });

    // setEncoding installs a StringDecoder, so multi-byte characters split
    // across chunks are reassembled before we look for line breaks.
    this.child.stdout.setEncoding("utf8");
    this.child.stdout.on("data", (chunk: string) => this.onStdout(chunk));
    this.child.stderr.setEncoding("utf8");
    this.child.stderr.on("data", (chunk: string) => handlers.onStderr(chunk));
    this.child.on("error", (err) => this.onExit(err));
    this.child.on("exit", (code, signal) => this.onExit(undefined, code, signal));
  }

  /** Sends a user message and resolves when the agent ends the turn. */
  ask(content: string): Promise<void> {
    if (this.exited) {
      return Promise.reject(new Error("agent process is not running"));
    }
    if (this.turn) {
      return Promise.reject(new Error("a turn is already in progress"));
    }
    return new Promise<void>((resolve, reject) => {
      this.turn = { resolve, reject };
      this.send({ type: "user", message: { role: "user", content } });
    });
  }

  dispose(): void {
    if (!this.exited) {
      this.child.kill();
    }
  }

  private send(message: unknown): void {
    // Only "\n" separates messages on the Rust side, so U+2028/U+2029 in the
    // payload need no special handling in this direction.
    this.child.stdin.write(JSON.stringify(message) + "\n");
  }

  // Split on "\n" only. Do not use readline: it also treats U+2028/U+2029 as
  // line terminators, which would cut a message apart mid-string.
  private onStdout(chunk: string): void {
    this.buffer += chunk;
    let newline: number;
    while ((newline = this.buffer.indexOf("\n")) !== -1) {
      let line = this.buffer.slice(0, newline);
      this.buffer = this.buffer.slice(newline + 1);
      if (line.endsWith("\r")) {
        line = line.slice(0, -1);
      }
      if (line.trim() !== "") {
        this.onLine(line);
      }
    }
  }

  private onLine(line: string): void {
    let msg: AgentMessage;
    try {
      msg = JSON.parse(line) as AgentMessage;
    } catch {
      this.handlers.onStderr(`[host] dropped unparseable line: ${line}\n`);
      return;
    }
    switch (msg.type) {
      case "text_delta":
        this.handlers.onTextDelta(msg.text);
        break;
      case "assistant":
        this.handlers.onAssistant(msg);
        if (msg.message.stop_reason === "end_turn") {
          this.finishTurn();
        }
        break;
      case "tool_result":
        this.handlers.onToolResult(msg);
        break;
      case "control_request":
        void this.answer(msg);
        break;
    }
  }

  private async answer(req: ControlRequest): Promise<void> {
    try {
      const decision = await this.handlers.onControlRequest(req);
      this.send({
        type: "control_response",
        request_id: req.request_id,
        subtype: "success",
        decision,
      });
    } catch (err) {
      this.send({
        type: "control_response",
        request_id: req.request_id,
        subtype: "error",
        error: String(err),
      });
    }
  }

  private finishTurn(): void {
    const turn = this.turn;
    this.turn = undefined;
    turn?.resolve();
  }

  private onExit(
    err?: Error,
    code: number | null = null,
    signal: NodeJS.Signals | null = null,
  ): void {
    if (this.exited) {
      return;
    }
    this.exited = true;
    const turn = this.turn;
    this.turn = undefined;
    turn?.reject(
      err ?? new Error(`agent exited (code ${code}, signal ${signal})`),
    );
    this.handlers.onExit(code, signal, err);
  }
}
