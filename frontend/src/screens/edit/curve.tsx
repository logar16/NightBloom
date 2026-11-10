import { useRef } from 'react';
import { RotateCcw } from 'lucide-react';
import { IconButton } from '../../ui';
import { clamp, type Layer } from '../../editor/state';
import { curveLut, curvePoints } from '../../editor/curve';
import { changeProperty } from './properties';
import { editorStore } from '../../editor/store';

export function CurveEditor({ layer }: { layer: Layer }) {
  const drag = useRef<number | null>(null), points = curvePoints(layer.curve), lut = curveLut(layer.curve);
  const update = (index: number, x: number, y: number, committed: boolean) => {
    const next = points.map(p => ({ ...p }));
    next[index] = { x: index === 0 ? 0 : index === next.length - 1 ? 1
      : clamp(x, next[index - 1].x + .005, next[index + 1].x - .005), y: clamp(y) };
    changeProperty('curve', next, committed);
  };
  const remove = (index: number) => {
    const next = points.filter((_, i) => i !== index);
    if (!index) next.unshift({ x: 0, y: 0 });
    if (index === points.length - 1) next.push({ x: 1, y: 1 });
    changeProperty('curve', next);
  };
  return <div className="curve-control">
    <div className="field"><span>Curve</span><IconButton icon={RotateCcw} title="Reset curve"
      onClick={() => changeProperty('curve', [])} /></div>
    <svg className="curve-editor" viewBox="-8 -8 176 176" role="group" aria-label="Tone curve"
      onPointerDown={e => {
        if (e.button !== 0) return;
        const index = (e.target as SVGElement).getAttribute('data-point');
        if (index !== null) drag.current = Number(index);
        else {
          const r = e.currentTarget.getBoundingClientRect();
          const x = clamp((e.clientX - r.left) / r.width * 176 / 160 - .05, .005, .995);
          if (points.some(p => Math.abs(p.x - x) < .005)) return;
          const y = clamp(1 - ((e.clientY - r.top) / r.height * 176 - 8) / 160);
          const next = [...points, { x, y }].sort((a, b) => a.x - b.x);
          drag.current = next.findIndex(p => p.x === x);
          changeProperty('curve', next, false);
        }
        e.currentTarget.setPointerCapture(e.pointerId);
      }}
      onPointerMove={e => {
        if (drag.current === null) return;
        const r = e.currentTarget.getBoundingClientRect();
        update(drag.current, ((e.clientX - r.left) / r.width * 176 - 8) / 160,
          1 - ((e.clientY - r.top) / r.height * 176 - 8) / 160, false);
      }}
      onPointerUp={() => { if (drag.current !== null) editorStore.getState().commit(); drag.current = null; }}
      onPointerCancel={() => { editorStore.getState().commit(); drag.current = null; }}
      onDoubleClick={e => {
        const index = (e.target as SVGElement).getAttribute('data-point');
        if (index === null) changeProperty('curve', []); else remove(Number(index));
      }}>
      <path className="curve-grid" d="M0 0H160V160H0Z M40 0V160 M80 0V160 M120 0V160 M0 40H160 M0 80H160 M0 120H160" />
      <path className="curve-identity" d="M0 160L160 0" />
      <path className="curve-line" d={Array.from(lut, (y, i) => `${i ? 'L' : 'M'}${i / 255 * 160},${(1 - y) * 160}`).join(' ')} />
      {points.map((p, i) => <circle key={i} cx={p.x * 160} cy={(1 - p.y) * 160} r="4" data-point={i}
        tabIndex={0} role="slider" aria-label={`Curve point ${i + 1}`} aria-valuemin={0} aria-valuemax={1}
        aria-valuenow={p.y} aria-valuetext={`Input ${p.x.toFixed(2)}, output ${p.y.toFixed(2)}`}
        onContextMenu={e => { e.preventDefault(); remove(i); }}
        onKeyDown={e => {
          const step = e.shiftKey ? .05 : .01;
          if (e.key.startsWith('Arrow')) {
            e.preventDefault();
            update(i, p.x + (e.key === 'ArrowRight' ? step : e.key === 'ArrowLeft' ? -step : 0),
              p.y + (e.key === 'ArrowUp' ? step : e.key === 'ArrowDown' ? -step : 0), true);
          } else if (e.key === 'Delete' || e.key === 'Backspace') { e.preventDefault(); remove(i); }
        }} />)}
    </svg>
  </div>;
}
