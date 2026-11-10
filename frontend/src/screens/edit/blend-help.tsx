import { useEffect, useRef, useState } from 'react';
import { CircleHelp, X } from 'lucide-react';
import { IconButton, Popover, Segmented, Select, Slider } from '../../ui';
import { BLENDS, clamp, type Blend, type Layer } from '../../editor/state';
import { blendPair, blendPixel, layerRamp, similarLayerHues } from '../../editor/blend';
import { useEditor } from '../../editor/store';
import { layerSwatch } from './panels';
import './blend-help.css';

const names: Record<Blend, string> = {
  normal: 'Normal', add: 'Add', screen: 'Screen', multiply: 'Multiply', overlay: 'Overlay',
  softlight: 'Soft light', lighten: 'Lighten', difference: 'Difference', subtract: 'Subtract', darken: 'Darken',
};
const descriptions: Record<Blend, string> = {
  normal: 'Brightness sets coverage; bright areas cover below.',
  add: 'Sums light; can clip to white.',
  screen: 'Combines light; never darker.',
  multiply: 'Darkens; keeps light both layers share.',
  overlay: 'Darkens shadows, brightens highlights of the base.',
  softlight: 'Gently shades or lights the base.',
  lighten: 'Keeps the brighter of each colour channel.',
  difference: 'Highlights where colour channels differ.',
  subtract: 'Removes top light from the base; stops at black.',
  darken: 'Keeps the darker of each colour channel.',
};

const contrastTop = '#ff944d', contrastBase = '#4dc9ff';

function BlendCanvas({ mode, top, base, opacity, size, useIntensity, contrast }: {
  mode: Blend; top?: Layer; base?: Layer; opacity: number; size: number; useIntensity: boolean; contrast: boolean;
}) {
  const ref = useRef<HTMLCanvasElement>(null);
  useEffect(() => {
    const context = ref.current?.getContext('2d');
    if (!context) return;
    const image = context.createImageData(size, size);
    const example = (layer: Layer | undefined, color: string) => contrast
      ? { mode: 'tint' as const, color, intensity: layer?.intensity ?? 1, gradient: [], spectral: { stops: [] } } : layer;
    const topRamp = layerRamp(example(top, contrastTop), useIntensity);
    const baseRamp = layerRamp(example(base, contrastBase), useIntensity);
    const surround = [49 / 255, 52 / 255, 57 / 255];
    for (let y = 0; y < size; y++) for (let x = 0; x < size; x++) {
      const u = (x + .5) / size, v = (y + .5) / size;
      const square = u >= .08 && u <= .7 && v >= .12 && v <= .74;
      const circle = Math.hypot(u - .63, v - .61) <= .31;
      let color = square ? baseRamp(clamp((u - .08) / .62)).color.map(c => c * (useIntensity ? base?.opacity ?? 1 : 1)) : surround;
      if (circle) {
        const sample = topRamp(clamp((v - .3) / .62));
        color = blendPixel(mode, color, sample.color, opacity, sample.hue, sample.bright);
      }
      const offset = (y * size + x) * 4;
      color.forEach((c, i) => { image.data[offset + i] = Math.round(clamp(c) * 255); });
      image.data[offset + 3] = 255;
    }
    context.putImageData(image, 0, 0);
    context.strokeStyle = '#b8bdc780';
    context.lineWidth = 1;
    context.strokeRect(size * .08, size * .12, size * .62, size * .62);
    context.beginPath(); context.arc(size * .63, size * .61, size * .31, 0, Math.PI * 2); context.stroke();
  }, [mode, top, base, opacity, size, useIntensity, contrast]);
  return <canvas ref={ref} width={size} height={size} aria-label={`${names[mode]} blend example`} />;
}

function BlendGuide({ current, onApply, onClose }: { current: Blend | ''; onApply: (mode: Blend) => void; onClose: () => void }) {
  const layers = useEditor(s => s.state!.layers);
  const selected = useEditor(s => s.selected);
  const [initialTop, initialBase] = blendPair(layers, selected);
  const [topId, setTop] = useState(initialTop?.id ?? '');
  const [baseId, setBase] = useState(initialBase?.id ?? '');
  const top = layers.find(l => l.id === topId), base = layers.find(l => l.id === baseId);
  const similar = similarLayerHues(top, base);
  const [pairOverride, setPair] = useState<'layers' | 'contrast'>();
  const pair = pairOverride ?? (similar ? 'contrast' : 'layers');
  const contrast = pair === 'contrast';
  const [useIntensity, setUseIntensity] = useState(false);
  const [active, setActive] = useState<Blend>(current || 'screen');
  const [opacity, setOpacity] = useState(1);
  const tiles = useRef<(HTMLButtonElement | null)[]>([]);
  useEffect(() => { tiles.current[BLENDS.indexOf(current || 'screen')]?.focus(); }, []); // Focus once when opened.
  const options = (exclude: string) => layers.filter(l => l.id !== exclude).map(l => ({
    value: l.id, label: l.name, swatch: layerSwatch(l),
  }));
  return <div role="dialog" aria-label="How blend modes work" className="blend-guide">
    <header><div><strong>Blend modes</strong></div>
      <IconButton icon={X} title="Close blend help" onClick={onClose} /></header>
    <div className="blend-pair">
      <label><span>● Top</span><Select label="Example top layer" value={topId} options={options(baseId)}
        onChange={id => { setTop(id); setPair(undefined); setOpacity(useIntensity ? layers.find(l => l.id === id)?.opacity ?? 1 : 1); }} /></label>
      <label><span>■ Base</span><Select label="Example base layer" value={baseId}
        options={[{ value: '', label: 'No layer below', swatch: '#808080' }, ...options(topId)]}
        onChange={id => { setBase(id); setPair(undefined); }} /></label>
    </div>
    <div className="blend-demo-controls">
      <Segmented label="Example colours" value={pair} options={[
        { value: 'layers', label: 'Layers' }, { value: 'contrast', label: 'Contrast' },
      ]} onChange={setPair} />
      <label><input type="checkbox" checked={useIntensity} onChange={event => {
        setUseIntensity(event.target.checked); setOpacity(event.target.checked ? top?.opacity ?? 1 : 1);
      }} />Use layer intensity</label>
    </div>
    <div className="blend-demo-note">
      <span>{similar && contrast ? 'Similar colours — showing contrasting pair' : contrast ? 'Contrasting pair' : 'Layer colours'}</span>
      {contrast && <span className="blend-demo-key">
        <i style={{ background: contrastTop }} />● Top<i style={{ background: contrastBase }} />■ Base
      </span>}
    </div>
    <div className="blend-grid" aria-label="Blend modes">
      {BLENDS.map((mode, index) => <button key={mode} ref={el => { tiles.current[index] = el; }} type="button"
        aria-label={`Apply ${names[mode]}`} aria-pressed={mode === current} tabIndex={mode === active ? 0 : -1}
        className={`blend-tile ${active === mode ? 'previewing' : ''}`}
        onMouseEnter={() => setActive(mode)} onFocus={() => setActive(mode)} onClick={() => onApply(mode)}
        onKeyDown={event => {
          const delta: Record<string, number> = { ArrowRight: 1, ArrowLeft: -1, ArrowDown: 5, ArrowUp: -5 };
          if (!(event.key in delta) && !['Home', 'End'].includes(event.key)) return;
          event.preventDefault();
          const next = event.key === 'Home' ? 0 : event.key === 'End' ? 9 : (index + delta[event.key] + 10) % 10;
          tiles.current[next]?.focus();
        }}>
        <BlendCanvas mode={mode} top={top} base={base} opacity={useIntensity ? top?.opacity ?? 1 : 1}
          size={104} useIntensity={useIntensity} contrast={contrast} />
        <strong>{names[mode]}{mode === current && <span className="blend-current" aria-hidden="true"> •</span>}</strong>
        <span className="blend-description">{descriptions[mode]}</span>
      </button>)}
    </div>
    <div className="blend-detail">
      <BlendCanvas mode={active} top={top} base={base} opacity={opacity} size={220} useIntensity={useIntensity} contrast={contrast} />
      <div><span className="section-label">PREVIEW ONLY</span><h3>{names[active]}</h3>
        <p>{descriptions[active]}</p>
        <Slider label="Example opacity" min={0} max={1} step={.01} value={opacity}
          format={v => `${Math.round(v * 100)}%`} onInput={setOpacity} onChange={setOpacity} />
        <small>Click a mode to apply · Enter applies · Esc closes</small>
        <small>Colour ramps, not image data; masks and tone are not shown.</small>
      </div>
    </div>
  </div>;
}

export function BlendHelp({ current, onApply }: { current: Blend | ''; onApply: (mode: Blend) => void }) {
  const anchor = useRef<HTMLButtonElement>(null);
  const [open, setOpen] = useState(false);
  const close = () => { setOpen(false); anchor.current?.focus(); };
  return <span ref={el => { anchor.current = el?.querySelector('button') ?? null; }} className="blend-help">
    <IconButton icon={CircleHelp} title="How blend modes work" aria-expanded={open} aria-haspopup="dialog"
      onClick={() => setOpen(!open)} />
    {open && <Popover anchor={anchor} onClose={() => setOpen(false)} className="blend-help-popover">
      <BlendGuide current={current} onApply={onApply} onClose={close} />
    </Popover>}
  </span>;
}
