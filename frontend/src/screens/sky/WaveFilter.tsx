import { useRef } from 'react';
import { X } from 'lucide-react';
import type { Observation } from '../../api/types';
import { IconButton } from '../../ui';
import { logPosition, wavelengthAt, waveLabel, waveStyle } from './utils';

export interface WaveRange { lo: number; hi: number }
export function WaveFilter({ observations, selected, range, onChange }: {
  observations: Observation[]; selected: Set<string>; range: WaveRange | null;
  onChange: (range: WaveRange | null) => void;
}) {
  const drag = useRef<{ fixed: number; start?: number; current: WaveRange | null } | null>(null);
  const maxNm = Math.max(25000, ...observations.map(o => o.wavelength_nm ?? 0));
  const position = (nm: number) => logPosition(nm, maxNm);
  const bins = new Map<number, { n: number; nm: number }>();
  observations.forEach((o) => {
    if (o.wavelength_nm == null) return;
    const index = Math.round(position(o.wavelength_nm) * 150);
    const bin = bins.get(index) ?? { n: 0, nm: o.wavelength_nm };
    bin.n++; bins.set(index, bin);
  });
  const peak = Math.max(1, ...[...bins.values()].map((bin) => bin.n));
  const nmAt = (element: HTMLElement, x: number) => {
    const rect = element.getBoundingClientRect();
    return wavelengthAt(Math.min(1, Math.max(0, (x - rect.left) / rect.width)), maxNm);
  };
  return <div className="wave-filter">
    <div className="wave-bands" id="wave-bands">
      {([[200, 400, 'UV', 'Ultraviolet · 200–400 nm'], [400, 700, 'Vis', 'Visible · 400–700 nm'],
        [700, 5000, 'NIR', 'Near-infrared · 0.7–5 µm'], [5000, 25000, 'MIR', 'Mid-infrared · 5–25 µm']] as const)
        .map(([lo, hi, label, title]) => <button key={label} title={title}
          className={range?.lo === lo && range.hi === hi ? 'active' : ''}
          onClick={() => onChange(range?.lo === lo && range.hi === hi ? null : { lo, hi })}>{label}</button>)}
      {maxNm > 25000 && <button title="Far-infrared · 25–1000 µm"
        className={range?.lo === 25000 && range.hi === 1000000 ? 'active' : ''}
        onClick={() => onChange(range?.lo === 25000 && range.hi === 1000000 ? null : { lo: 25000, hi: 1000000 })}>FIR</button>}
    </div>
    <div id="mini-rail" className="mini-rail" title="Drag to filter by wavelength"
      onPointerDown={(event) => {
        event.currentTarget.setPointerCapture(event.pointerId);
        const nm = nmAt(event.currentTarget, event.clientX);
        const edge = (event.target as HTMLElement).dataset.edge;
        drag.current = {
          fixed: edge && range ? edge === 'lo' ? range.hi : range.lo : nm,
          start: edge ? undefined : event.clientX, current: range,
        };
      }} onPointerMove={(event) => {
        if (!drag.current) return;
        const nm = nmAt(event.currentTarget, event.clientX);
        const next = { lo: Math.round(Math.min(nm, drag.current.fixed)), hi: Math.round(Math.max(nm, drag.current.fixed)) };
        drag.current.current = next; onChange(next);
      }} onPointerUp={(event) => {
        if (!drag.current) return;
        const state = drag.current; drag.current = null;
        onChange(state.start !== undefined && Math.abs(event.clientX - state.start) < 3 ? null : state.current);
      }} onPointerCancel={() => { drag.current = null; }}>
      <div id="wave-density">{[...bins].map(([index, bin]) =>
        <span key={index} style={{ ...waveStyle(bin.nm), left: `${index / 150 * 100}%`, height: 2 + 9 * Math.sqrt(bin.n / peak) }}
          title={`${waveLabel(bin.nm)} · ${bin.n} images`} />)}</div>
      {range && <div id="wave-brush" style={{
        left: `${position(range.lo) * 100}%`,
        width: `${(position(range.hi) - position(range.lo)) * 100}%`,
      }}><i data-edge="lo" /><i data-edge="hi" /></div>}
      <div id="wave-selected">{observations.filter((o) => selected.has(o.obs_id)).map((o) =>
        <span key={o.obs_id} className="mini-marker" style={{
          ...waveStyle(o.wavelength_nm), left: `${position(o.wavelength_nm ?? 550) * 100}%`,
        }} />)}</div>
    </div>
    <span id="wave-label" data-testid="wave-label" className="mono">{range ? `${waveLabel(range.lo)} – ${waveLabel(range.hi)}` : ''}</span>
    {range && <IconButton id="wave-clear" icon={X} title="Clear wavelength filter" className="small" onClick={() => onChange(null)} />}
  </div>;
}
