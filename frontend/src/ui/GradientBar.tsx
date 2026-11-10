import { useRef, useState } from 'react';
import type { CSSProperties } from 'react';
import { ColorPicker } from './ColorPicker';
import { clamp } from './common';

export interface GradientStop { t: number; color: string }
export interface GradientAxis {
  min: number; max: number; log?: boolean; format?: (value: number) => string;
}
export interface GradientMarker { value: number; color: string; label: string; title?: string }
export interface GradientBarProps {
  stops: GradientStop[];
  onInput?: (stops: GradientStop[]) => void;
  onChange?: (stops: GradientStop[]) => void;
  axis?: GradientAxis; markers?: GradientMarker[]; height?: number;
}
export function GradientBar({ stops, axis, markers = [], height = 18, onInput, onChange }: GradientBarProps) {
  const bar = useRef<HTMLDivElement>(null);
  const selectedAnchor = useRef<HTMLButtonElement>(null);
  const latest = useRef(stops);
  latest.current = stops;
  const [selected, setSelected] = useState(-1);
  const [picker, setPicker] = useState(false);
  const drag = useRef<{ index: number; x: number; y: number; moved: boolean } | null>(null);
  const [removing, setRemoving] = useState(-1);
  const min = axis?.min ?? 0, max = axis?.max ?? 1;
  const ratio = (t: number) => axis?.log ? Math.log(t / min) / Math.log(max / min) : (t - min) / (max - min);
  const from = (t: number) => axis?.log ? min * (max / min) ** t : min + t * (max - min);
  const position = (x: number) => {
    const rect = bar.current!.getBoundingClientRect();
    return from(clamp((x - rect.left) / rect.width));
  };
  const update = (next: GradientStop[], commit = false) => {
    latest.current = next; onInput?.(next); if (commit) onChange?.(next);
  };
  const remove = (index: number) => {
    if (latest.current.length <= 2) return;
    update(latest.current.filter((_, i) => i !== index), true); setSelected(-1);
  };
  const sorted = [...stops].sort((a, b) => a.t - b.t);
  const interpolate = (t: number) => {
    const right = sorted.findIndex((stop) => stop.t >= t);
    if (right <= 0) return sorted[right === -1 ? sorted.length - 1 : 0].color;
    const a = sorted[right - 1], b = sorted[right], fraction = (ratio(t) - ratio(a.t)) / (ratio(b.t) - ratio(a.t));
    return '#' + [1, 3, 5].map((i) => Math.round(
      parseInt(a.color.slice(i, i + 2), 16) * (1 - fraction) +
      parseInt(b.color.slice(i, i + 2), 16) * fraction,
    ).toString(16).padStart(2, '0')).join('');
  };
  return <div className="ui-control ui-gradient">
    <div className="ui-gradient-markers">{markers.map((marker, index) =>
      <span key={index} className="ui-gradient-marker" tabIndex={0}
        title={marker.title ?? marker.label} style={{ left: `${ratio(marker.value) * 100}%`, color: marker.color }}>
        <span className="ui-gradient-marker-label">{marker.label}</span>
      </span>)}</div>
    <div ref={bar} className="ui-gradient-bar" tabIndex={0} role="button" aria-label="Add gradient stop"
      style={{ height, background: `linear-gradient(90deg,${sorted.map((s) => `${s.color} ${ratio(s.t) * 100}%`).join(',')})` }}
      onClick={(event) => {
        const t = position(event.clientX);
        update([...stops, { t, color: interpolate(t) }], true); setSelected(stops.length);
      }} onKeyDown={(event) => {
        if (event.key === 'Enter' || event.key === ' ') {
          event.preventDefault();
          const t = from(.5); update([...stops, { t, color: interpolate(t) }], true);
        }
      }} />
    <div className="ui-gradient-handles">
      {stops.map((stop, index) => <button key={index} type="button"
        ref={selected === index ? selectedAnchor : undefined}
        className={`ui-gradient-stop ${selected === index ? 'ui-selected' : ''} ${removing === index ? 'ui-removing' : ''}`}
        aria-label={`Gradient stop ${index + 1}`} title={`${axis?.format?.(stop.t) ?? stop.t}`}
        style={{ left: `${ratio(stop.t) * 100}%`, '--ui-stop-color': stop.color } as CSSProperties}
        onPointerDown={(event) => {
          event.preventDefault(); event.currentTarget.focus();
          event.currentTarget.setPointerCapture(event.pointerId); setSelected(index);
          drag.current = { index, x: event.clientX, y: event.clientY, moved: false };
        }} onPointerMove={(event) => {
          if (!drag.current) return;
          if (Math.hypot(event.clientX - drag.current.x, event.clientY - drag.current.y) > 3) drag.current.moved = true;
          if (!drag.current.moved) return;
          setRemoving(event.clientY - drag.current.y >= 30 && stops.length > 2 ? index : -1);
          update(latest.current.map((s, i) => i === index ? { ...s, t: position(event.clientX) } : s));
        }} onPointerUp={(event) => {
          const state = drag.current;
          if (!state) return;
          drag.current = null; setRemoving(-1);
          if (event.clientY - state.y >= 30 && stops.length > 2) remove(index);
          else if (state.moved) onChange?.(latest.current);
          else { setPicker(true); }
        }} onKeyDown={(event) => {
          if (event.key === 'Delete' || event.key === 'Backspace') {
            event.preventDefault(); remove(index);
          } else if (['ArrowLeft', 'ArrowRight'].includes(event.key)) {
            event.preventDefault();
            update(stops.map((s, i) => i === index ? {
              ...s, t: from(clamp(ratio(s.t) + (event.key === 'ArrowLeft' ? -1 : 1) * (event.shiftKey ? .001 : .01))),
            } : s), true);
          } else if (event.key === 'Enter') { event.preventDefault(); setPicker(true); }
        }} />)}
    </div>
    {axis && <div className="ui-gradient-axis">{[0, .25, .5, .75, 1].map((t) =>
      <span key={t} className="ui-gradient-axis-tick" style={{ left: `${t * 100}%` }}>
        {axis.format?.(from(t)) ?? from(t).toPrecision(3)}
      </span>)}</div>}
    {picker && stops[selected] &&
      <ColorPicker value={stops[selected].color} label="Stop color"
        anchor={selectedAnchor} initiallyOpen onClose={() => setPicker(false)}
        onInput={(color) => update(stops.map((stop, i) => i === selected ? { ...stop, color } : stop))}
        onChange={(color) => update(latest.current.map((stop, i) => i === selected ? { ...stop, color } : stop), true)} />}
  </div>;
}
