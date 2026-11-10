import { useRef, type PointerEvent } from 'react';
import { Compositor } from '../../editor/compositor';
import { clamp, type Layer, type LayerClip } from '../../editor/state';
import { editorStore } from '../../editor/store';

type Handle = 'draw' | 'move' | 'nw' | 'n' | 'ne' | 'e' | 'se' | 's' | 'sw' | 'w';
const handles: [Handle, string, number, number][] = [
  ['nw', 'top left', 0, 0], ['n', 'top', .5, 0], ['ne', 'top right', 1, 0], ['e', 'right', 1, .5],
  ['se', 'bottom right', 1, 1], ['s', 'bottom', .5, 1], ['sw', 'bottom left', 0, 1], ['w', 'left', 0, .5],
];
function resized(clip: LayerClip, handle: Handle, dx: number, dy: number): LayerClip {
  if (handle === 'move') return { ...clip, x: clamp(clip.x + dx, 0, 1 - clip.w), y: clamp(clip.y + dy, 0, 1 - clip.h) };
  let x = clip.x, y = clip.y, right = x + clip.w, bottom = y + clip.h;
  if (handle.includes('w')) x = clamp(x + dx, 0, right - .01);
  if (handle.includes('e')) right = clamp(right + dx, x + .01, 1);
  if (handle.includes('n')) y = clamp(y + dy, 0, bottom - .01);
  if (handle.includes('s')) bottom = clamp(bottom + dy, y + .01, 1);
  return { ...clip, x, y, w: right - x, h: bottom - y };
}
export function LayerClipOverlay({ compositor: comp, layer, width, height, left, editing }: {
  compositor: Compositor; layer: Layer; width: number; height: number; left: number; editing: boolean;
}) {
  const drag = useRef<{ handle: Handle; start: { x: number; y: number }; initial: LayerClip | null } | null>(null);
  const change = (clip: LayerClip | null, committed: boolean) => editorStore.getState().change(s => {
    const target = s.layers.find(l => l.id === layer.id); if (target) target.clip = clip;
  }, committed);
  const point = (e: PointerEvent<SVGSVGElement>) => {
    const bounds = e.currentTarget.getBoundingClientRect();
    return comp.screenToImage(e.clientX - bounds.left, e.clientY - bounds.top);
  };
  const keyMove = (e: React.KeyboardEvent, handle: Handle) => {
    if (!layer.clip) return;
    if (e.key === 'Delete' || e.key === 'Backspace') { e.preventDefault(); change(null, true); return; }
    if (!e.key.startsWith('Arrow')) return;
    e.preventDefault();
    const step = e.shiftKey ? .01 : .001;
    change(resized(layer.clip, handle,
      e.key === 'ArrowLeft' ? -step : e.key === 'ArrowRight' ? step : 0,
      e.key === 'ArrowUp' ? -step : e.key === 'ArrowDown' ? step : 0), true);
  };
  const clip = layer.clip, origin = comp.imageToScreen(0, 0), a = comp.imageToScreen(1, 0), b = comp.imageToScreen(0, 1);
  const matrix = `matrix(${a.x-origin.x} ${a.y-origin.y} ${b.x-origin.x} ${b.y-origin.y} ${origin.x} ${origin.y})`;
  const inset = clip ? clip.feather * Math.min(clip.w * comp.meta.width, clip.h * comp.meta.height) : 0;
  return <svg className={`layer-clip-overlay ${editing ? 'editing' : ''}`} role="group" aria-label="Layer crop overlay"
    style={{ left, width, height }} viewBox={`0 0 ${width} ${height}`}
    onPointerDown={e => {
      if (!editing || e.button !== 0) return;
      e.preventDefault(); e.currentTarget.setPointerCapture(e.pointerId);
      const handle = ((e.target as SVGElement).getAttribute('data-handle') || 'draw') as Handle;
      drag.current = { handle, start: point(e), initial: clip ? { ...clip } : null };
    }}
    onPointerMove={e => {
      const d = drag.current; if (!d) return;
      const p = point(e), dx = p.x - d.start.x, dy = p.y - d.start.y;
      if (d.handle !== 'draw' && d.initial) change(resized(d.initial, d.handle, dx, dy), false);
      else {
        const x = clamp(Math.min(d.start.x, p.x), 0, .99), y = clamp(Math.min(d.start.y, p.y), 0, .99);
        change({ x, y, w: clamp(Math.max(d.start.x, p.x) - x, .01, 1 - x),
          h: clamp(Math.max(d.start.y, p.y) - y, .01, 1 - y),
          shape: d.initial?.shape ?? 'rect', feather: d.initial?.feather ?? .1 }, false);
      }
    }}
    onPointerUp={() => { if (drag.current) editorStore.getState().commit(); drag.current = null; }}
    onPointerCancel={() => { if (drag.current) change(drag.current.initial, false); drag.current = null; }}>
    {editing && <rect className="layer-clip-draw" width={width} height={height} />}
    {clip && <>
      <g transform={matrix}>
        {clip.shape === 'ellipse'
          ? <ellipse className="layer-clip-boundary" data-handle="move" cx={clip.x + clip.w/2} cy={clip.y + clip.h/2}
            rx={clip.w/2} ry={clip.h/2} tabIndex={editing ? 0 : -1} role="button" aria-label="Move layer crop"
            onKeyDown={e => keyMove(e, 'move')} />
          : <rect className="layer-clip-boundary" data-handle="move" x={clip.x} y={clip.y} width={clip.w} height={clip.h}
            tabIndex={editing ? 0 : -1} role="button" aria-label="Move layer crop" onKeyDown={e => keyMove(e, 'move')} />}
        {inset > 0 && (clip.shape === 'ellipse'
          ? <ellipse className="layer-clip-feather" cx={clip.x + clip.w/2} cy={clip.y + clip.h/2}
            rx={clip.w/2 * (1-2*clip.feather)} ry={clip.h/2 * (1-2*clip.feather)} />
          : <rect className="layer-clip-feather" x={clip.x + inset/comp.meta.width} y={clip.y + inset/comp.meta.height}
            width={Math.max(0, clip.w-2*inset/comp.meta.width)} height={Math.max(0, clip.h-2*inset/comp.meta.height)} />)}
      </g>
      {editing && handles.map(([handle, label, x, y]) => {
        const p = comp.imageToScreen(clip.x + clip.w*x, clip.y + clip.h*y);
        return <rect key={handle} className="layer-clip-handle" data-handle={handle} x={p.x - 4} y={p.y - 4}
          width={8} height={8} tabIndex={0} role="button" aria-label={`Resize layer crop ${label}`}
          onKeyDown={e => keyMove(e, handle)} />;
      })}
    </>}
  </svg>;
}
