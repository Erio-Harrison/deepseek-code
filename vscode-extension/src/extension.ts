import * as vscode from "vscode";
import { ChatViewProvider } from "./chatView";
import { PROPOSED_SCHEME, ProposedContentProvider, showWriteDiff } from "./diff";
import { createLauncher, promptForApiKey, workspaceRoot } from "./launcher";
import { SessionManager } from "./sessionManager";

// Models offered in the dropdown; the first is the core's default.
const KNOWN_MODELS = ["deepseek-chat", "deepseek-flash", "deepseek-v4-pro"];

export function activate(context: vscode.ExtensionContext): void {
  const output = vscode.window.createOutputChannel("DeepSeek Code");
  const proposed = new ProposedContentProvider();
  const launch = createLauncher(context);

  const configuredModel = vscode.workspace
    .getConfiguration("deepseekCode")
    .get<string>("model");

  let chat: ChatViewProvider;
  const manager = new SessionManager(
    {
      launch,
      showDiff: async (sessionId, requestId, input, preserveFocus) => {
        const cwd = workspaceRoot();
        return cwd
          ? showWriteDiff(proposed, cwd, sessionId, requestId, input, preserveFocus)
          : false;
      },
      requestAttention: () => chat.requestAttention(),
      log: (text) => output.append(text),
    },
    {
      // Applies to sessions started after this; running ones keep their model.
      setModel: (model) =>
        void vscode.workspace
          .getConfiguration("deepseekCode")
          .update("model", model, vscode.ConfigurationTarget.Workspace),
    },
    (sessionId) => proposed.deleteSession(sessionId),
    configuredModel || KNOWN_MODELS[0],
    KNOWN_MODELS,
  );
  chat = new ChatViewProvider(context.extensionUri, manager);

  context.subscriptions.push(
    output,
    { dispose: () => manager.dispose() },
    vscode.workspace.registerTextDocumentContentProvider(PROPOSED_SCHEME, proposed),
    vscode.window.registerWebviewViewProvider(ChatViewProvider.viewType, chat, {
      webviewOptions: { retainContextWhenHidden: true },
    }),
    vscode.commands.registerCommand("deepseekCode.openChat", () => chat.focus()),
    vscode.commands.registerCommand("deepseekCode.newSession", async () => {
      manager.create();
      await chat.focus();
    }),
    vscode.commands.registerCommand("deepseekCode.stop", () => manager.active.stop()),
    vscode.commands.registerCommand("deepseekCode.setApiKey", () =>
      promptForApiKey(context),
    ),
  );
}

export function deactivate(): void {}
