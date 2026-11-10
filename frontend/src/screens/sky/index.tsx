import { useEffect, useMemo, useRef, useState } from 'react';
import { ArrowRight, Check, CloudOff, EyeOff, Layers3, LoaderCircle, Orbit, RefreshCw, Search, Star, Telescope, X } from 'lucide-react';
import { useIsMutating, useMutation } from '@tanstack/react-query';
import { api, projectKey, queryClient } from '../../api/client';
import { activeJob } from '../../api/shell';
import { refreshSession, toast, useBuildPlan, useCancelJob, useSession, useShell, useStatus } from '../../api/hooks';
import type { Observation, Session } from '../../api/types';
import { IconButton } from '../../ui';
import { Catalog } from './Catalog';
import { Preview } from './Preview';
import { Snapshot } from './Snapshot';
import type { Point } from './Snapshot';
import { WaveFilter } from './WaveFilter';
import './sky.css';
import type { WaveRange } from './WaveFilter';
import { bytes, insidePolygon, waveStyle } from './utils';
import { useBookmarks } from '../../api/bookmarks';

function loadRange(id?: string): WaveRange | null {
  try { return JSON.parse(localStorage.getItem(`astroimp.waveRange.${id}`) ?? 'null') as WaveRange | null; }
  catch { return null; }
}
export function SkyScreen() {
  const { data: session } = useSession();
  const bookmarks = useBookmarks();
  const bookmarked = new Set(bookmarks.data?.map(item => item.obs_id) ?? []);
  const bookmark = (ids: string[]) => {
    if (session) bookmarks.toggle.mutate({ ids, session: session.id });
  };
  const bookmarkBusy = bookmarks.isPending || bookmarks.isError || bookmarks.toggle.isPending;
  const { data: status } = useStatus();
  const { data: plan } = useBuildPlan(!!session);
  const { screen, setScreen } = useShell();
  const saving = useIsMutating({ mutationKey: ['session-write'] }) > 0;
  const recapturing = useIsMutating({ mutationKey: ['recapture', session?.id] }) > 0;
  const rotating = useIsMutating({ mutationKey: ['session-write', 'rotation', session?.id] }) > 0;
  const blockingJob = activeJob(status?.job) ? status?.job : null;
  const jobName = blockingJob?.name || blockingJob?.kind.replaceAll('-', ' ');
  const busyReason = blockingJob ? blockingJob.cancel_requested ? `Cancelling ${jobName}…`
    : `${jobName} · ${blockingJob.message || blockingJob.stage}`
    : rotating ? 'Updating capture rotation…' : recapturing ? 'Recapturing sky…' : undefined;
  const writeBlocked = !!busyReason;
  const [collections, setCollections] = useState(new Set(['HST', 'JWST', 'Survey']));
  const [multi, setMulti] = useState(true);
  const [search, setSearch] = useState('');
  const [point, setPoint] = useState<Point | null>(null);
  const [range, setRange] = useState<WaveRange | null>(() => loadRange(session?.id));
  const [hovered, setHovered] = useState<number | string | null>(null);
  const [flash, setFlash] = useState<string | null>(null);
  const [showDismissed, setShowDismissed] = useState(false);
  const [showSoft, setShowSoft] = useState<boolean | null>(null);
  const [preview, setPreview] = useState<{ group: number; index: number } | null>(null);
  const flashTimer = useRef<ReturnType<typeof setTimeout> | undefined>(undefined);
  const observations = useMemo(() => session?.catalog.groups.flatMap((group) => group.observations)
    .filter(o => o.instrument !== 'Pan-STARRS' || session.selection.obs_ids.includes(o.obs_id)) ?? [],
  [session?.catalog, session?.selection.obs_ids]);
  const available = useMemo(() => new Set(observations.map(o => o.obs_id)), [observations]);
  const selected = new Set(session?.selection.obs_ids ?? []);
  const favorites = new Set(session?.triage.favorites ?? []);
  const dismissed = new Set(session?.triage.dismissed ?? []);
  const pointIds = useMemo(() => new Set(point ? observations.filter((o) =>
    (o.pixels ?? []).some((polygon) => insidePolygon(point.x, point.y, polygon))).map((o) => o.obs_id) : []),
  [point, observations]);
  useEffect(() => {
    setRange(loadRange(session?.id)); setPoint(null); setPreview(null); setShowSoft(null);
  }, [session?.id]);
  useEffect(() => {
    const key = (event: KeyboardEvent) => {
      if (screen !== 'sky' || event.defaultPrevented || document.querySelector('dialog[open]')
        || (event.target instanceof Element && event.target.closest('input,textarea,select,[contenteditable=true],.ui-popover'))) return;
      if (event.key === 'Escape') setPoint(null);
    };
    document.addEventListener('keydown', key);
    return () => document.removeEventListener('keydown', key);
  }, [screen]);
  useEffect(() => () => clearTimeout(flashTimer.current), []);
  const selectionMutation = useMutation({
    mutationKey: ['session-write', 'selection'],
    mutationFn: (ids: string[]) => api('/selection', 'PUT', { obs_ids: ids }),
    scope: { id: 'selection' },
    onError: (error: Error) => { toast(error.message); refreshSession(); },
    onSettled: () => { void queryClient.invalidateQueries({ queryKey: ['plan'] }); },
  });
  const saveSelection = (ids: string[]) => {
    if (busyReason) { toast(busyReason); return; }
    queryClient.setQueryData<Session>(projectKey('session'), (current) => current
      ? { ...current, selection: { obs_ids: ids } } : current);
    selectionMutation.mutate(ids);
  };
  const toggle = (id: string) => {
    const ids = new Set(queryClient.getQueryData<Session>(projectKey('session'))?.selection.obs_ids ?? []);
    ids.has(id) ? ids.delete(id) : ids.add(id);
    saveSelection([...ids]);
  };
  const triageMutation = useMutation({
    mutationKey: ['session-write', 'triage'],
    mutationFn: (triage: Session['triage']) => api('/triage', 'PUT', triage), scope: { id: 'triage' },
    onError: (error: Error) => { toast(error.message); refreshSession(); },
  });
  const triage = (ids: string[], kind: 'favorite' | 'dismiss') => {
    if (busyReason) { toast(busyReason); return; }
    const current = queryClient.getQueryData<Session>(projectKey('session'));
    if (!current) return;
    const key = kind === 'favorite' ? 'favorites' : 'dismissed';
    const set = new Set(current.triage[key]);
    const on = !ids.every((id) => set.has(id));
    ids.forEach((id) => on ? set.add(id) : set.delete(id));
    const next = { ...current.triage, [key]: [...set] };
    queryClient.setQueryData<Session>(projectKey('session'), { ...current, triage: next });
    triageMutation.mutate(next);
  };
  const changeRange = (next: WaveRange | null) => {
    setRange(next); localStorage.setItem(`astroimp.waveRange.${session?.id}`, JSON.stringify(next));
  };
  const groups = (session?.catalog.groups ?? []).map((group, originalIndex) => ({
    ...group, originalIndex, observations: group.observations.filter((o) =>
      available.has(o.obs_id) &&
      (!point || pointIds.has(o.obs_id)) &&
      (!range || o.wavelength_nm != null && o.wavelength_nm >= range.lo && o.wavelength_nm <= range.hi) &&
      (!search || [o.target, group.title, ...(group.targets ?? []), o.instrument, o.filter]
        .join(' ').toLowerCase().includes(search.toLowerCase()))),
  })).filter((group) => collections.has(group.collection) && group.observations.length &&
    (!multi || point || group.collection === 'Survey' ||
      new Set(session?.catalog.groups[group.originalIndex].observations.map((o) => o.filter)).size >= 2));
  groups.sort((a, b) => Number(b.observations.some((o) => favorites.has(o.obs_id))) -
    Number(a.observations.some((o) => favorites.has(o.obs_id))) || a.originalIndex - b.originalIndex);
  const hasPrimary = groups.some(group => group.observations.some(o =>
    !dismissed.has(o.obs_id) && !(o.collection === 'Survey' && o.soft)));
  const filteredIds = groups.flatMap((group) => group.observations.map((o) => o.obs_id));
  const jump = (id: string) => {
    setSearch(''); setMulti(false); setPoint(null); changeRange(null);
    setCollections(new Set(['HST', 'JWST', 'Survey']));
    if (dismissed.has(id)) setShowDismissed(true);
    if (observations.find(o => o.obs_id === id)?.soft) setShowSoft(true);
    setFlash(id); clearTimeout(flashTimer.current);
    requestAnimationFrame(() => requestAnimationFrame(() => {
      document.querySelector<HTMLElement>(`.observation[data-obs="${CSS.escape(id)}"]`)
        ?.scrollIntoView({ block: 'center', behavior: 'instant' });
    }));
    flashTimer.current = setTimeout(() => setFlash(null), 2200);
  };
  const cancelJob = useCancelJob();
  const captureJob = ['capture', 'search-mast'].includes(status?.job?.kind ?? '') &&
    activeJob(status?.job) ? status?.job : null;
  const capturing = !!captureJob || recapturing;
  const buildReason = busyReason ?? (selectionMutation.isPending ? 'Saving image selection…'
    : !selected.size ? 'Select at least one image' : undefined);
  return <section id="sky" className={`screen ${screen === 'sky' ? 'active' : ''}`}>
    <div className="screenbar sky-sessionbar">
      {!capturing && <>
      <span id="coordinates" className="mono">{session?.view.center_icrs_deg?.map((v) => `${v.toFixed(4)}°`).join('  ')}</span>
      <span id="view-size" className="mono">{session?.view.field_arcmin?.map((v) => v.toFixed(2)).join(' × ')}{session ? ' arcmin' : ''}</span>
      <WaveFilter observations={observations} selected={selected} range={range} onChange={changeRange} />
      </>}
    </div>
    {capturing ? <div className="capture-progress" role="status" aria-live="polite">
      <Telescope className="capture-icon" />
      <h2>Searching the sky<span className="wave-dots" aria-hidden="true"><i>.</i><i>.</i><i>.</i></span></h2>
      <p className="mono">{captureJob?.message || captureJob?.stage || 'Reading Aladin view'}
        {captureJob?.stage === 'search' && <Elapsed key={captureJob.id} />}</p>
      <div className={`capture-bar${captureJob?.stage === 'search' && !captureJob.progress ? ' indeterminate' : ''}`}>
        <span style={{ width: `${Math.round((captureJob?.progress ?? 0) * 100)}%` }} /></div>
      {captureJob && <button className="subtle" onClick={() => cancelJob.mutate()}><X />Cancel</button>}
    </div> : <>
    <div className="sky-workspace">
      <Snapshot session={session} available={available} selected={selected} favorites={favorites} dismissed={dismissed}
        hovered={hovered} flash={flash} point={point} onPoint={setPoint} onJump={jump} onHome={() => setScreen('home')}
        busy={activeJob(status?.job) || saving} />
      <div className="catalog-panel">
        <div className="catalog-toolbar">
          <div className="segmented">{['HST', 'JWST', 'Survey'].map((collection) =>
            <button key={collection} data-collection={collection} aria-pressed={collections.has(collection)}
              className={collections.has(collection) ? 'active' : ''} onClick={() => setCollections((current) => {
                const next = new Set(current); next.has(collection) ? next.delete(collection) : next.add(collection); return next;
              })}>{collection === 'Survey' ? 'Surveys' : collection}</button>)}</div>
          {session?.catalog.mast_searched === false && <button className="subtle" disabled={saving || writeBlocked}
            title="This wide frame was checked for surveys only. Search smaller HST/JWST footprints within it."
            onClick={() => {
              void api('/session/search-mast', 'POST').then(() =>
                queryClient.invalidateQueries({ queryKey: ['status'] }))
                .catch((error: Error) => toast(error.message));
            }}><Telescope />Also search HST/JWST</button>}
          <button id="multi-filter" className={`toggle ${multi ? 'active' : ''}`}
            title="Show groups with at least two filters" aria-pressed={multi} onClick={() => setMulti(!multi)}>≥ 2 filters</button>
          <div className="search"><Search /><input id="catalog-search" type="search"
            placeholder="Search target, filter, instrument" aria-label="Search target, filter, instrument"
            value={search} onChange={(event) => setSearch(event.target.value)} /></div>
          <IconButton id="show-footprints" icon={Orbit} title="Show in Aladin" onClick={() => {
            void api('/aladin/footprints', 'POST', { obs_ids: point ? filteredIds : [...selected] })
              .then(() => toast('Footprints sent to Aladin')).catch((error: Error) => toast(error.message));
          }} />
          <span id="group-count" className="mono">{groups.filter((group) => group.observations.some((o) => !dismissed.has(o.obs_id))).length}</span>
          {point && <div id="point-actions">
            <button id="clear-point" className="subtle" onClick={() => setPoint(null)}>Covering point ×</button>
            <IconButton id="point-favorite" icon={Star} title="Favorite all covering point" onClick={() => triage(filteredIds, 'favorite')} />
            <IconButton id="point-dismiss" icon={EyeOff} title="Dismiss all covering point" onClick={() => triage(filteredIds, 'dismiss')} />
            <IconButton id="point-select" icon={Check} title="Select all covering point"
              onClick={() => saveSelection([...new Set([...selected, ...filteredIds])])} />
          </div>}
        </div>
        {session?.catalog.survey_warning && <div className="sky-callout" role="alert">
          <CloudOff aria-hidden />
          <p>{session.catalog.survey_warning}</p>
          <button className="subtle" disabled={saving || writeBlocked} onClick={() => {
            void api('/session/search-surveys', 'POST').then(() =>
              queryClient.invalidateQueries({ queryKey: ['status'] }))
              .catch((error: Error) => toast(error.message));
          }}><RefreshCw />Retry</button>
        </div>}
        {session && <Catalog groups={groups} view={session.view} selected={selected} favorites={favorites} dismissed={dismissed} busy={writeBlocked}
          showSoft={showSoft ?? !hasPrimary} onSoft={setShowSoft}
          showDismissed={showDismissed} onDismissed={setShowDismissed} onToggle={toggle} onTriage={triage}
          bookmarked={bookmarked} onBookmark={bookmark} bookmarkBusy={bookmarkBusy}
          onGroupToggle={ids => {
            const next = new Set(queryClient.getQueryData<Session>(projectKey('session'))?.selection.obs_ids ?? []);
            const remove = ids.every(id => next.has(id));
            ids.forEach(id => remove ? next.delete(id) : next.add(id));
            saveSelection([...next]);
          }}
          onHover={setHovered} flash={flash} onPreview={(o: Observation) => {
            const group = session!.catalog.groups.findIndex((g) => g.observations.some((item) => item.obs_id === o.obs_id));
            setPreview({ group, index: session!.catalog.groups[group].observations.findIndex((item) => item.obs_id === o.obs_id) });
          }} />}
      </div>
    </div>
    <footer className="selection-tray">
      <div className="selection-label"><span id="selected-count" className="mono">{selected.size}</span> selected
        <small id="selection-size">{plan ? `${bytes(plan.total_bytes)}${plan.cached_bytes ? ` · ${bytes(plan.cached_bytes)} cached` : ''}` : ''}</small></div>
      <div id="selection-chips">{observations.filter((o) => selected.has(o.obs_id)).map((o) =>
        <span key={o.obs_id} className="selection-chip" style={waveStyle(o.wavelength_nm)}>
          <button data-jump={o.obs_id} title={`Find ${o.filter}`} aria-label={`Find ${o.filter}`} onClick={() => jump(o.obs_id)}>{o.filter}</button>
          <button data-remove={o.obs_id} title={`Deselect ${o.filter}`} aria-label={`Deselect ${o.filter}`}
            disabled={writeBlocked} onClick={() => toggle(o.obs_id)}><X size={12} /></button>
        </span>)}</div>
      <IconButton id="clear-selection" icon={X} title="Clear selection" disabled={writeBlocked || !selected.size}
        onClick={() => saveSelection([])} />
      {(busyReason || selectionMutation.isPending) && <span className="selection-blocker" role="status">
        <LoaderCircle className="export-spinner" aria-hidden />
        <span title={buildReason}>{buildReason}</span>
        {blockingJob && <IconButton icon={X} title={`Cancel ${jobName}`}
          disabled={blockingJob.cancel_requested || cancelJob.isPending}
          onClick={() => cancelJob.mutate(blockingJob.id)} />}
      </span>}
      <span className="selection-build" title={buildReason}>
        <button className="action" id="build-button" disabled={!!buildReason} aria-description={buildReason}
          onClick={() => setScreen('build')}><Layers3 />Build<ArrowRight /></button>
      </span>
    </footer>
    </>}
    {preview && session && <Preview group={session.catalog.groups[preview.group]} view={session.view} index={preview.index} busy={writeBlocked}
      bookmarked={bookmarked} onBookmark={bookmark} bookmarkBusy={bookmarkBusy}
      selected={selected} favorites={favorites} dismissed={dismissed} onToggle={toggle} onTriage={triage} onClose={() => setPreview(null)}
      onIndex={(index) => setPreview({ ...preview, index })} />}
  </section>;
}

function Elapsed() {
  const [start] = useState(() => Date.now()), [, tick] = useState(0);
  useEffect(() => { const timer = setInterval(() => tick(v => v + 1), 1000); return () => clearInterval(timer); }, []);
  return <span className="capture-elapsed"> · {Math.floor((Date.now() - start) / 1000)} s</span>;
}
