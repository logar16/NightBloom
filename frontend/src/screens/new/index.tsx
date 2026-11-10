import { useEffect, useState } from 'react';
import { ArrowRight, CloudOff, Crosshair, ExternalLink, LoaderCircle, MapPin, Search, Scan, Telescope } from 'lucide-react';
import { useMutation, useQuery } from '@tanstack/react-query';
import { api } from '../../api/client';
import { toast, useCapture, useStatus } from '../../api/hooks';
import { BookmarksPanel } from './Bookmarks';
import '../sky/sky.css';
import './new.css';

interface Position { name: string; ra: number; dec: number }
interface Coverage {
  pending?: boolean;
  counts: { HST: number; JWST: number };
  instruments: { mission: string; instrument: string; count: number }[];
  frame: { width_arcmin: number; height_arcmin: number; aspect: string; covering: number };
  warning?: string;
}
export function NewProjectScreen() {
  const { data: status } = useStatus();
  const capture = useCapture();
  const [target, setTarget] = useState('');
  const [position, setPosition] = useState<Position | null>(null);
  const [aspect, setAspect] = useState('16:9');
  const [width, setWidth] = useState(10);
  const [rotation, setRotation] = useState(0);
  const [coverageRetry, setCoverageRetry] = useState(0);
  const [bookmarkFrame, setBookmarkFrame] = useState<{ warning: string | null } | null>(null);
  const resolve = useMutation({
    mutationFn: (value: string) => api<Position>(`/coordinates/resolve?target=${encodeURIComponent(value)}`),
    onSuccess: setPosition,
  });
  const coverage = useQuery({
    queryKey: ['coordinate-coverage', position?.ra, position?.dec, aspect, coverageRetry],
    queryFn: () => api<Coverage>(`/coordinates/coverage?ra=${position!.ra}&dec=${position!.dec}&aspect=${aspect}&refresh=${coverageRetry > 0}`),
    enabled: !!position, retry: false, staleTime: 300_000,
    refetchInterval: query => query.state.data?.pending ? 1000 : false,
  });
  useEffect(() => {
    if (coverage.data && !coverage.data.pending && !bookmarkFrame) setWidth(coverage.data.frame.width_arcmin);
  }, [coverage.data, bookmarkFrame]);
  const offline = !status?.aladin;
  const checking = coverage.isFetching || coverage.data?.pending;
  const zero = coverage.data && !coverage.data.warning && !coverage.data.pending &&
    coverage.data.counts.HST + coverage.data.counts.JWST === 0;
  const valid = Number.isFinite(width) && width >= 1 && width <= 180 &&
    Number.isFinite(rotation) && rotation >= -180 && rotation <= 180;
  const capturePosition = (surveysOnly = false) => {
    if (!position || !valid) return;
    capture.mutate({ ...position, width_arcmin: width, aspect, rotation_deg: rotation, surveys_only: surveysOnly });
  };
  const ratio = aspect.split(':').map(Number).reduce((a, b) => a / b);
  return <section id="new" className="screen active" aria-label="New project">
    <div className="new-project-workspace">
      <div className="new-project-heading"><h1>New project</h1></div>
      <div className="new-project-choices">
        <article className="new-search" aria-labelledby="new-search-title">
          <div className="new-choice-heading"><Search /><h2 id="new-search-title">Search</h2></div>
          <form className="position-search" onSubmit={event => {
            event.preventDefault(); setBookmarkFrame(null); setPosition(null); resolve.mutate(target);
          }}>
            <label className="position-field"><MapPin aria-hidden />
              <input autoFocus aria-label="Target name or coordinates" placeholder="Name or coordinates"
                value={target} disabled={resolve.isPending}
                onChange={event => { setTarget(event.target.value); setBookmarkFrame(null); setPosition(null); resolve.reset(); }} />
            </label>
            <button type="submit" className="action" disabled={!target.trim() || resolve.isPending || capture.isPending}>
              {resolve.isPending ? <LoaderCircle className="export-spinner" /> : <Search />}Search
            </button>
          </form>
          {resolve.error && <div className="sky-callout" role="alert"><CloudOff /><p>{resolve.error.message}</p></div>}
          {position && <div className="position-result">
            <div className="position-identity"><Crosshair /><div><h3>{position.name}</h3>
              <span className="mono">{position.ra.toFixed(5)}° &nbsp; {position.dec.toFixed(5)}°</span>
            </div></div>
            {bookmarkFrame?.warning && <div className="sky-callout" role="alert">{bookmarkFrame.warning}</div>}
            {checking ? <div className="coverage-pending" role="status">
              <LoaderCircle className="export-spinner" />Checking HST/JWST coverage…
            </div> : coverage.data && <div className="coverage-chips" aria-label="Archive coverage">
              {(['HST', 'JWST'] as const).map(mission => <span key={mission} title={
                coverage.data!.instruments.filter(item => item.mission === mission)
                  .map(item => `${item.instrument}: ${item.count}`).join(' · ') || 'No nearby observations'}>
                <Telescope />{mission}<b>{coverage.data!.warning ? '—' : coverage.data!.counts[mission]}</b>
              </span>)}
            </div>}
            {(coverage.error || coverage.data?.warning) && <div className="sky-callout" role="alert">
              <CloudOff /><p>{coverage.error?.message || coverage.data?.warning}</p>
              <button onClick={() => setCoverageRetry(value => value + 1)}>Retry</button>
            </div>}
            <div className="frame-settings">
              <div className="frame-plate" aria-hidden>
                <div style={{ width: 76*Math.min(1, ratio), height: 76/Math.max(1, ratio),
                  transform: `rotate(${rotation}deg)` }}><Crosshair /></div>
              </div>
              <div className="frame-fields">
                <label>Width <span>arcmin</span><input type="number" aria-label="Frame width" min="1" max="180" step=".1"
                  value={Number.isNaN(width) ? '' : width} onChange={event => setWidth(event.target.valueAsNumber)} /></label>
                <label>Aspect<select aria-label="Frame aspect" value={aspect} onChange={event => setAspect(event.target.value)}>
                  {['16:9', '3:2', '1:1', '9:16'].map(value => <option key={value}>{value}</option>)}
                </select></label>
                <label title="North clockwise from up · +90° puts north right">Rotation <span>°</span>
                  <input type="number" aria-label="Frame rotation" min="-180" max="180" step=".1"
                    value={Number.isNaN(rotation) ? '' : rotation} onChange={event => setRotation(event.target.valueAsNumber)} />
                </label>
              </div>
            </div>
            {zero ? <div className="sky-callout no-coverage" role="status">
              <Telescope /><p>No HST/JWST here</p>
              <button className="action" disabled={!valid || capture.isPending} onClick={() => capturePosition(true)}>Use surveys</button>
              <span title={offline ? 'Aladin is offline' : 'Open this position in Aladin'}>
                <button disabled={offline} onClick={() => {
                  void api('/aladin/goto', 'POST', { target: `${position.ra} ${position.dec}`, fov_arcmin: width })
                    .then(() => toast('Position sent to Aladin')).catch((error: Error) => toast(error.message));
                }}><ExternalLink />Open in Aladin</button>
              </span>
            </div> : <div className="position-actions"><button className="action"
              disabled={!valid || checking || capture.isPending} onClick={() => capturePosition()}>
              <Scan />Capture<ArrowRight />
            </button></div>}
          </div>}
        </article>
        <article className="new-aladin" aria-labelledby="new-aladin-title">
          <div className="new-choice-heading"><Scan /><h2 id="new-aladin-title">Capture from Aladin</h2></div>
          <span className={`connection ${offline ? '' : 'connected'}`}><i />{offline ? 'Aladin offline' : 'Aladin connected'}</span>
          <span title={offline ? 'Open Aladin Desktop with its SAMP hub enabled' : 'Capture the current Aladin view'}>
            <button className="subtle" disabled={offline || capture.isPending} onClick={() => capture.mutate()}>
              <Scan />Capture from Aladin<ArrowRight />
            </button>
          </span>
        </article>
      </div>
      <BookmarksPanel onStart={frame => {
        setBookmarkFrame({ warning: frame.warning }); resolve.reset();
        setTarget(`${frame.ra.toFixed(6)} ${frame.dec.toFixed(6)}`);
        setPosition({ ra: frame.ra, dec: frame.dec, name: frame.name });
        setWidth(frame.width_arcmin); setAspect(frame.aspect); setRotation(0);
        document.querySelector('.new-search')?.scrollIntoView({ block: 'nearest', behavior: 'smooth' });
      }} />
    </div>
  </section>;
}
