import { randomBytes } from "crypto";
import * as vscode from "vscode";
import { SessionManager } from "./sessionManager";
import { FromWebview } from "./transcript";

export class ChatViewProvider implements vscode.WebviewViewProvider {
  static readonly viewType = "deepseekCode.chat";

  private view: vscode.WebviewView | undefined;

  constructor(
    private readonly extensionUri: vscode.Uri,
    private readonly manager: SessionManager,
  ) {}

  resolveWebviewView(view: vscode.WebviewView): void {
    this.view = view;
    view.webview.options = {
      enableScripts: true,
      localResourceRoots: [vscode.Uri.joinPath(this.extensionUri, "media")],
    };
    view.webview.html = this.html(view.webview);
    view.webview.onDidReceiveMessage((message: FromWebview) =>
      this.manager.handle(message),
    );
    this.manager.setPoster((message) => void view.webview.postMessage(message));
    view.onDidDispose(() => {
      this.view = undefined;
      this.manager.setPoster(() => {});
    });
  }

  focus(): Thenable<unknown> {
    return vscode.commands.executeCommand(`${ChatViewProvider.viewType}.focus`);
  }

  /** Nudges the user when the agent is blocked on an approval they may not be looking at. */
  requestAttention(): void {
    if (this.view?.visible) {
      return;
    }
    void vscode.window
      .showInformationMessage("DeepSeek Code is waiting for your approval.", "Open")
      .then((choice) => (choice ? this.focus() : undefined));
  }

  private html(webview: vscode.Webview): string {
    const nonce = randomBytes(16).toString("hex");
    const asset = (name: string) =>
      webview.asWebviewUri(vscode.Uri.joinPath(this.extensionUri, "media", name));
    return `<!DOCTYPE html>
<html lang="en">
<head>
  <meta charset="UTF-8">
  <meta http-equiv="Content-Security-Policy" content="default-src 'none'; style-src ${webview.cspSource}; script-src 'nonce-${nonce}';">
  <meta name="viewport" content="width=device-width, initial-scale=1.0">
  <link rel="stylesheet" href="${asset("chat.css")}">
</head>
<body>
  <div id="tabs"></div>
  <div id="list"></div>
  <div id="composer">
    <textarea id="input" rows="3" placeholder="Ask DeepSeek Code (Enter to send, Shift+Enter for a new line)"></textarea>
    <div class="row">
      <div class="controls">
        <select id="mode" title="Permission mode"></select>
        <select id="model" title="Model"></select>
      </div>
      <div class="actions">
        <button id="stop" class="secondary" hidden>Stop</button>
        <button id="send">Send</button>
      </div>
    </div>
  </div>
  <script nonce="${nonce}" src="${asset("vendor/marked.js")}"></script>
  <script nonce="${nonce}" src="${asset("vendor/purify.min.js")}"></script>
  <script nonce="${nonce}" src="${asset("chat.js")}"></script>
</body>
</html>`;
  }
}
