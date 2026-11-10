import { useEffect, useRef, useState } from 'react';
import { FolderOutput, X } from 'lucide-react';
import { api, projectHeaders, projectKey, queryClient } from '../../api/client';
import { deliverExport, isDesktop } from '../../api/desktop';
import { activeJob } from '../../api/shell';
import { toast, useSession, useStatus } from '../../api/hooks';
import type { Job, Status } from '../../api/types';
import { IconButton } from '../../ui';

interface ExportPlan {
  width: number; height: number; preview_width: number; preview_height: number; estimated_s: number;
  estimate?: { fixed_s: number; scaled_s: number };
  max_long_side?: number;
}
type Resolution = 'preview' | '2k' | '4k' | 'native' | 'custom';
type Format = 'png8' | 'png16' | 'tiff16' | 'jpeg';
export interface ImageExportOptions { long_side?: number; format: Format; quality: number; preview: boolean; save_as?: boolean }
const storageKey = 'astroimp.image-export.v1';

export function useFullResolutionExport(open: boolean, flush: () => Promise<void>) {
  const [plan, setPlan] = useState<ExportPlan | null>(null), [busy, setBusy] = useState(false);
  const [error, setError] = useState('');
  const { data: status } = useStatus();
  const { data: session } = useSession();
  const running = activeJob(status?.job) && status?.job?.kind.startsWith('export');
  useEffect(() => {
    if (!open) return;
    let active = true;
    setPlan(null); setError('');
    void flush().then(() => api<ExportPlan>('/export/full/plan', 'GET', undefined, session?.id))
      .then(value => { if (active) setPlan(value); })
      .catch((e: Error) => { if (active) setError(e.message); });
    return () => { active = false; };
  }, [open, flush, session?.id]);

  async function start(options: ImageExportOptions) {
    setBusy(true);
    try {
      await flush();
      let job = await api<Job>('/export/full', 'POST', {
        format: options.format, quality: options.quality, long_side: options.long_side,
      }, session?.id);
      const id = job.id;
      queryClient.setQueryData<Status>(projectKey('status', job.session), status => status ? { ...status, job } : status);
      while (activeJob(job)) {
        await new Promise(resolve => setTimeout(resolve, 500));
        const next = await api<Job | null>('/job', 'GET', undefined, job.session);
        if (!next || next.id !== id) throw Error('Image export job is no longer available.');
        job = next;
        queryClient.setQueryData<Status>(projectKey('status', job.session), status => status ? { ...status, job } : status);
      }
      if (job.state === 'cancelled') return;
      if (job.state !== 'done' || !job.result?.download_url)
        throw Error(job.error || 'Image export failed.');
      deliverExport(job.result.download_url, job.result.path.split(/[\\/]/).at(-1) || 'full-resolution.png', options.save_as);
      toast(`${job.name || session?.name || 'Project'} · export finished`, job.session,
        isDesktop() ? job.result.path : undefined);
    } catch (error) { toast(error instanceof Error ? error.message : String(error)); }
    finally { setBusy(false); }
  }
  return { plan, error, busy: busy || running, start };
}

function storedChoices(): { resolution?: Resolution; format?: Format; custom?: number; quality?: number } {
  try { return JSON.parse(localStorage.getItem(storageKey) || '{}'); } catch { return {}; }
}

export function ImageExportDialog({ plan, error, preview, sessionId, measurePreviewPNG, onClose, onExport }: {
  plan: ExportPlan | null; error: string; preview: [number, number];
  sessionId: string;
  measurePreviewPNG?: () => Promise<number>;
  onClose: () => void; onExport: (options: ImageExportOptions) => void;
}) {
  const dialog = useRef<HTMLDialogElement>(null), [saved] = useState(storedChoices);
  const [resolution, setResolution] = useState<Resolution | ''>(
    ['preview','2k','4k','native','custom'].includes(saved.resolution || '') ? saved.resolution! : '');
  const [format, setFormat] = useState<Format>(
    ['png8','png16','tiff16','jpeg'].includes(saved.format || '') ? saved.format! : 'png8');
  const [custom, setCustom] = useState(saved.custom || 4096), [quality, setQuality] = useState(
    Math.max(1,Math.min(100,saved.quality || 92)));
  const [estimate, setEstimate] = useState<{ key: string; bytes?: number; error?: string; exact?: boolean } | null>(null);
  useEffect(() => { dialog.current?.showModal(); }, []);
  const nativeLong = plan ? Math.max(plan.width,plan.height) : 0;
  const maximumLong = plan?.max_long_side ?? nativeLong;
  const previewLong = Math.max(...preview);
  const choice = resolution || (maximumLong >= 4096 ? '4k' : nativeLong <= maximumLong ? 'native'
    : maximumLong >= 2048 ? '2k' : 'preview');
  const selected = (choice === '4k' && nativeLong < 4096) || (choice === '2k' && nativeLong < 2048)
    ? 'native' : choice;
  const longSide = selected === 'preview' ? previewLong : selected === '2k' ? 2048
    : selected === '4k' ? 4096 : selected === 'custom' ? custom : nativeLong;
  const instant = selected === 'preview' && format === 'png8';
  const valid = instant || (!!plan && longSide >= 1 && Number.isInteger(longSide) && longSide <= maximumLong);
  const [width, height] = selected === 'native' && plan ? [plan.width, plan.height]
    : preview.map(value => Math.max(1,Math.round(value/previewLong*longSide)));
  const sizeQuality = format === 'jpeg' ? quality : 92;
  const estimateKey = `${sessionId}:${width}:${height}:${format}:${sizeQuality}`;
  const estimateReady = (instant && !!measurePreviewPNG) || !!plan || !!error;
  useEffect(() => {
    if (!isDesktop() || !estimateReady || !valid) return;
    const controller = new AbortController();
    const timer = setTimeout(() => {
      void (async () => {
        const exact = instant && !!measurePreviewPNG;
        let bytes: number;
        if (instant && measurePreviewPNG) bytes = await measurePreviewPNG();
        else {
          const response = await fetch('/api/export/full/size', {
            method: 'POST', signal: controller.signal,
            headers: { ...projectHeaders(sessionId), 'Content-Type': 'application/json' },
            body: JSON.stringify({ width, height, format, quality: sizeQuality }),
          });
          const result: unknown = await response.json();
          if (!response.ok) throw Error(result && typeof result === 'object' && 'detail' in result
            ? String(result.detail) : 'File size could not be estimated.');
          if (!result || typeof result !== 'object' || !('estimated_bytes' in result)
            || typeof result.estimated_bytes !== 'number') throw Error('Invalid file-size estimate.');
          bytes = result.estimated_bytes;
        }
        if (!Number.isFinite(bytes) || bytes < 0) throw Error('Invalid file-size estimate.');
        if (!controller.signal.aborted) setEstimate({ key: estimateKey, bytes, exact });
      })().catch((error: unknown) => {
        if (!controller.signal.aborted) setEstimate({ key: estimateKey,
          error: error instanceof Error ? error.message : String(error) });
      });
    }, 250);
    return () => { clearTimeout(timer); controller.abort(); };
  }, [estimateKey, estimateReady, valid, sessionId, width, height, format, sizeQuality, instant, measurePreviewPNG]);
  const measured = estimate?.key === estimateKey ? estimate : null;
  const fileSize = !valid ? '—' : measured?.error ? 'Size unavailable'
    : measured?.bytes === undefined ? 'Estimating size…'
    : measured.bytes >= 1_000_000 ? `≈${(measured.bytes/1_000_000).toFixed(1)} MB`
    : `≈${Math.max(1,Math.round(measured.bytes/1000))} KB`;
  const size = (side: number) => preview.map(value => Math.max(1,Math.round(value/previewLong*side))).join(' × ');
  const area = (longSide/nativeLong)**2;
  const seconds = !plan ? 0 : plan.estimate ? plan.estimate.fixed_s + plan.estimate.scaled_s*area
    : plan.estimated_s*area;
  const time = instant ? 'Instant' : !plan ? 'Reading source resolution…'
    : seconds < 60 ? `≈${Math.max(1,Math.round(seconds))} s` : `≈${Math.ceil(seconds/60)} min`;
  function saveAndExport(saveAs = false) {
    try { localStorage.setItem(storageKey,JSON.stringify({ resolution:selected,format,custom,quality })); } catch { /* Storage may be disabled. */ }
    onExport({ format,quality,preview:instant,long_side:selected === 'native' ? undefined : longSide,save_as:saveAs });
  }
  return <dialog ref={dialog} className="image-export-dialog" aria-labelledby="image-export-title" onClose={onClose}>
    <div className="dialog-bar"><h2 id="image-export-title">Export image</h2>
      <IconButton icon={X} title="Close image export" onClick={() => dialog.current?.close()} /></div>
    <fieldset><legend>Resolution</legend><div className="export-resolutions">
      {(['preview','2k','4k','native'] as const).map(value => {
        const side = value === 'preview' ? previewLong : value === '2k' ? 2048 : value === '4k' ? 4096 : nativeLong;
        const disabled = value !== 'preview' && (!plan || side > maximumLong);
        return <label key={value} className={disabled ? 'unavailable' : ''}
          title={disabled && plan?.max_long_side !== undefined ? plan.max_long_side === 0
            ? 'Source-backed export is unavailable; use Preview PNG (8-bit)'
            : 'Exceeds the desktop export memory budget' : undefined}>
          <input type="radio" name="export-resolution" value={value} checked={selected === value}
            disabled={disabled} onChange={() => setResolution(value)} />
          <span>{value === 'preview' ? 'Preview' : value === 'native' ? 'Native' : value.toUpperCase()}</span>
          <small className="mono">{value === 'native' && plan ? `≈${plan.width.toLocaleString()} × ${plan.height.toLocaleString()}`
            : side ? size(side) : '—'}</small>
        </label>;
      })}
    </div><label className="export-custom">
      <input type="radio" name="export-resolution" checked={selected === 'custom'}
        onChange={() => setResolution('custom')} />Custom
      <input type="number" aria-label="Custom long side" min={1} max={maximumLong || undefined} step={1}
        value={custom} onChange={e => { setCustom(Number(e.target.value)); setResolution('custom'); }} />
      <span className="mono">{size(custom)}</span>
    </label></fieldset>
    <fieldset><legend>Format</legend><div className="export-formats">
      {([['png8','PNG (8-bit)'],['png16','PNG (16-bit)'],['tiff16','TIFF (16-bit)'],['jpeg','JPEG']] as const)
        .map(([value,label]) => <label key={value} title={value === 'tiff16' ? 'For further editing' : undefined}>
          <input type="radio" name="export-format" checked={format === value} onChange={() => setFormat(value)} />{label}
        </label>)}
    </div>{format === 'jpeg' && <label className="export-quality">Quality
      <input type="range" aria-label="JPEG quality" min={1} max={100} value={quality}
        onChange={e => setQuality(Number(e.target.value))} /><output>{quality}</output>
    </label>}</fieldset>
    {error && <p role="alert">{error}</p>}
    <div className="export-dialog-footer"><div className="export-estimates mono">
      {isDesktop() && <span aria-label="Estimated file size" title={measured?.error
        || (measured?.exact ? 'Measured from the current Preview PNG.'
          : format === 'tiff16' ? 'Uncompressed RGB16 size; includes estimated TIFF metadata.'
          : 'Estimated from the current encoded preview. Final size varies with source detail.')}>{fileSize}</span>}
      <small>{time}</small></div>
      <div className="export-footer-actions">{isDesktop() && <IconButton icon={FolderOutput} title="Export as…"
        disabled={!valid} onClick={() => saveAndExport(true)} />}
        <button className="action" disabled={!valid} onClick={() => saveAndExport()}>Export</button></div></div>
  </dialog>;
}
