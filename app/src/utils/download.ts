// Downloading a file — with a shortcut when it's already on disk.
//
// If the file is already synced to a local folder, re-fetching it from the
// server is wasteful and confusing (the user now has two copies). Instead we
// offer to reveal the existing copy in the file manager; "Download a copy"
// falls back to the normal save-dialog download.

import { ask, save } from "@tauri-apps/plugin-dialog";
import { revealItemInDir } from "@tauri-apps/plugin-opener";
import { files, media } from "../api";
import type { FileEntry } from "../api/types";

/** The default "download a copy": a save dialog then `files.download`. */
async function saveDialogDownload(entry: FileEntry): Promise<void> {
  const dest = await save({ defaultPath: entry.name });
  if (dest) await files.download(entry.path, dest);
}

/**
 * Download `entry`, but if a synced local copy exists, first ask whether to show
 * that copy in the file manager instead of downloading again. When the file
 * isn't synced (or the user picks "Download a copy") it runs `saveCopy` —
 * defaulting to the standard save-dialog download.
 */
export async function downloadOrReveal(
  entry: FileEntry,
  saveCopy: () => Promise<void> = () => saveDialogDownload(entry),
): Promise<void> {
  const local = await media.localPath(entry.path).catch(() => null);
  if (local) {
    const reveal = await ask(
      `“${entry.name}” is already synced to your computer:\n${local}`,
      {
        title: "Already on your computer",
        kind: "info",
        okLabel: "Show in file manager",
        cancelLabel: "Download a copy",
      },
    );
    if (reveal) {
      await revealItemInDir(local);
      return;
    }
  }
  await saveCopy();
}
