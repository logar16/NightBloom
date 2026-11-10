import { useState } from 'react';
import { LoaderCircle, RotateCw } from 'lucide-react';
import { api } from '../api/client';
import { toast } from '../api/hooks';

const wait = (ms: number) => new Promise(resolve => setTimeout(resolve, ms));

/** Shown when the server's Python sources changed since it started. */
export function StaleServerBanner({ restartable }: { restartable: boolean }) {
  const [restarting, setRestarting] = useState(false);
  const restart = async () => {
    setRestarting(true);
    try {
      await api('/restart', 'POST');
    } catch (error) {
      setRestarting(false);
      toast((error as Error).message);
      return;
    }
    // The old server stops ~0.3 s after replying; wait for the new one (it may rebuild the UI first).
    await wait(1500);
    for (let attempt = 0; attempt < 240; attempt++) {
      try {
        const response = await fetch('/api/status', { cache: 'no-store' });
        if (response.ok && !(await response.json() as { stale?: boolean }).stale) { location.reload(); return; }
      } catch { /* Not up yet. */ }
      await wait(500);
    }
    setRestarting(false);
    toast('NightBloom did not come back. Check the terminal.');
  };
  return <div id="stale-server" className="stale-server" role="alert">
    {restarting ? <><LoaderCircle className="export-spinner" />Restarting NightBloom…</> : <>
      NightBloom was updated.
      {restartable
        ? <button className="action" onClick={() => void restart()}><RotateCw />Restart now</button>
        : <> Stop the server (Ctrl+C) and run <code>uv run astroimp</code> again.</>}
    </>}
  </div>;
}
