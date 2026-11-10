import { useEffect } from 'react';
import { useStore } from 'zustand';
import { Copy, MapPin, Trash2 } from 'lucide-react';
import { api } from '../../api/client';
import { toast } from '../../api/hooks';
import { IconButton } from '../../ui';
import { createMarkerStore, markerCoordinates, markerText, type Marker, type RectMarker } from '../../editor/markers';
import { trackEditorSave } from '../../editor/lifecycle';
import type { Compositor } from '../../editor/compositor';
import type { ProjectMeta } from '../../editor/state';

export const markerStore = createMarkerStore({
  read: sessionId => api<Marker[]>('/markers', 'GET', undefined, sessionId),
  write: (markers, sessionId) => {
    const pending = api<unknown>('/markers', 'PUT', markers, sessionId).then(() => {});
    trackEditorSave(pending);
    return pending;
  },
}, {
  get: sessionId => {
    try { return Math.max(0, Math.floor(Number(localStorage.getItem(`astroimp.markers.high-water.${sessionId}`)) || 0)); }
    catch { return 0; }
  },
  set: (sessionId, n) => {
    try { localStorage.setItem(`astroimp.markers.high-water.${sessionId}`, String(n)); }
    catch { /* The store still remembers numbers when browser storage is unavailable. */ }
  },
});
export function useMarkersSession(sessionId: string, projectKey: string) {
  const error = useStore(markerStore, s => s.error);
  useEffect(() => { void markerStore.getState().load(sessionId); }, [sessionId, projectKey]);
  useEffect(() => { if (error) toast(`Markers: ${error}`); }, [error]);
}
export function MarkerOptions() {
  const markers = useStore(markerStore, s => s.items), ready = useStore(markerStore, s => s.ready);
  return <>
    <strong>Mark</strong>
    <div className="marker-list" role="list" aria-label="Markers">
      {!markers.length && <span className="muted">{ready ? 'No markers' : 'Loading markers…'}</span>}
      {markers.map(marker => <div key={marker.id} className="marker-row" role="listitem">
        <span className="mono" title={markerCoordinates(marker)}>#{marker.n} · {markerCoordinates(marker)}</span>
        <input type="text" aria-label={`Note for marker #${marker.n}`} placeholder="Note" value={marker.note || ''}
          onChange={e => markerStore.getState().note(marker.id, e.target.value)} />
        <IconButton icon={Trash2} className="small" title={`Delete marker #${marker.n}`}
          onClick={() => markerStore.getState().remove(marker.id)} />
      </div>)}
    </div>
    <button className="subtle marker-action" disabled={!markers.length}
      aria-label="Clear all markers" onClick={() => markerStore.getState().clear()}><Trash2 />Clear all</button>
    <button className="subtle marker-action" disabled={!markers.length} aria-label="Copy markers" onClick={() => {
      void markerStore.getState().flush().then(() => navigator.clipboard.writeText(markerText(markers)))
        .then(() => toast('Markers copied')).catch((e: Error) => toast(e.message));
    }}><Copy />Copy</button>
  </>;
}
export type MarkerRectangle = Pick<RectMarker, 'x1' | 'y1' | 'x2' | 'y2'>;
function corners(rect: MarkerRectangle, comp: Compositor, meta: ProjectMeta) {
  return [[rect.x1, rect.y1], [rect.x2, rect.y1], [rect.x2, rect.y2], [rect.x1, rect.y2]]
    .map(([x, y]) => comp.imageToScreen(x / meta.width, y / meta.height));
}
export function markerHover(comp: Compositor, meta: ProjectMeta, x: number, y: number) {
  const source = comp.screenToImage(x, y);
  const marker = [...markerStore.getState().items].reverse().find(marker => {
    if (marker.kind === 'rect') return source.x * meta.width >= marker.x1 && source.x * meta.width <= marker.x2
      && source.y * meta.height >= marker.y1 && source.y * meta.height <= marker.y2;
    const p = comp.imageToScreen(marker.x / meta.width, marker.y / meta.height);
    return Math.hypot(p.x - x, p.y - y) < 16 || (x >= p.x && x <= p.x + 44 && y >= p.y - 25 && y <= p.y);
  });
  return marker ? `#${marker.n} ${markerCoordinates(marker)}${marker.note ? ` · ${marker.note}` : ''}` : '';
}
export function MarkerOverlay({ compositor, meta, width, height, left, draft }: {
  compositor: Compositor | null; meta: ProjectMeta; width: number; height: number; left: number;
  draft: MarkerRectangle | null;
}) {
  const markers = useStore(markerStore, s => s.items);
  if (!compositor?.ready || !compositor.state) return null;
  const comp = compositor;
  return <svg className="marker-overlay" aria-label="Problem area markers"
    style={{ left, width, height }} viewBox={`0 0 ${width} ${height}`}>
    {markers.map(marker => {
      const p = comp.imageToScreen((marker.kind === 'point' ? marker.x : marker.x1) / meta.width,
        (marker.kind === 'point' ? marker.y : marker.y1) / meta.height);
      return <g key={marker.id} data-testid={`marker-${marker.kind}`} data-marker={marker.n}
        role="img" aria-label={`Marker #${marker.n} ${markerCoordinates(marker)}`}>
        <title>#{marker.n} {markerCoordinates(marker)}{marker.note ? ` · ${marker.note}` : ''}</title>
        {marker.kind === 'rect' ? <polygon className="marker-region"
          points={corners(marker, comp, meta).map(p => `${p.x},${p.y}`).join(' ')} />
          : <>
            <MapPin x={p.x - 9} y={p.y - 23} className="marker-pin" style={{ width: 18, height: 24 }} />
            <circle className="marker-anchor" cx={p.x} cy={p.y} r={2} />
          </>}
        <text className="marker-number" x={p.x + 10} y={p.y - 10}>#{marker.n}</text>
      </g>;
    })}
    {draft && <polygon className="marker-region marker-draft"
      points={corners(draft, comp, meta).map(p => `${p.x},${p.y}`).join(' ')} />}
  </svg>;
}
