import { useEffect, useRef, useState } from 'react';
import { ArrowRight, Check, Circle, Crop, Download, Expand, Layers3, LoaderCircle, LockKeyhole, Combine, RefreshCw } from 'lucide-react';
import { useIsMutating, useQuery } from '@tanstack/react-query';
import { api, projectKey, queryClient } from '../../api/client';
import { activeJob } from '../../api/shell';
import { sessionAction } from '../../api/session-actions';
import { refreshSession, toast, useBuildPlan, useJob, useSession, useShell, useStatus } from '../../api/hooks';
import type { BuildGrid, Job, PlanFile, Session } from '../../api/types';
import { Popover } from '../../ui';
import { CanvasDialog } from '../canvas/CanvasDialog';
import { bytes, eta, progressMessage, waveStyle } from '../sky/utils';
import { jobProgress, sourceFieldsLabel, transferBytes, transferLabel, transferTotal, transferredBytes } from './sizes';

interface BuildHistory {
  id: string; built_at: string; current: boolean;
  layers: { filter: string; wavelength_nm: number }[];
}
export function BuildScreen() {
  const { data: session } = useSession();
  const { data: status } = useStatus();
  const { data: polledJob } = useJob();
  const { data: plan } = useBuildPlan(!!session);
  const { screen, setScreen } = useShell();
  const [mosaicChoice, setMosaicChoice] = useState<boolean | null>(null);
  const saving = useIsMutating({ mutationKey: ['session-write'] }) > 0 || mosaicChoice !== null;
  const [historyOpen, setHistoryOpen] = useState(false);
  const [starting, setStarting] = useState(false);
  const historyAnchor = useRef<HTMLButtonElement>(null);
  const [canvasOpen, setCanvasOpen] = useState(false);
  const [fillMargin, setFillMargin] = useState(.5);
  const { data: fillGrid } = useQuery({
    queryKey: ['fill-plan', session?.id, fillMargin, plan?.grid],
    queryFn: () => api<BuildGrid>(`/build/fill-around/plan?margin=${fillMargin}`),
    enabled: screen === 'build' && !!session?.edits && !!plan?.grid,
  });
  const { data: history } = useQuery({
    queryKey: ['builds', session?.id], queryFn: () => api<BuildHistory[]>('/builds'), enabled: historyOpen,
  });
  const job = activeJob(status?.job) ? status!.job : polledJob ?? status?.job;
  const running = activeJob(job);
  const buildJob = job?.kind === 'build' ? job : null;
  const files: PlanFile[] = buildJob?.files?.length ? buildJob.files
    : (plan?.files ?? []).map((file) => ({
      ...file, state: file.cached ? 'cached' : transferBytes(file) === 0 ? 'to prepare' : 'to download',
      done_bytes: file.cached ? file.size_bytes ?? 0 : 0,
    }));
  const selected = new Set(session?.selection.obs_ids ?? []);
  const mosaic = mosaicChoice ?? (session?.build?.mosaic !== false);
  const meta = session?.meta as { layers?: { obs_id: string; collection?: string; tiles?: { obs_id: string }[] }[]; mosaic?: boolean; build_version?: string | number } | null;
  const built = new Set(meta?.layers?.flatMap(layer => layer.tiles?.map(tile => tile.obs_id) ?? [layer.obs_id]) ?? []);
  const same = selected.size > 0 && selected.size === built.size && [...selected].every((id) => built.has(id));
  const mosaicChanged = !!meta && (meta.mosaic === true) !== mosaic;
  const outdated = same && (meta?.build_version !== session?.build_version || mosaicChanged || session?.rebuild_required);
  const missing = plan?.files.filter((file) => !file.cached) ?? [];
  const downloads = missing.filter(file => transferBytes(file) !== 0);
  const open = same && !missing.length && !outdated;
  const label = open ? 'Open editor' : downloads.length ? 'Download & Build' : same ? 'Rebuild' : 'Build';
  const phase = buildJob?.phase ?? (buildJob?.stage === 'reproject' ? 'align' : buildJob?.stage);
  const heading = buildJob?.state === 'queued' ? 'Queued' : buildJob?.state === 'error' ? 'Build failed' : buildJob?.state === 'done' ? 'Ready'
    : phase === 'download' ? files.some(file => file.state === 'downloading') ? 'Downloading' : 'Preparing sources'
      : phase === 'align' ? 'Aligning' : 'Writing layers';
  const cached = (plan?.files.length ?? 0) - missing.length;
  const planText = !plan?.files.length ? 'Select images on the Sky screen first.' : [
    downloads.length ? `Download ${downloads.length} image${downloads.length > 1 ? 's' : ''} (${transferLabel(downloads)})` : '',
    missing.length > downloads.length ? `Prepare ${missing.length - downloads.length} image${missing.length - downloads.length > 1 ? 's' : ''} (cached sources)` : '',
    cached ? `${cached} cached (${bytes(plan.cached_bytes)})` : '',
    `${plan.files.length} images → ${plan.layer_count ?? plan.files.length} layers`,
  ].filter(Boolean).join(' · ').replace(/^./, (c) => c.toUpperCase());
  const active = files.filter((file) => file.state === 'downloading').length;
  const assembling = files.filter((file) => file.state === 'assembling').length;
  const fetched = files.filter((file) => file.state === 'done').length;
  const toFetch = files.filter((file) => file.state !== 'cached').length;
  const summary = buildJob?.state === 'queued'
    ? `Queued · ${buildJob.message}${buildJob.waiting_for ? ` (${buildJob.waiting_for.stage} ${Math.round(buildJob.waiting_for.progress * 100)}%)` : ''}`
    : running && phase === 'download' && files.length
    ? [active ? `${active} downloading${active > 1 ? ' in parallel' : ''}` : '',
      assembling ? `${assembling} assembling` : '', `${fetched} of ${toFetch} done`,
      files.length > toFetch ? `${files.length - toFetch} cached` : ''].filter(Boolean).join(' · ')
    : running ? progressMessage(buildJob?.message || buildJob?.stage || 'Preparing')
      : buildJob?.state === 'error' ? progressMessage(buildJob.error ?? 'Build failed') : planText;
  const total = transferTotal(files);
  const done = files.reduce((n, file) => n + transferredBytes(file), 0);
  const percent = Math.round(buildJob ? jobProgress(buildJob) * 100 : 0);
  const [rate, setRate] = useState(0);
  const transferSample = useRef<{ id: string; time: number; bytes: number } | null>(null);
  useEffect(() => {
    if (!buildJob) return;
    const now = performance.now(), previous = transferSample.current;
    const transferred = (buildJob.files ?? []).filter((file) => file.state !== 'cached')
      .reduce((n, file) => n + transferredBytes(file), 0);
    if (previous?.id === buildJob.id && now - previous.time > 300)
      setRate(Math.max(0, (transferred - previous.bytes) / ((now - previous.time) / 1000)));
    else if (previous?.id !== buildJob.id) setRate(0);
    if (!previous || now - previous.time > 300)
      transferSample.current = { id: buildJob.id, time: now, bytes: transferred };
  }, [buildJob]);
  const log = useRef<HTMLPreElement>(null);
  useEffect(() => {
    if (!log.current) return;
    if (log.current.scrollTop + log.current.clientHeight >= log.current.scrollHeight - 90)
      log.current.scrollTop = log.current.scrollHeight;
  }, [buildJob?.log]);
  const start = () => {
    if (starting) return;
    setStarting(true);
    void sessionAction<Job>('/build').then((next) => {
      queryClient.setQueryData(projectKey('job'), next);
      void queryClient.invalidateQueries({ queryKey: ['status'] });
    }).catch((error: Error) => toast(error.message)).finally(() => setStarting(false));
  };
  const observations = session?.catalog.groups.flatMap((group) => group.observations) ?? [];
  const alignRemaining = (buildJob?.steps ?? []).filter((step) => step.label.startsWith('Align') && step.state !== 'done')
    .reduce((n, step) => n + 1 - step.progress, 0);
  return <section id="build" className={`screen ${screen === 'build' ? 'active' : ''}`}>
    <div className="screenbar"><div><span className="eyebrow">PREPARATION</span><h1>Build</h1></div>
      <button ref={historyAnchor} id="build-history" className="icon" title="Previous builds" aria-label="Previous builds"
        disabled={saving}
        onClick={() => setHistoryOpen(!historyOpen)}><Layers3 /></button>
    </div>
    <div className="build-workspace"><div className="build-summary">
      {buildJob && (running || buildJob.state === 'error') && <h2 id="build-heading">{heading}
        {running && <span data-testid="heading-dots" className="wave-dots" aria-label="…"><i>.</i><i>.</i><i>.</i></span>}</h2>}
      {!running && <div className="build-actions">
        <button id="build-start" data-testid="build-start" className="action build-go" disabled={!selected.size || !plan || saving || starting}
          onClick={() => open ? setScreen('edit') : start()}>
          {open ? <ArrowRight /> : downloads.length ? <Download /> : same ? <RefreshCw /> : <Layers3 />}
          <span>{label}</span>
        </button>
        {open && <button id="build-again" className="subtle" title="Align all layers again" disabled={saving || starting} onClick={() => start()}><RefreshCw />Rebuild</button>}
        <button className="subtle" title="Choose canvas" disabled={!selected.size || saving || starting}
          onClick={() => setCanvasOpen(true)}><Crop />Canvas</button>
        {session?.edits && meta?.layers?.some(layer => ['HST', 'JWST'].includes(layer.collection ?? '')) && <>
          <button id="fill-around" className="subtle" disabled={saving || starting}
            title="Add Hα (or DSS2 red), match its tone and soften the subject edges. Saves a Before fill checkpoint."
            onClick={() => {
              setStarting(true);
              void sessionAction<Job>('/build/fill-around', 'POST', { margin: fillMargin }).then(next => {
                queryClient.setQueryData(projectKey('job'), next);
                void queryClient.invalidateQueries({ queryKey: ['status'] });
              }).catch((error: Error) => toast(error.message)).finally(() => setStarting(false));
            }}><Expand />Fill around</button>
          <select aria-label="Fill margin per side" value={fillMargin}
            onChange={event => setFillMargin(Number(event.target.value))}>
            <option value={.25}>+25% per side</option><option value={.5}>+50% per side</option>
            <option value={1}>+100% per side</option>
          </select>
          {fillGrid && <span className="mono" title="Fill canvas; resolution is limited to 3072 pixels on the longest side">
            {fillGrid.width} × {fillGrid.height} · {fillGrid.pixel_scale_arcsec.toFixed(4)}″/px
          </span>}
        </>}
      </div>}
      <p id="build-message" data-testid="build-message" title="Internet-transfer estimate; ~ marks approximate sizes. Cached amounts are stored FITS file sizes.">{summary}</p>
      <div style={{ display: 'flex', alignItems: 'center', gap: 20 }}>
      <label className="field" title="Combine same-filter tiles into one seamless layer">
        <Combine size={14} /><span>Merge tiles</span><input type="checkbox" checked={mosaic}
          disabled={running || saving || starting} onChange={event => {
            const value = event.target.checked;
            setMosaicChoice(value);
            void api('/session/build', 'PUT', { mosaic: value }).then(() => {
              queryClient.setQueryData<Session>(projectKey('session'), old => old
                ? { ...old, build: { mosaic: value } } : old);
              refreshSession();
            }).catch((error: Error) => toast(error.message)).finally(() => setMosaicChoice(null));
          }} />
      </label>
      </div>
      {mosaicChanged && <p className="muted">Merge tiles changed · Rebuild required</p>}
      {session?.rebuild_required && <p className="muted">View rotation changed · Rebuild required</p>}
      {plan?.grid_locked && plan.grid && <p className="grid-lock mono"><LockKeyhole size={13} />
        {plan.grid.width} × {plan.grid.height} · {plan.grid.pixel_scale_arcsec.toFixed(4)}″/px · Locked
      </p>}
      {buildJob && <><div className="build-progress"><span id="build-progress" style={{ width: `${percent}%` }} /></div>
        <div id="build-percent" className="mono">{percent}%</div></>}
      <div id="transfer-total" data-testid="transfer-total" className="mono" title="Internet-transfer payload; cached files are excluded. ~ marks an estimate.">{buildJob &&
        <>{total === 0 ? 'No internet transfer needed' : `${bytes(done)} of ${transferLabel(files)}${phase === 'download' && rate > 0 ? ` · ${bytes(rate)}/s` : ''}`}
          {running && (buildJob.eta_s != null ? ` · ${eta(buildJob.eta_s)}` : phase === 'align' || phase === 'publish' ? ' · Estimating…' : '')}</>}</div>
      <div id="file-plan" data-testid="file-plan">{files.map((file) => <div key={file.obs_id}
        className={`file-row ${file.state?.replaceAll(' ', '-')}`}>
        <span className="filter-chip" style={waveStyle(observations.find((o) => o.obs_id === file.obs_id)?.wavelength_nm)}>{file.filter}</span>
        <span className="file-name" title={file.filename}>{file.filename}
          {file.source_fields?.length ? <span className="source-fields" title={file.source_fields.map(field =>
            `${field.name}: ${bytes(field.size_bytes)}${field.cached ? ' (cached)' : ' to fetch'}`).join('\n')}>{sourceFieldsLabel(file)}</span> : null}
        </span>
        <span className="file-size mono" title={file.cached ? 'Cached FITS file size' : file.transfer_note}>
          {file.cached ? bytes(file.size_bytes) : transferLabel([file])}</span>
        <span className="file-state">{file.state}</span>
        {!running && (plan?.files.find(f => f.obs_id === file.obs_id)?.outside_fraction ?? 0) >= .005 &&
          <small className="canvas-clipping">{Math.round((plan!.files.find(f => f.obs_id === file.obs_id)!.outside_fraction ?? 0) * 100)}% outside canvas</small>}
        <div className="file-progress"><i style={{
          width: `${file.state === 'cached' || file.state === 'done' ? 100 : file.state === 'assembling' ? (file.progress ?? 0) * 100
            : transferBytes(file) ? Math.min(99, transferredBytes(file) / transferBytes(file)! * 100) : 0}%`,
        }} /></div>
        {file.error && <small>{file.error}</small>}
      </div>)}</div>
      <div id="build-steps">{buildJob?.steps?.map((step) => {
        const seconds = step.eta_s === undefined ? (phase === 'align' && step.label.startsWith('Align') && alignRemaining && buildJob.eta_s != null
          ? buildJob.eta_s * (1 - step.progress) / alignRemaining : null) : step.eta_s;
        return <div key={step.key} className={`align-step ${step.state}`}>
        <span>{step.state === 'done' ? <Check size={12} /> : step.state === 'running'
          ? <LoaderCircle size={12} /> : <Circle size={4} />}</span>
        <span>{step.label}</span><span className="step-eta mono">{step.state !== 'done' && seconds != null
          ? eta(seconds) : step.state === 'running' ? 'Estimating…' : ''}</span>
        <span className="mono">{Math.round(step.progress * 100)}%</span>
        <div className="file-progress"><i style={{ width: `${step.state === 'done' ? 100 : step.progress * 100}%` }} /></div>
      </div>;
      })}</div>
    </div>
    <div className="build-log-panel"><div className="panel-heading"><span>ACTIVITY</span>
      <span id="job-stage" className="mono">{(buildJob?.stage ?? 'PLAN').toUpperCase()}</span></div>
      <pre ref={log} id="build-log">{buildJob?.log?.map((entry) =>
        progressMessage(typeof entry === 'string' ? entry : JSON.stringify(entry))).join('\n') ?? ''}</pre>
    </div></div>
    {canvasOpen && <CanvasDialog onClose={() => setCanvasOpen(false)} />}
    {historyOpen && <Popover anchor={historyAnchor} onClose={() => setHistoryOpen(false)} className="session-history">
      <div className="popover-title">Builds</div>
      {!history?.length && <p className="muted">No builds yet</p>}
      {history?.map((item) => <button key={item.id} className={item.current ? 'current' : ''}
        onClick={() => {
          setHistoryOpen(false);
          if (item.current) { setScreen('edit'); return; }
          void sessionAction(`/builds/${encodeURIComponent(item.id)}/restore`).then(() => {
            refreshSession(); setScreen('edit');
          }).catch((error: Error) => toast(error.message));
        }}><strong>{new Date(item.built_at).toLocaleString()}{item.current && <b>Current</b>}</strong>
        <small>{item.layers.map((layer, index) => <span key={index} className="filter-chip"
          style={waveStyle(layer.wavelength_nm)}>{layer.filter}</span>)}</small></button>)}
    </Popover>}
  </section>;
}
