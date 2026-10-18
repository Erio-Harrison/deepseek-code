import { Session, SessionEvent, SessionHost } from "./session";
import { FromWebview, Mode, SessionInfo, ToWebview } from "./transcript";

export interface AppHost {
  /** Persists the chosen model; takes effect for sessions launched after this call. */
  setModel(model: string): void;
}

export class SessionManager {
  private readonly sessions = new Map<string, Session>();
  private activeId = "";
  private counter = 0;
  private post: (message: ToWebview) => void = () => {};
  private currentMode: Mode = "manual";

  constructor(
    private readonly host: Omit<SessionHost, "onEvent" | "mode">,
    private readonly appHost: AppHost,
    private readonly onSessionClosed: (sessionId: string) => void,
    private model: string,
    private readonly models: string[],
  ) {
    this.create();
  }

  get active(): Session {
    return this.sessions.get(this.activeId)!;
  }

  /** Attach (or detach, with a no-op) the webview. */
  setPoster(post: (message: ToWebview) => void): void {
    this.post = post;
  }

  create(): Session {
    const id = `s${++this.counter}`;
    const session = new Session(id, {
      ...this.host,
      onEvent: (s, e) => this.onEvent(s, e),
      mode: () => this.currentMode,
    });
    this.sessions.set(id, session);
    this.activeId = id;
    this.postState();
    return session;
  }

  switchTo(id: string): void {
    if (this.sessions.has(id) && id !== this.activeId) {
      this.activeId = id;
      this.postState();
    }
  }

  close(id: string): void {
    const session = this.sessions.get(id);
    if (!session) {
      return;
    }
    session.dispose();
    this.sessions.delete(id);
    this.onSessionClosed(id);
    if (this.sessions.size === 0) {
      this.create();
    } else {
      if (id === this.activeId) {
        this.activeId = [...this.sessions.keys()].pop()!;
      }
      this.postState();
    }
  }

  dispose(): void {
    for (const session of this.sessions.values()) {
      session.dispose();
    }
    this.sessions.clear();
  }

  postState(): void {
    this.post({
      type: "state",
      sessions: this.infos(),
      activeId: this.activeId,
      items: this.active.items,
    });
    this.postConfig();
  }

  postConfig(): void {
    this.post({ type: "config", mode: this.currentMode, model: this.model, models: this.models });
  }

  handle(message: FromWebview): void {
    const session = this.active;
    switch (message.type) {
      case "ready":
        this.postState();
        break;
      case "send":
        void session.send(message.text);
        break;
      case "newSession":
        this.create();
        break;
      case "switchSession":
        this.switchTo(message.id);
        break;
      case "closeSession":
        this.close(message.id);
        break;
      case "stop":
        session.stop();
        break;
      case "permission":
        session.resolvePermission(message.requestId, message.allow, message.remember);
        break;
      case "showDiff":
        void session.reopenDiff(message.requestId);
        break;
      case "setMode":
        this.currentMode = message.mode;
        this.postConfig();
        break;
      case "setModel":
        this.model = message.model;
        this.appHost.setModel(message.model);
        this.postConfig();
        break;
    }
  }

  private onEvent(session: Session, event: SessionEvent): void {
    if (event.kind === "meta") {
      this.post({
        type: "sessions",
        sessions: this.infos(),
        activeId: this.activeId,
      });
    } else if (event.kind === "patch") {
      this.post({
        type: "patch",
        sessionId: session.id,
        index: event.index,
        item: event.item,
      });
    } else {
      this.post({
        type: "append",
        sessionId: session.id,
        index: event.index,
        text: event.text,
      });
    }
  }

  private infos(): SessionInfo[] {
    return [...this.sessions.values()].map((s) => ({
      id: s.id,
      title: s.title,
      status: s.status,
    }));
  }
}
