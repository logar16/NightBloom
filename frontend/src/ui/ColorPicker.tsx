import { useRef, useState } from 'react';
import type { RefObject } from 'react';
import { Pipette, X } from 'lucide-react';
import { clamp, IconButton, Popover } from './common';
import type { ValueProps } from './common';

export function normalizeHex(value: string) {
  if (/^#[a-f0-9]{6}$/i.test(value)) return value.toLowerCase();
  if (/^#[a-f0-9]{3}$/i.test(value)) return `#${[...value.slice(1)].map((c) => c + c).join('')}`;
  return null;
}
function toHSV(hex: string): [number, number, number] {
  const [r, g, b] = [1, 3, 5].map((i) => parseInt(hex.slice(i, i + 2), 16) / 255);
  const max = Math.max(r, g, b), min = Math.min(r, g, b), d = max - min;
  const h = !d ? 0 : max === r ? ((g - b) / d + 6) % 6 : max === g ? (b - r) / d + 2 : (r - g) / d + 4;
  return [h / 6, max ? d / max : 0, max];
}
function fromHSV(h: number, s: number, v: number) {
  return '#' + [5, 3, 1].map((n) => {
    const k = (n + h * 6) % 6;
    return Math.round((v - v * s * Math.max(0, Math.min(k, 4 - k, 1))) * 255)
      .toString(16).padStart(2, '0');
  }).join('');
}
function recentColors(): string[] {
  try { return (JSON.parse(localStorage.getItem('astroimp-colors') ?? '[]') as string[]).filter(normalizeHex).slice(0, 8); }
  catch { return []; }
}
export interface ColorPickerProps extends ValueProps<string> {
  label?: string; presets?: string[];
  anchor?: RefObject<HTMLElement | null>; initiallyOpen?: boolean; onClose?: () => void;
}
export function ColorPicker({
  value, label = 'Color', presets = [], onInput, onChange,
  anchor: externalAnchor, initiallyOpen = false, onClose,
}: ColorPickerProps) {
  const anchor = useRef<HTMLButtonElement>(null);
  const [open, setOpen] = useState(initiallyOpen);
  const [text, setText] = useState(value);
  const [hsv, setHSV] = useState(() => toHSV(value));
  const latest = useRef(value);
  latest.current = value;
  const [recent, setRecent] = useState(recentColors);
  const commit = (color = latest.current) => {
    const colors = [color, ...recentColors().filter((c) => c !== color)].slice(0, 8);
    try { localStorage.setItem('astroimp-colors', JSON.stringify(colors)); } catch { /* Storage is optional. */ }
    setRecent(colors); onChange?.(color);
  };
  const update = (color: string) => {
    latest.current = color; setText(color); onInput?.(color);
  };
  const dragColor = (element: HTMLElement, x: number, y: number, hue = false) => {
    const rect = element.getBoundingClientRect();
    const next: [number, number, number] = hue
      ? [clamp((x - rect.left) / rect.width), hsv[1], hsv[2]]
      : [hsv[0], clamp((x - rect.left) / rect.width), 1 - clamp((y - rect.top) / rect.height)];
    setHSV(next); update(fromHSV(...next));
  };
  const close = () => { commit(); setOpen(false); onClose?.(); };
  return <div className="ui-control ui-color-picker">
    <button ref={anchor} type="button" aria-label={label} aria-expanded={open} className="ui-color-trigger"
      hidden={!!externalAnchor}
      onClick={() => {
        if (open) close();
        else { setHSV(toHSV(value)); setText(value); setOpen(true); }
      }}><span className="ui-color-chip" style={{ background: value }} />
      <span className="ui-color-value">{value}</span></button>
    {open && <Popover anchor={externalAnchor ?? anchor} onClose={close} className="ui-color-panel">
      <div role="dialog" aria-label={`${label} color`}>
        <div className="ui-color-header"><span className="ui-label">{label}</span>
          <IconButton icon={X} title="Close color picker" onClick={close} /></div>
        <div className="ui-color-sv" tabIndex={0} role="slider" aria-label="Saturation and brightness"
          aria-valuenow={Math.round(hsv[1] * 100)} aria-valuemin={0} aria-valuemax={100}
          style={{ backgroundColor: fromHSV(hsv[0], 1, 1) }}
          onPointerDown={(event) => {
            event.currentTarget.setPointerCapture(event.pointerId);
            dragColor(event.currentTarget, event.clientX, event.clientY);
          }} onPointerMove={(event) => {
            if (event.currentTarget.hasPointerCapture(event.pointerId))
              dragColor(event.currentTarget, event.clientX, event.clientY);
          }} onPointerUp={() => commit()} onKeyDown={(event) => {
            if (!event.key.startsWith('Arrow')) return;
            event.preventDefault();
            const next: [number, number, number] = [
              hsv[0], clamp(hsv[1] + (event.key === 'ArrowRight' ? .01 : event.key === 'ArrowLeft' ? -.01 : 0)),
              clamp(hsv[2] + (event.key === 'ArrowUp' ? .01 : event.key === 'ArrowDown' ? -.01 : 0)),
            ];
            setHSV(next); update(fromHSV(...next)); commit();
          }}>
          <span className="ui-color-cursor" style={{ left: `${hsv[1] * 100}%`, top: `${(1 - hsv[2]) * 100}%` }} />
        </div>
        <div className="ui-color-hue" tabIndex={0} role="slider" aria-label="Hue"
          aria-valuemin={0} aria-valuemax={360} aria-valuenow={Math.round(hsv[0] * 360)}
          onPointerDown={(event) => {
            event.currentTarget.setPointerCapture(event.pointerId);
            dragColor(event.currentTarget, event.clientX, event.clientY, true);
          }} onPointerMove={(event) => {
            if (event.currentTarget.hasPointerCapture(event.pointerId))
              dragColor(event.currentTarget, event.clientX, event.clientY, true);
          }} onPointerUp={() => commit()} onKeyDown={(event) => {
            if (!event.key.startsWith('Arrow')) return;
            event.preventDefault();
            const next: [number, number, number] = [
              clamp(hsv[0] + (['ArrowRight', 'ArrowUp'].includes(event.key) ? 1 : -1) / 360), hsv[1], hsv[2],
            ];
            setHSV(next); update(fromHSV(...next)); commit();
          }}><span className="ui-hue-cursor" style={{ left: `${hsv[0] * 100}%` }} /></div>
        <div className="ui-color-fields"><span className="ui-color-preview" style={{ background: value }} />
          <input className="ui-color-hex" aria-label="Hex color" value={text}
            aria-invalid={!normalizeHex(text)} onChange={(event) => {
              setText(event.target.value);
              const color = normalizeHex(event.target.value);
              if (color) { update(color); setHSV(toHSV(color)); }
            }} onBlur={() => commit()} onKeyDown={(event) => { if (event.key === 'Enter') commit(); }} />
        </div>
        {[presets, recent].map((colors, index) => <div className="ui-color-swatches" key={index}>
          {colors.map((color) => <button key={color} type="button" className="ui-color-preset"
            aria-label={`Use ${color}`} title={color} style={{ background: color }}
            onClick={() => { update(color); setHSV(toHSV(color)); commit(color); }} />)}
        </div>)}
        {'EyeDropper' in window && <button className="ui-eyedropper" onClick={() => {
          const api = window as unknown as {
            EyeDropper: new () => { open: () => Promise<{ sRGBHex: string }> };
          };
          void new api.EyeDropper().open().then(({ sRGBHex }) => {
            update(sRGBHex); setHSV(toHSV(sRGBHex)); commit(sRGBHex);
          }).catch(() => undefined);
        }}><Pipette size={13} />Pick from screen</button>}
      </div>
    </Popover>}
  </div>;
}
