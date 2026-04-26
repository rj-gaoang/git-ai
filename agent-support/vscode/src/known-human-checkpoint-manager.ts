import * as vscode from "vscode";
import * as path from "node:path";
import { spawn } from "child_process";
import { getGitAiBinary } from "./utils/binary-path";
import { getGitRepoRoot } from "./utils/git-api";

/**
 * Fires a `git-ai checkpoint known_human --hook-input stdin` whenever a
 * document is saved. Debounces per repo root over a 500ms window so that
 * bulk saves (e.g. "Save All") are batched into one checkpoint call.
 *
 * Skips non-file-scheme documents and .vscode/ internal files.
 */
export class KnownHumanCheckpointManager {
  private readonly debounceMs = 500;
  private readonly aiEditSuppressionMaxAgeMs = 15000;

  // per repo root: pending debounce timer
  private pendingTimers = new Map<string, NodeJS.Timeout>();

  // per repo root: set of absolute file paths queued in current debounce window
  private pendingPaths = new Map<string, Set<string>>();

  // per file: last time we saw a Copilot chat-editing document for this path
  private recentAiEditAt = new Map<string, number>();

  constructor(
    private readonly editorVersion: string,
    private readonly extensionVersion: string,
  ) {}

  public handlePotentialAiEditEvent(doc: vscode.TextDocument): void {
    if (!this.isCopilotChatEditingDocument(doc)) {
      return;
    }

    const filePath = doc.uri.fsPath;
    if (!filePath || this.isInternalVSCodePath(filePath)) {
      return;
    }

    this.recentAiEditAt.set(filePath, Date.now());
    console.log("[git-ai] KnownHumanCheckpointManager: Marked next save as AI-associated for", filePath);
  }

  public handleSaveEvent(doc: vscode.TextDocument): void {
    if (doc.uri.scheme !== "file") {
      return;
    }

    const filePath = doc.uri.fsPath;

    if (this.isInternalVSCodePath(filePath)) {
      console.log("[git-ai] KnownHumanCheckpointManager: Ignoring internal VSCode file:", filePath);
      return;
    }

    if (this.consumeRecentAiSaveSuppression(filePath)) {
      console.log("[git-ai] KnownHumanCheckpointManager: Skipping AI-associated save for", filePath);
      return;
    }

    const repoRoot = getGitRepoRoot(doc.uri);
    if (!repoRoot) {
      console.log("[git-ai] KnownHumanCheckpointManager: No git repo found for", filePath, "- skipping");
      return;
    }

    // Accumulate file into pending set for this repo root
    let pending = this.pendingPaths.get(repoRoot);
    if (!pending) {
      pending = new Set();
      this.pendingPaths.set(repoRoot, pending);
    }
    pending.add(filePath);

    // Reset debounce timer
    const existing = this.pendingTimers.get(repoRoot);
    if (existing) {
      clearTimeout(existing);
    }

    const timer = setTimeout(() => {
      this.executeCheckpoint(repoRoot).catch((err) =>
        console.error("[git-ai] KnownHumanCheckpointManager: Checkpoint error:", err)
      );
    }, this.debounceMs);

    this.pendingTimers.set(repoRoot, timer);
    console.log("[git-ai] KnownHumanCheckpointManager: Save queued for", filePath);
  }

  private async executeCheckpoint(repoRoot: string): Promise<void> {
    this.pendingTimers.delete(repoRoot);

    const paths = this.pendingPaths.get(repoRoot);
    if (!paths || paths.size === 0) {
      return;
    }
    const snapshot = [...paths];
    paths.clear();

    // Build dirty_files as absolute path → current content
    const dirtyFiles: Record<string, string> = {};
    for (const absolutePath of snapshot) {
      const doc = vscode.workspace.textDocuments.find(
        (d) => d.uri.fsPath === absolutePath && d.uri.scheme === "file"
      );

      let content: string | null = null;
      if (doc) {
        // Use open document buffer if available (handles codespaces/remote lag)
        content = doc.getText();
      } else {
        // Fall back to reading from disk if document was closed within debounce window
        try {
          const bytes = await vscode.workspace.fs.readFile(vscode.Uri.file(absolutePath));
          content = Buffer.from(bytes).toString("utf-8");
        } catch (err) {
          console.error("[git-ai] KnownHumanCheckpointManager: Failed to read file", absolutePath, err);
        }
      }

      if (content !== null) {
        dirtyFiles[absolutePath] = content;
      }
    }

    if (Object.keys(dirtyFiles).length === 0) {
      return;
    }

    const editedFilepaths = Object.keys(dirtyFiles);

    const hookInput = JSON.stringify({
      editor: "vscode",
      editor_version: this.editorVersion,
      extension_version: this.extensionVersion,
      cwd: repoRoot,
      edited_filepaths: editedFilepaths,
      dirty_files: dirtyFiles,
    });

    console.log("[git-ai] KnownHumanCheckpointManager: Firing known_human checkpoint for", editedFilepaths);

    const proc = spawn(getGitAiBinary(), ["checkpoint", "known_human", "--hook-input", "stdin"], {
      cwd: repoRoot,
    });

    let stdout = "";
    let stderr = "";

    proc.stdout.on("data", (data) => { stdout += data.toString(); });
    proc.stderr.on("data", (data) => { stderr += data.toString(); });

    proc.on("error", (err) => {
      console.error("[git-ai] KnownHumanCheckpointManager: Spawn error:", err.message);
    });

    proc.on("close", (code) => {
      if (code !== 0) {
        console.error("[git-ai] KnownHumanCheckpointManager: Checkpoint exited with code", code, stdout, stderr);
      } else {
        console.log("[git-ai] KnownHumanCheckpointManager: Checkpoint succeeded", stdout.trim());
      }
    });

    proc.stdin.write(hookInput);
    proc.stdin.end();
  }

  private isInternalVSCodePath(filePath: string): boolean {
    const normalized = filePath.replace(/\\/g, "/");
    return normalized.includes("/.vscode/");
  }

  private isCopilotChatEditingDocument(doc: vscode.TextDocument): boolean {
    return doc.uri.scheme === "chat-editing-snapshot-text-model"
      || doc.uri.scheme === "chat-editing-text-model";
  }

  private consumeRecentAiSaveSuppression(filePath: string): boolean {
    const markedAt = this.recentAiEditAt.get(filePath);
    if (!markedAt) {
      return false;
    }

    this.recentAiEditAt.delete(filePath);
    return Date.now() - markedAt <= this.aiEditSuppressionMaxAgeMs;
  }

  public dispose(): void {
    for (const timer of this.pendingTimers.values()) {
      clearTimeout(timer);
    }
    this.pendingTimers.clear();
    this.pendingPaths.clear();
    this.recentAiEditAt.clear();
  }
}
