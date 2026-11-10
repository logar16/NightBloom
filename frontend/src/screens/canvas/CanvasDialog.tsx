import { useEffect, useRef, useState } from 'react';
import { Layers3, LoaderCircle, X } from 'lucide-react';
import { api, projectKey, projectUrl, queryClient } from '../../api/client';
import { activeJob } from '../../api/shell';
import { sessionAction } from '../../api/session-actions';
import type { CanvasPlan, CanvasPreview, CanvasRect, Job, Status } from '../../api/types';
import { IconButton, Segmented } from '../../ui';
import './canvas.css';

type Shape = 'free' | 'square' | '4:5' | '3:2';
type Point = { x: number; y: number };
const ratios: Record<Shape, number> = { free: 0, square: 1, '4:5': .8, '3:2': 1.5 };
const clamp = (n: number, lo: number, hi: number) => Math.max(lo, Math.min(hi, n));

function rectangle(a: Point, b: Point, shape: Shape, width: number, height: number): CanvasRect {
  let w = Math.abs(b.x-a.x), h = Math.abs(b.y-a.y);
  const sx = b.x >= a.x ? 1 : -1, sy = b.y >= a.y ? 1 : -1;
  const ratio = ratios[shape];
  if (ratio) { w = Math.max(w, h*ratio); h = w/ratio; }
  const f = Math.min(1, (sx > 0 ? width-a.x : a.x)/Math.max(w, 1e-8),
    (sy > 0 ? height-a.y : a.y)/Math.max(h, 1e-8));
  w = Math.max(.1, w*f); h = Math.max(.1, h*f);
  return { x: sx > 0 ? a.x : a.x-w, y: sy > 0 ? a.y : a.y-h, w, h };
}

export function CanvasDialog({ onClose }: { onClose: () => void }) {
  const dialog = useRef<HTMLDialogElement>(null), svg = useRef<SVGSVGElement>(null);
  const [preview, setPreview] = useState<CanvasPreview | null>(null);
  const [region, setRegion] = useState<CanvasRect | null>(null);
  const [plan, setPlan] = useState<CanvasPlan | null>(null);
  const [shape, setShape] = useState<Shape>('free');
  const [error, setError] = useState(''), [progress, setProgress] = useState('');
  const [planning, setPlanning] = useState(false), [applying, setApplying] = useState(false);
  const jobId = useRef<string | null>(null);
  const drag = useRef<{ start: Point; region: CanvasRect; handle: string } | null>(null);
  useEffect(() => { dialog.current?.showModal(); }, []);
  useEffect(() => {
    let active = true;
    void (async () => {
      let job = await api<Job>('/build/canvas/prepare', 'POST');
      jobId.current = job.id;
      while (active) {
        queryClient.setQueryData(projectKey('job'), job);
        queryClient.setQueryData<Status>(projectKey('status'), old => old ? { ...old, job } : old);
        setProgress(job.message || job.stage);
        if (!activeJob(job)) break;
        await new Promise(resolve => setTimeout(resolve, 400));
        const next = await api<Job | null>('/job');
        if (!next || next.id !== job.id) throw Error('Canvas preview job changed.');
        job = next;
      }
      if (!active) return;
      jobId.current = null;
      if (job.state !== 'done') throw Error(job.error || 'Canvas preview cancelled.');
      const result = await api<CanvasPreview>('/build/canvas/preview');
      if (active) { setPreview(result); setRegion(result.default_region); }
    })().catch((e: Error) => { if (active) setError(e.message); });
    return () => { active = false; };
  }, []);
  useEffect(() => {
    if (!preview || !region) return;
    let active = true;
    setPlanning(true); setPlan(null); setError('');
    const timer = setTimeout(() => {
      void api<CanvasPlan>('/build/canvas/plan', 'POST', { preview_id: preview.id, region })
        .then(value => { if (active) setPlan(value); })
        .catch((e: Error) => { if (active) setError(e.message); })
        .finally(() => { if (active) setPlanning(false); });
    }, 180);
    return () => { active = false; clearTimeout(timer); };
  }, [preview, region]);
  function point(event: React.PointerEvent): Point {
    const matrix = svg.current?.getScreenCTM()?.inverse();
    const p = new DOMPoint(event.clientX, event.clientY).matrixTransform(matrix);
    return { x: clamp(p.x, 0, preview!.width), y: clamp(p.y, 0, preview!.height) };
  }
  function down(event: React.PointerEvent<SVGSVGElement>) {
    if (!preview || !region || event.button !== 0) return;
    const start = point(event);
    const handle = (event.target as SVGElement).dataset.handle || 'draw';
    drag.current = { start, region, handle };
    event.currentTarget.setPointerCapture(event.pointerId);
    if (handle === 'draw') setRegion(rectangle(start, start, shape, preview.width, preview.height));
  }
  function move(event: React.PointerEvent<SVGSVGElement>) {
    if (!drag.current || !preview) return;
    const p = point(event), d = drag.current, r = d.region;
    if (d.handle === 'move') {
      setRegion({ ...r, x: clamp(r.x+p.x-d.start.x, 0, preview.width-r.w),
        y: clamp(r.y+p.y-d.start.y, 0, preview.height-r.h) });
    } else {
      const anchor = d.handle === 'draw' ? d.start : {
        x: d.handle.includes('w') ? r.x+r.w : r.x,
        y: d.handle.includes('n') ? r.y+r.h : r.y,
      };
      setRegion(rectangle(anchor, p, shape, preview.width, preview.height));
    }
  }
  function chooseShape(next: Shape) {
    setShape(next);
    if (preview && region && next !== 'free')
      setRegion(rectangle({ x: region.x, y: region.y }, { x: region.x+region.w, y: region.y+region.h },
        next, preview.width, preview.height));
  }
  function close() {
    if (applying) return;
    const id = jobId.current;
    if (id) void api<Job | null>('/job').then(job => {
      if (job?.id === id && activeJob(job)) return api(`/job/cancel?id=${id}`, 'POST');
    }).catch(() => {});
    dialog.current?.close();
  }
  function apply() {
    if (!preview || !region || !plan) return;
    setApplying(true);
    void sessionAction<Job>('/build/canvas', 'POST', { preview_id: preview.id, region, grid: plan.grid })
      .then(job => {
        queryClient.setQueryData(projectKey('job'), job);
        queryClient.setQueryData<Status>(projectKey('status'), old => old ? { ...old, job } : old);
        dialog.current?.close();
      }).catch((e: Error) => setError(e.message)).finally(() => setApplying(false));
  }
  const detail = plan ? plan.detail_ratio > 1.005 ? `${plan.detail_ratio.toFixed(1)}× less detail`
    : plan.detail_ratio < .995 ? `${(1/plan.detail_ratio).toFixed(1)}× more detail` : 'Same detail' : '—';
  return <dialog ref={dialog} className="canvas-dialog" aria-labelledby="canvas-title" onClose={onClose}
    onCancel={event => { event.preventDefault(); close(); }}>
    <div className="dialog-bar"><h2 id="canvas-title">Canvas</h2>
      <Segmented<Shape> label="Canvas aspect" value={shape} onChange={chooseShape} options={[
        { value: 'free', label: 'Free' }, { value: 'square', label: 'Square' },
        { value: '4:5', label: '4:5' }, { value: '3:2', label: '3:2' },
      ]} /><IconButton icon={X} title="Close Canvas" disabled={applying} onClick={close} />
    </div>
    <div className="canvas-layout"><div className="canvas-mosaic">
      {!preview ? <div className="canvas-loading" role="status"><LoaderCircle /><span>{progress}</span></div>
        : <svg ref={svg} viewBox={`0 0 ${preview.width} ${preview.height}`} role="img"
          aria-label="Canvas mosaic" onPointerDown={down} onPointerMove={move}
          onPointerUp={() => { drag.current = null; }} onPointerCancel={() => { drag.current = null; }}>
          <image href={projectUrl(preview.image_url)} width={preview.width} height={preview.height} />
          {preview.footprints.map(f => <polygon key={f.key} className="canvas-footprint"
            points={f.points.map(p => p.join(',')).join(' ')} stroke={f.color} vectorEffect="non-scaling-stroke" />)}
          {preview.current_rect && <rect className="canvas-current" x={preview.current_rect.x} y={preview.current_rect.y}
            width={preview.current_rect.w} height={preview.current_rect.h} vectorEffect="non-scaling-stroke" />}
          {region && <><rect className="canvas-region" data-handle="move" x={region.x} y={region.y}
            width={region.w} height={region.h} vectorEffect="non-scaling-stroke" />
            {(['nw', 'ne', 'se', 'sw'] as const).map(handle => <circle key={handle} data-handle={handle}
              className="canvas-handle" cx={region.x+(handle.includes('e') ? region.w : 0)}
              cy={region.y+(handle.includes('s') ? region.h : 0)} r={Math.max(preview.width, preview.height)*.006}
              vectorEffect="non-scaling-stroke" />)}</>}
        </svg>}
    </div><aside className="canvas-readout">
      <dl className="mono"><dt>Size</dt><dd>{plan ? `${plan.grid.width} × ${plan.grid.height}` : '—'}</dd>
        <dt>Scale</dt><dd>{plan ? `${plan.grid.pixel_scale_arcsec.toFixed(4)}″/px` : '—'}</dd>
        <dt>Detail</dt><dd className={plan && plan.detail_ratio > 1.005 ? 'canvas-warning' : ''}>{detail}</dd></dl>
      <div className="canvas-filters">{preview?.footprints.map(f => <div key={f.key} title={f.key}>
        <i style={{ background: f.color }} /><span>{f.filter}</span>
        <span className="mono">{plan ? `${Math.round((plan.kept.find(k => k.key === f.key)?.kept_fraction || 0)*100)}% kept` : '—'}</span>
      </div>)}</div>
    </aside></div>
    <div className="canvas-footer">{error && <span role="alert" className="canvas-warning">{error}</span>}
      {planning && <LoaderCircle size={14} aria-label="Planning canvas" />}
      <button className="subtle" disabled={applying} onClick={close}>Cancel</button>
      <button className="action" disabled={!plan || planning || applying} onClick={apply}
        title="Remaps edits and saves a Before canvas change checkpoint"><Layers3 />Apply & build</button>
    </div>
  </dialog>;
}
