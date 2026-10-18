import * as fs from "fs";
import * as path from "path";
import * as vscode from "vscode";
import { AgentHandlers, AgentProcess } from "./agentProcess";

const API_KEY_SECRET = "deepseekCode.apiKey";

export function workspaceRoot(): string | undefined {
  return vscode.workspace.workspaceFolders?.[0]?.uri.fsPath;
}

export async function promptForApiKey(
  context: vscode.ExtensionContext,
): Promise<string | undefined> {
  const key = await vscode.window.showInputBox({
    prompt: "DeepSeek API key",
    password: true,
    ignoreFocusOut: true,
  });
  if (key) {
    await context.secrets.store(API_KEY_SECRET, key);
  }
  return key;
}

export function createLauncher(context: vscode.ExtensionContext) {
  return async (handlers: AgentHandlers): Promise<AgentProcess | undefined> => {
    // The agent's sandbox is scoped to its cwd, so it must be the workspace root.
    const cwd = workspaceRoot();
    if (!cwd) {
      vscode.window.showErrorMessage("DeepSeek Code: open a folder first.");
      return undefined;
    }

    const binaryPath = resolveBinary(context);
    if (!fs.existsSync(binaryPath)) {
      vscode.window.showErrorMessage(
        `DeepSeek Code: binary not found at ${binaryPath}. Run \`cargo build\`, or set deepseekCode.binaryPath.`,
      );
      return undefined;
    }

    const apiKey =
      (await context.secrets.get(API_KEY_SECRET)) ??
      (await promptForApiKey(context));
    if (!apiKey) {
      return undefined;
    }

    const env: NodeJS.ProcessEnv = { ...process.env, DEEPSEEK_API_KEY: apiKey };
    const model = vscode.workspace
      .getConfiguration("deepseekCode")
      .get<string>("model");
    if (model) {
      env.DEEPSEEK_MODEL = model;
    }

    return new AgentProcess({ binaryPath, cwd, env }, handlers);
  };
}

function resolveBinary(context: vscode.ExtensionContext): string {
  const configured = vscode.workspace
    .getConfiguration("deepseekCode")
    .get<string>("binaryPath");
  if (configured) {
    return configured;
  }
  const exe =
    process.platform === "win32" ? "deepseek-code.exe" : "deepseek-code";
  return path.join(context.extensionPath, "..", "target", "debug", exe);
}
