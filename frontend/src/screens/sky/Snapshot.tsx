import { useEffect, useRef, useState } from 'react';
import type { CSSProperties } from 'react';
import { Focus, House, LoaderCircle, Orbit, RefreshCw, RotateCcw, ScanLine, SunMedium, WandSparkles, X } from 'lucide-react';
import { useIsMutating, useMutation } from '@tanstack/react-query';
import type { Job, Observation, Session } from '../../api/types';
import { api, queryClient } from '../../api/client';
import { refreshSession, toast, useStatus } from '../../api/hooks';
import { IconButton, Popover, Slider } from '../../ui';
import { wavelengthColor } from './utils';
import { CaptureCompass, RotationControl } from './Rotation';

export interface Point { x: number; y: number }
interface Tone { black: number; white: number; midtones: number; saturation: number }
const defaultTone: Tone = { black: 0, white: 1, midtones: 1, saturation: 1 };
function loadTone(id?: string): Tone {
  try { return { ...defaultTone, ...JSON.parse(localStorage.getItem(`astroimp.snapshotTone.${id}`) ?? '{}') }; }
  catch { return defaultTone; }
}
export function Snapshot({ session, available, selected, favorites, dismissed, hovered, flash, point, onPoint, onJump, onHome, busy = false }: {
  session?: Session; available: Set<string>; selected: Set<string>; favorites: Set<string>; dismissed: Set<string>;
  hovered: number | string | null; flash: string | null; point: Point | null;
  onPoint: (point: Point | null) => void; onJump: (id: string) => void; onHome: () => void;
  busy?: boolean;
}) {
  const wrap = useRef<HTMLDivElement>(null);
  const image = useRef<HTMLImageElement>(null);
  const anchor = useRef<HTMLButtonElement>(null);
  const [size, setSize] = useState({ width: 100, height: 100 });
  const [tone, setTone] = useState<Tone>(() => loadTone(session?.id));
  const [toneOpen, setToneOpen] = useState(false);
  const [imageRevision, setImageRevision] = useState(0);
  const [refreshing, setRefreshing] = useState(false);
  const [focusFootprints, setFocusFootprints] = useState(() => localStorage.getItem('astroimp.footprintFocus') === '1');
  const toggleFocus = () => setFocusFootprints(on => {
    localStorage.setItem('astroimp.footprintFocus', on ? '0' : '1'); return !on;
  });
  const [snapshotFailed, setSnapshotFailed] = useState(false);
  const [loadedSnapshot, setLoadedSnapshot] = useState('');
  const rotating = useIsMutating({ mutationKey: ['session-write', 'rotation', session?.id] }) > 0;
  const { data: status } = useStatus();
  const coordinates = session?.view.source === 'coordinates';
  const [angle, setAngle] = useState(session?.view.rotation_deg ?? 0);
  useEffect(() => { setAngle(session?.view.rotation_deg ?? 0); }, [session?.id, session?.view.rotation_deg]);
  const snapshotSrc = !session ? '' : coordinates
    ? `/api/session/backdrop.jpg?session=${session.id}&rotation=${session.view.rotation_deg ?? 0}&v=hips-fits-v2-dss-overlap-v2-${imageRevision}`
    : `/api/session/snapshot.jpg?session=${session.id}&rotation=${session.view.rotation_deg ?? 0}&v=${imageRevision}-${session.view.captured_at_utc ?? ''}`;
  const updating = !!session && (rotating || refreshing || angle !== (session.view.rotation_deg ?? 0) ||
    !snapshotFailed && loadedSnapshot !== snapshotSrc);
  const [w, h] = session?.view.pixel_dimensions ?? [1000, 1000];
  useEffect(() => {
    const fit = () => {
      if (!wrap.current) return;
      const factor = Math.min((wrap.current.clientWidth - 36) / w, (wrap.current.clientHeight - 36) / h);
      setSize({ width: Math.max(10, w * factor), height: Math.max(10, h * factor) });
    };
    const observer = new ResizeObserver(fit);
    if (wrap.current) observer.observe(wrap.current);
    fit(); return () => observer.disconnect();
  }, [w, h]);
  useEffect(() => { setTone(loadTone(session?.id)); }, [session?.id, session?.view.captured_at_utc]);
  useEffect(() => { setSnapshotFailed(false); }, [snapshotSrc]);
  useEffect(() => {
    if (coordinates && status?.services?.cds?.ok) setSnapshotFailed(false);
  }, [coordinates, status?.services?.cds?.ok]);
  const changeTone = (next: Tone, save = true) => {
    setTone(next);
    if (save) localStorage.setItem(`astroimp.snapshotTone.${session?.id}`, JSON.stringify(next));
  };
  const recapture = useMutation({
    mutationKey: ['recapture', session?.id],
    mutationFn: async (id: string) => {
      let job = await api<Job>('/session/recapture', 'POST', undefined, id);
      void queryClient.invalidateQueries({ queryKey: ['status'] });
      while (job.state === 'running' || job.state === 'queued') {
        await new Promise(resolve => setTimeout(resolve, 400));
        job = await api<Job>('/job', 'GET', undefined, id);
      }
      if (job.state !== 'done') throw Error(job.error || 'Recapture cancelled');
    },
    onSuccess: async (_, id) => {
      localStorage.setItem(`astroimp.snapshotTone.${id}`, JSON.stringify(defaultTone));
      await Promise.all(['session', 'sessions', 'plan'].map(key =>
        queryClient.invalidateQueries({ queryKey: [key] })));
    },
    onError: (error: Error, id) => toast(error.message, id),
  });
  const autoTone = () => {
    if (!image.current) return;
    const canvas = document.createElement('canvas'), img = image.current;
    const scale = Math.min(1, 400 / Math.max(img.naturalWidth, 1));
    canvas.width = Math.max(1, Math.round(img.naturalWidth * scale));
    canvas.height = Math.max(1, Math.round(img.naturalHeight * scale));
    const context = canvas.getContext('2d', { willReadFrequently: true });
    if (!context) return;
    context.drawImage(img, 0, 0, canvas.width, canvas.height);
    const data = context.getImageData(0, 0, canvas.width, canvas.height).data;
    const histogram = new Uint32Array(256);
    for (let i = 0; i < data.length; i += 4)
      histogram[Math.round(.2126 * data[i] + .7152 * data[i + 1] + .0722 * data[i + 2])]++;
    const percentile = (q: number) => {
      let n = 0;
      for (let v = 0; v < 256; v++) { n += histogram[v]; if (n >= q * data.length / 4) return v / 255; }
      return 1;
    };
    const black = percentile(.005), white = Math.max(black + .05, percentile(.998));
    const median = Math.min(.98, Math.max(.02, (percentile(.5) - black) / (white - black)));
    changeTone({
      ...tone, black: Number(black.toFixed(3)), white: Number(white.toFixed(3)),
      midtones: Number(Math.min(4, Math.max(.3, Math.log(.3) / Math.log(median))).toFixed(2)),
    });
  };
  const neutral = Object.keys(defaultTone).every((key) => tone[key as keyof Tone] === defaultTone[key as keyof Tone]);
  const range = Math.max(.01, tone.white - tone.black);
  return <div className="sky-view">
    <div className="panel-heading snapshot-toolbar"><span>{coordinates ? 'SKY FRAME' : 'ALADIN VIEW'}</span><div className="snapshot-actions">
      <span id="catalog-count">{available.size} observations</span>
      {!coordinates && <button id="recapture" className="capture-action"
        title={session?.meta ? 'Built projects keep their view. Use New project for a new view.'
          : status?.aladin ? 'Replace this view and search with the current Aladin view. Clears selected images.' : 'Open Aladin Desktop to recapture'}
        disabled={!session || !!session.meta || !status?.aladin || refreshing || recapture.isPending || busy}
        onClick={() => session && recapture.mutate(session.id)}>
        {recapture.isPending ? <LoaderCircle className="export-spinner" aria-hidden /> : <ScanLine aria-hidden />}Recapture
      </button>}
      {!coordinates && <button id="refresh-snapshot" className="capture-action"
        title={status?.aladin ? 'Refresh the Aladin image without searching again. Keep the same sky position and zoom.'
          : 'Open Aladin Desktop to refresh the image'}
        disabled={!session || !status?.aladin || refreshing || recapture.isPending || busy}
        onClick={() => {
          setRefreshing(true);
          void api('/session/snapshot/refresh', 'POST').then(() => {
            changeTone(defaultTone); setImageRevision(Date.now()); refreshSession(); toast('Image refreshed from Aladin');
          }).catch((error: Error) => toast(error.message)).finally(() => setRefreshing(false));
        }}>
        {refreshing ? <LoaderCircle className="export-spinner" aria-hidden /> : <RefreshCw aria-hidden />}Refresh image
      </button>}
      <IconButton id="focus-footprints" icon={Focus} className={`small ${focusFootprints ? 'active' : ''}`}
        aria-pressed={focusFootprints}
        title={focusFootprints ? 'Show all footprints' : 'Show only selected and hovered footprints'}
        disabled={!session} onClick={toggleFocus} />
      <button ref={anchor} id="snapshot-tone" className={`icon small ${neutral ? '' : 'active'}`}
        disabled={!session || snapshotFailed || updating}
        title="Adjust screenshot tone (not science images)" aria-label="Adjust view tone"
        aria-expanded={toneOpen} onClick={() => setToneOpen(!toneOpen)}><SunMedium /></button>
    </div></div>
    <div ref={wrap} id="snapshot-wrap" className="snapshot-wrap">
      {!session && <div id="sky-empty" className="empty-state"><Orbit /><h2>No captured view</h2>
        <button className="subtle" onClick={onHome}><House />Home</button></div>}
      {session && <div id="snapshot-frame" style={size} aria-busy={updating} inert={updating}>
        {!snapshotFailed ? <img ref={image} id="snapshot" key={snapshotSrc} decoding="async"
          alt={coordinates ? 'DSS2 red survey sky preview' : 'Captured Aladin sky view'}
          src={snapshotSrc} onLoad={() => setLoadedSnapshot(snapshotSrc)}
          onError={() => setSnapshotFailed(true)}
          style={{ filter: neutral ? '' : 'url(#snapshot-tone-filter)' }} /> :
          <div className="coordinate-backdrop" data-testid="coordinate-backdrop" aria-label="RA and Dec sky grid">
            <svg viewBox={`0 0 ${w} ${h}`} preserveAspectRatio="none" aria-hidden>
              <g transform={`rotate(${angle} ${w/2} ${h/2})`}>
                {Array.from({ length: 17 }, (_, i) => <g key={i}>
                  <path d={`M${w/2+(i-8)*w/6} ${-h*2}V${h*3}M${-w*2} ${h/2+(i-8)*h/6}H${w*3}`} />
                </g>)}
              </g>
            </svg>
            <span className="coordinate-grid-label mono">
              RA {session.view.center_icrs_deg?.[0]?.toFixed(4)}° · Dec {session.view.center_icrs_deg?.[1]?.toFixed(4)}°
            </span>
          </div>}
        <svg id="footprints" data-testid="footprints" aria-label="Observation footprints" viewBox={`0 0 ${w} ${h}`}
          onClick={(event) => {
            const rect = event.currentTarget.getBoundingClientRect();
            const next = { x: (event.clientX - rect.left) / rect.width * w, y: (event.clientY - rect.top) / rect.height * h };
            onPoint(point && Math.hypot(next.x - point.x, next.y - point.y) * rect.width / w < 14 ? null : next);
          }}>
          <defs>{session.catalog.groups.flatMap((group, index) => group.observations.filter(o => available.has(o.obs_id)).map((observation, oi) => {
            const color = wavelengthColor(observation.wavelength_nm);
            return <pattern key={`${index}-${oi}`} id={`stripe-${index}-${oi}`} width="9" height="9" patternUnits="userSpaceOnUse"
              patternTransform="rotate(45)">
              <rect width="9" height="9" fill={color} opacity=".1" />
              <rect width="4" height="9" fill={color} opacity=".55" />
              {!window.matchMedia('(prefers-reduced-motion: reduce)').matches &&
                <animateTransform attributeName="patternTransform" type="translate" from="0 0" to="18 0"
                  dur="5s" repeatCount="indefinite" additive="sum" />}
            </pattern>;
          }))}</defs>
          {session.catalog.groups.flatMap((group, index) => group.observations.filter(o => available.has(o.obs_id)).map((o: Observation, oi) =>
            focusFootprints && !selected.has(o.obs_id) && hovered !== index && hovered !== o.obs_id
              && flash !== o.obs_id ? [] : (o.pixels ?? []).map((polygon, pi) => <polygon key={`${o.obs_id}-${pi}`} data-obs={o.obs_id} data-group={index}
              onDoubleClick={(event) => { event.stopPropagation(); onJump(o.obs_id); }}
              onClick={(event) => {
                if (event.altKey) { event.stopPropagation(); onJump(o.obs_id); }
              }}
              points={polygon.map((p) => p.join(',')).join(' ')}
              className={[
                group.collection === 'Survey' ? 'survey-footprint' : '',
                selected.has(o.obs_id) ? 'selected' : '', favorites.has(o.obs_id) ? 'favorite' : '',
                dismissed.has(o.obs_id) ? 'dismissed' : '', hovered === index || hovered === o.obs_id || flash === o.obs_id ? 'highlight' : '',
              ].join(' ')}
              style={{ '--wave': wavelengthColor(o.wavelength_nm), '--stripe': `url(#stripe-${index}-${oi})` } as CSSProperties}>
              <title>{o.filter} · {o.target} · Double-click or Alt+click to find image</title>
            </polygon>)))}
          <g id="point-pin">{point && <><circle cx={point.x} cy={point.y} r="9" />
            <path d={`M${point.x - 17} ${point.y}h34M${point.x} ${point.y - 17}v34`} /></>}</g>
        </svg>
        <div className="capture-frame-outline" />
        <CaptureCompass angle={angle} />
        {point && <button id="clear-pin" className="pin-clear" title="Clear point (Esc)"
          onClick={() => onPoint(null)}><X />Clear point</button>}
      </div>}
      {updating && <div className="snapshot-working" role="status">
        <span><LoaderCircle className="export-spinner" aria-hidden />Updating view…</span>
      </div>}
      <svg width="0" height="0" style={{ position: 'absolute' }} aria-hidden="true">
        <filter id="snapshot-tone-filter" colorInterpolationFilters="sRGB">
          <feComponentTransfer>{(['R', 'G', 'B'] as const).map((c) => {
            const Component = `feFunc${c}` as const;
            return <Component key={c} type="linear" slope={1 / range} intercept={-tone.black / range} />;
          })}</feComponentTransfer>
          <feComponentTransfer>{(['R', 'G', 'B'] as const).map((c) => {
            const Component = `feFunc${c}` as const;
            return <Component key={c} type="gamma" amplitude={1} exponent={tone.midtones} offset={0} />;
          })}</feComponentTransfer>
          <feColorMatrix type="saturate" values={String(tone.saturation)} />
        </filter>
      </svg>
    </div>
    {session && <RotationControl session={session.id} key={session.id}
      saved={session.view.rotation_deg ?? 0} angle={angle} onAngle={setAngle}
      busy={busy || refreshing || recapture.isPending} />}
    {toneOpen && <Popover anchor={anchor} onClose={() => setToneOpen(false)} className="tone-popover">
      <div className="popover-title">View tone</div>
      {([['black', 'Black point', 0, .9, .005], ['white', 'White point', .1, 1, .005],
        ['midtones', 'Midtones', .3, 4, .01], ['saturation', 'Saturation', 0, 2, .01]] as const)
        .map(([key, label, min, max, step]) => <Slider key={key} label={label}
          min={min} max={max} step={step} value={tone[key]} defaultValue={defaultTone[key]}
          log={key === 'midtones'} onInput={(value) => changeTone({ ...tone, [key]: value }, false)}
          onChange={(value) => changeTone({ ...tone, [key]: value })} />)}
      <div className="tone-actions"><button onClick={autoTone}><WandSparkles />Auto</button>
        <button onClick={() => changeTone(defaultTone)}><RotateCcw />Reset</button></div>
    </Popover>}
  </div>;
}
