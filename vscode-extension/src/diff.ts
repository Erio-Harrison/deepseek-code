import * as fs from "fs";
import * as path from "path";
import * as vscode from "vscode";
import { isWriteInput } from "./session";

export const PROPOSED_SCHEME = "deepseek-proposed";

/** Serves the proposed (and, for new files, empty) side of a diff as read-only virtual documents. */
export class ProposedContentProvider implements vscode.TextDocumentContentProvider {
  private readonly docs = new Map<string, string>();

  provideTextDocumentContent(uri: vscode.Uri): string {
    return this.docs.get(uri.path) ?? "";
  }

  set(uriPath: string, content: string): void {
    this.docs.set(uriPath, content);
  }

  deleteSession(sessionId: string): void {
    const prefix = `/${sessionId}/`;
    for (const key of this.docs.keys()) {
      if (key.startsWith(prefix)) {
        this.docs.delete(key);
      }
    }
  }
}

/** Opens current-file vs proposed-content for a `write_file` call. Returns false if `input` isn't one. */
export async function showWriteDiff(
  provider: ProposedContentProvider,
  cwd: string,
  sessionId: string,
  requestId: string,
  input: unknown,
  preserveFocus: boolean,
): Promise<boolean> {
  if (!isWriteInput(input)) {
    return false;
  }
  const absolute = path.resolve(cwd, input.path);
  const name = path.basename(absolute);
  const base = `/${sessionId}/${requestId}`;

  const proposed = vscode.Uri.from({
    scheme: PROPOSED_SCHEME,
    path: `${base}/proposed/${name}`,
  });
  provider.set(proposed.path, input.content);

  let original = vscode.Uri.file(absolute);
  if (!fs.existsSync(absolute)) {
    original = vscode.Uri.from({
      scheme: PROPOSED_SCHEME,
      path: `${base}/empty/${name}`,
    });
    provider.set(original.path, "");
  }

  await vscode.commands.executeCommand(
    "vscode.diff",
    original,
    proposed,
    `${name} (proposed changes)`,
    { preview: true, preserveFocus },
  );
  return true;
}
