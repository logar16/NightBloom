import { api } from './client';

// Integration with the Tauri desktop shell. Browser downloads retain their existing behavior.

/** True inside the Tauri desktop WebView (Tauri injects this before any page script runs). */
export function isDesktop(): boolean {
  return typeof window !== 'undefined' && '__TAURI_INTERNALS__' in window;
}

/**
 * Window event the desktop shell dispatches when a download it handled has ended. `detail.url` is
 * the downloaded URL (the blob URL for exports) and `detail.state` is how it ended.
 */
export const DOWNLOAD_EVENT = 'astroimp:download';
export interface DownloadEventDetail { url: string; state: 'finished' | 'failed' | 'canceled' }

// Design decision: a browser needs the blob URL for only a moment, so a 10 s timer releases it. The
// desktop shell answers each download with a native Save dialog that waits for the user, so a timer
// could revoke the blob while the dialog is open. There the blob is released when the shell reports
// that exact download finished, failed or was canceled.
// Rejected: a long timer — it retains huge blobs after quick saves; Tauri IPC commands — they
// would expose native commands to a web origin.
const BROWSER_REVOKE_MS = 10_000;

/**
 * Starts a file download from a Blob or a same-origin URL with an anchor click, which a browser
 * saves directly and the desktop WebView routes to the shell's Save dialog.
 */
export function saveDownload(source: Blob | string, filename: string) {
  const url = typeof source === 'string' ? source : URL.createObjectURL(source);
  if (typeof source !== 'string' && isDesktop()) {
    const ended = (event: Event) => {
      if ((event as CustomEvent<DownloadEventDetail>).detail?.url !== url) return;
      window.removeEventListener(DOWNLOAD_EVENT, ended);
      URL.revokeObjectURL(url);
    };
    window.addEventListener(DOWNLOAD_EVENT, ended);
  } else if (typeof source !== 'string') {
    setTimeout(() => URL.revokeObjectURL(url), BROWSER_REVOKE_MS);
  }
  const link = document.createElement('a');
  link.href = url; link.download = filename; link.click();
}

/** The backend already saved this image. Native exports need no second copy or dialog. */
export function deliverExport(source: Blob | string, filename: string, saveAs = false) {
  if (!isDesktop() || saveAs) saveDownload(source, filename);
}

export function openProjectFolder(project: string) {
  return api(`/desktop/projects/${encodeURIComponent(project)}/folder`, 'POST', {}, project);
}

/** Reveal the primary library copy, including when Export as saved an additional copy elsewhere. */
export function revealExportFile(project: string, filename: string) {
  return api(`/desktop/projects/${encodeURIComponent(project)}/reveal`, 'POST', { filename }, project);
}

export function openExportImage(project: string, filename: string) {
  return api(`/desktop/projects/${encodeURIComponent(project)}/open-image`, 'POST', { filename }, project);
}
