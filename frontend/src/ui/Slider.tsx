import { useEffect, useRef, useState } from 'react';
import type { ValueProps } from './common';
import { clamp } from './common';

export interface SliderProps extends ValueProps<number> {
  label: string; min?: number; max?: number; step?: number; defaultValue?: number;
  log?: boolean; format?: (value: number) => string; unit?: string;
}
export function Slider({
  label, value, min = 0, max = 1, step = .01, defaultValue = value,
  log = false, format = (v) => Number(v.toPrecision(5)).toString(), unit = '', onInput, onChange,
}: SliderProps) {
  const current = useRef(value);
  const start = useRef<{ x: number; ratio: number; before: number } | null>(null);
  const keyStart = useRef<number | null>(null);
  const track = useRef<HTMLDivElement>(null);
  const [editing, setEditing] = useState(false);
  const editingRef = useRef(false);
  const [text, setText] = useState('');
  const [dragging, setDragging] = useState(false);
  current.current = value;
  const ratio = (v: number) => log && min > 0
    ? Math.log(v / min) / Math.log(max / min) : (v - min) / (max - min || 1);
  const fromRatio = (t: number) => log && min > 0 ? min * (max / min) ** t : min + t * (max - min);
  const normalize = (v: number) => clamp(Number((step > 0
    ? min + Math.round((v - min) / step) * step : v).toPrecision(12)), min, max);
  const input = (v: number) => {
    const next = normalize(v);
    if (!Number.isFinite(next)) return;
    current.current = next; onInput?.(next);
  };
  const commit = () => onChange?.(current.current);
  const textCommit = (cancel = false) => {
    if (!editingRef.current) return;
    editingRef.current = false;
    setEditing(false);
    if (!cancel && text.trim() && Number.isFinite(Number(text))) {
      input(Number(text)); commit();
    }
  };
  const wheelActions = useRef({ input, commit, step });
  wheelActions.current = { input, commit, step };
  useEffect(() => {
    const element = track.current;
    const wheel = (event: WheelEvent) => {
      if (document.activeElement !== element) return;
      event.preventDefault();
      const action = wheelActions.current;
      action.input(current.current + (event.deltaY < 0 ? action.step : -action.step));
      action.commit();
    };
    element?.addEventListener('wheel', wheel, { passive: false });
    return () => element?.removeEventListener('wheel', wheel);
  }, []);
  const percent = `${clamp(ratio(value)) * 100}%`;
  return <div className="ui-control ui-slider">
    <div className="ui-slider-heading">
      <label className="ui-label">{label}</label>
      <input className="ui-slider-value" type="text" inputMode="decimal" aria-label={`${label} value`}
        value={editing ? text : format(value)} onFocus={(event) => {
          setText(String(value)); editingRef.current = true; setEditing(true); event.currentTarget.select();
        }} onChange={(event) => setText(event.target.value)} onBlur={() => textCommit()}
        onKeyDown={(event) => {
          if (!['Enter', 'Escape'].includes(event.key)) return;
          event.preventDefault(); textCommit(event.key === 'Escape'); track.current?.focus();
        }} />
      <span className="ui-unit">{unit}</span>
    </div>
    <div ref={track} className={`ui-slider-track ${dragging ? 'ui-dragging' : ''}`}
      role="slider" tabIndex={0} aria-label={label} aria-valuemin={min} aria-valuemax={max}
      aria-valuenow={value} aria-valuetext={`${format(value)}${unit ? ` ${unit}` : ''}`}
      onPointerDown={(event) => {
        if (event.button !== 0) return;
        event.preventDefault(); event.currentTarget.focus();
        event.currentTarget.setPointerCapture(event.pointerId);
        const rect = event.currentTarget.getBoundingClientRect();
        const t = event.shiftKey ? ratio(value) : clamp((event.clientX - rect.left) / rect.width);
        start.current = { x: event.clientX, ratio: t, before: value };
        setDragging(true); if (!event.shiftKey) input(fromRatio(t));
      }} onPointerMove={(event) => {
        if (!start.current) return;
        const rect = event.currentTarget.getBoundingClientRect();
        const t = clamp(start.current.ratio +
          (event.clientX - start.current.x) / rect.width * (event.shiftKey ? .1 : 1));
        start.current.x = event.clientX; start.current.ratio = t; input(fromRatio(t));
      }} onPointerUp={() => {
        if (!start.current) return;
        start.current = null; setDragging(false); commit();
      }} onPointerCancel={() => {
        if (start.current) input(start.current.before);
        start.current = null; setDragging(false);
      }} onDoubleClick={() => { input(defaultValue); commit(); }}
      onKeyDown={(event) => {
        const delta: Record<string, number> = {
          ArrowLeft: -1, ArrowDown: -1, ArrowRight: 1, ArrowUp: 1, PageDown: -10, PageUp: 10,
        };
        if (!(event.key in delta) && !['Home', 'End'].includes(event.key)) return;
        event.preventDefault(); keyStart.current ??= value;
        input(event.key === 'Home' ? min : event.key === 'End' ? max : current.current + delta[event.key] * step);
      }} onKeyUp={() => { if (keyStart.current !== null) commit(); keyStart.current = null; }}
      onBlur={() => { if (keyStart.current !== null) commit(); keyStart.current = null; }}>
      <div className="ui-slider-line"><div className="ui-slider-fill" style={{ width: percent }} /></div>
      {[0, 25, 50, 75, 100].map((tick) => <span key={tick} className="ui-slider-tick" style={{ left: `${tick}%` }} />)}
      <span className="ui-slider-thumb" style={{ left: percent }} />
    </div>
  </div>;
}
