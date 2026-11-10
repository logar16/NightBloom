import { useState, type ReactNode } from 'react';
import { Captions, Circle, Crop, RotateCcw, Square, X } from 'lucide-react';
import { ColorPicker, GradientBar, IconButton, Segmented, Select, Slider } from '../../ui';
import { gradientCSS, waveLabel } from '../../editor/compositor';
import { editorStore, useEditor } from '../../editor/store';
import { BLENDS, rampPresets, relativeAdjustments, type Blend, type Layer, type ProjectMeta } from '../../editor/state';
import { layerSwatch } from './panels';
import { BlendHelp } from './blend-help';
import { CurveEditor } from './curve';

export function ActiveLayer({ meta }: { meta: ProjectMeta }) {
  const layer = useEditor(s => s.state?.layers.find(l => l.id === s.selected.at(-1)));
  const count = useEditor(s => s.selected.length);
  const source = meta.layers.find(s => s.key === layer?.source);
  return <div className="active-layer-target" aria-label="Active layer" data-testid="active-layer-target">
    {layer ? <>
      <span className="layer-swatch" style={{ '--swatch': layerSwatch(layer) } as React.CSSProperties} />
      <div><strong title={layer.name}>{layer.name}</strong>
        <small>{source?.filter ?? 'Luminance'}{count > 1 ? ` · ${count} layers selected` : ''}</small>
      </div>
    </> : <strong>No layer selected</strong>}
  </div>;
}

export function changeProperty(path: string, value: unknown, committed = true) {
  const { selected, change } = editorStore.getState();
  change(state => {
    const parts = path.split('.');
    let target: unknown = path.startsWith('global.') ? state : state.layers.find(l => l.id === selected.at(-1));
    if (!target) return;
    for (const part of parts.slice(0, -1)) target = (target as Record<string, unknown>)[part];
    (target as Record<string, unknown>)[parts[parts.length - 1]] = path === 'mask.source' && !value ? null : value;
  }, committed);
}
export function Section({ label, children }: { label: string; children: ReactNode }) {
  return <div className="property-section"><h3 className="section-label">{label.charAt(0) + label.slice(1).toLowerCase()}</h3>{children}</div>;
}
const defaults: Record<string, number> = {
  black: 0, white: 1, k: 10, intensity: 1, opacity: 1, 'mask.black': 0, 'mask.white': 1,
  'transform.scale': 1, 'transform.rotation': 0, 'transform.x': 0, 'transform.y': 0,
  'global.rotation': 0, 'global.output.exposure': 0, 'global.output.highlights': 0,
  'global.output.black': 0, 'global.output.white': 1, 'global.output.midtones': 1,
  'edge.feather': 0, 'edge.irregularity': 0,
  'detail.amount': 0, 'detail.radius': 8,
  'clip.feather': .1,
};
export function PropertySlider({ label, path, value, min, max, step = .01, unit = '', log = false }: {
  label: string; path: string; value: number; min: number; max: number; step?: number; unit?: string; log?: boolean;
}) {
  return <div className="control-host" data-control="slider">
    <Slider label={label} value={value} min={min} max={max} step={step} log={log}
      defaultValue={defaults[path] ?? value} unit={unit}
      onInput={v => changeProperty(path, v, false)} onChange={v => changeProperty(path, v)} />
  </div>;
}
export function Toggle({ label, path, checked }: { label: string; path: string; checked: boolean }) {
  return <label className="field"><span>{label}</span>
    <input type="checkbox" checked={checked} onChange={e => changeProperty(path, e.target.checked)} />
  </label>;
}
function PropertySelect({ label, path, value, options }: {
  label: string; path: string; value: string; options: (string | [string, string])[];
}) {
  return <div className="control-host" data-control="select" data-label={label}>
    <Select label={label} value={value} options={options.map(o => typeof o === 'string'
      ? { value: o, label: o.charAt(0).toUpperCase() + o.slice(1) } : { value: o[0], label: o[1] })}
      onChange={v => changeProperty(path, v)} />
  </div>;
}
function OutputSection() {
  const output = useEditor(s => s.state!.global.output);
  return <Section label="OUTPUT · ALL LAYERS">
    <PropertySlider label="Exposure" path="global.output.exposure" value={output.exposure} min={-3} max={3} step={.05} />
    <PropertySlider label="Highlight roll-off" path="global.output.highlights" value={output.highlights} min={0} max={1} />
    <PropertySlider label="Black point" path="global.output.black" value={output.black ?? 0} min={0} max={1} />
    <PropertySlider label="White point" path="global.output.white" value={output.white ?? 1} min={.01} max={3} />
    <PropertySlider label="Midtones" path="global.output.midtones" value={output.midtones ?? 1} min={.3} max={4} />
  </Section>;
}
const blends = BLENDS;
function ColorProperties({ layer, meta, onLayerCrop, cropping }: {
  layer: Layer; meta: ProjectMeta; onLayerCrop: () => void; cropping: boolean;
}) {
  const sources: [string, string][] = meta.layers.map(s => [s.key, s.filter]);
  return <>
    <Section label="COLOR ASSIGNMENT">
      <label className="field"><span>Name</span><input type="text" value={layer.name}
        onChange={e => changeProperty('name', e.target.value, false)}
        onBlur={() => editorStore.getState().commit()}
        onKeyDown={e => { if (e.key === 'Enter') { editorStore.getState().commit(); e.currentTarget.blur(); } }} />
      </label>
      <PropertySelect label="Source" path="source" options={[...sources, ['lum', 'Luminance']]} value={layer.source} />
      <PropertySelect label="Mode" path="mode" options={['tint', 'gradient', 'spectral']} value={layer.mode} />
      {layer.mode === 'tint' ? <div className="control-host" data-control="colorPicker" data-label="Tint">
        <ColorPicker label="Tint" value={layer.color}
          presets={['#ff3232', '#40e070', '#4080ff', '#46c4cd', '#efad64', '#dcd8cf']}
          onInput={v => changeProperty('color', v, false)} onChange={v => changeProperty('color', v)} />
      </div> : layer.mode === 'gradient' ? <>
        <div id="intensity-gradient"><GradientBar stops={layer.gradient}
          onInput={stops => changeProperty('gradient', stops, false)}
          onChange={stops => changeProperty('gradient', stops)} /></div>
        <div className="presets ramp-presets">{rampPresets.map((ramp, i) =>
          <button key={i} title={`Gradient preset ${i + 1}`} style={{ background: gradientCSS(ramp) }}
            onClick={() => changeProperty('gradient', structuredClone(ramp))} />)}</div>
      </> : <>
        <div className="gradient-preview" style={{
          background: gradientCSS(layer.spectral.stops.map(s => ({ t: Math.log(s.nm / 200) / Math.log(125), color: s.color }))),
        }} />
        <div className="field"><span>Wavelength gradient</span><span className="mono">{layer.spectral.stops.length} stops</span></div>
      </>}
    </Section>
    <Section label="COMPOSITE">
      <div className="blend-control">
        <PropertySelect label="Blend" path="blend" options={blends} value={layer.blend} />
        <BlendHelp current={layer.blend} onApply={value => changeProperty('blend', value)} />
      </div>
      <PropertySlider label="Opacity" path="opacity" value={layer.opacity} min={0} max={1} />
    </Section>
    <Section label="TONE">
      <PropertySelect label="Stretch" path="stretch" options={['linear', 'asinh', 'log', 'gamma']} value={layer.stretch} />
      <PropertySlider label="Black" path="black" value={layer.black} min={0} max={1} />
      <PropertySlider label="White" path="white" value={layer.white} min={.01} max={3} />
      {layer.stretch !== 'linear' && <PropertySlider label={layer.stretch === 'gamma' ? 'Gamma' : 'Strength'}
        path="k" value={layer.k} min={.1} max={30} step={.1} />}
      <Toggle label="Invert" path="invert" checked={layer.invert} />
      <CurveEditor layer={layer} />
      <PropertySlider label="Intensity" path="intensity" value={layer.intensity} min={0} max={2} />
      {layer.toneMatch && <div className="field">
        <span>Survey match</span><span className="mono">
          {layer.toneMatch.gain.toFixed(2)}× {layer.toneMatch.offset >= 0 ? '+' : ''}{layer.toneMatch.offset.toFixed(2)}
        </span>
        <IconButton icon={RotateCcw} title="Reset survey tone match"
          onClick={() => changeProperty('toneMatch', undefined)} />
      </div>}
    </Section>
    <Section label="DETAIL">
      <PropertySlider label="Amount" path="detail.amount" value={layer.detail.amount} min={0} max={3} step={.05} />
      <PropertySlider label="Radius" path="detail.radius" value={layer.detail.radius} min={1} max={64} step={1} unit="px" log />
    </Section>
    <Section label="MASK">
      <PropertySelect label="Source" path="mask.source" options={[['', 'None'], ...sources]} value={layer.mask.source || ''} />
      {layer.mask.source && <>
        <PropertySlider label="Black" path="mask.black" value={layer.mask.black} min={0} max={1} />
        <PropertySlider label="White" path="mask.white" value={layer.mask.white} min={.01} max={2} />
        <Toggle label="Invert" path="mask.invert" checked={layer.mask.invert} />
      </>}
    </Section>
    <Section label="EDGE">
      <PropertySlider label="Edge softness" path="edge.feather" value={layer.edge.feather} min={0} max={400} step={1} unit="px" />
      <PropertySlider label="Edge irregularity" path="edge.irregularity" value={layer.edge.irregularity} min={0} max={1} />
    </Section>
    <Section label="LAYER CROP">
      <div className="layer-clip-controls">
        <button className={`subtle ${cropping ? 'active' : ''}`} aria-pressed={cropping}
          title="Draw or adjust a crop for this layer only; fixed in canvas space" onClick={onLayerCrop}><Crop />Crop layer</button>
        {layer.clip && <IconButton icon={X} title="Remove layer crop" onClick={() => changeProperty('clip', null)} />}
      </div>
      {layer.clip && <>
        <div className="layer-clip-controls" role="group" aria-label="Layer crop shape">
          <IconButton icon={Square} title="Rectangle" aria-pressed={layer.clip.shape === 'rect'}
            onClick={() => changeProperty('clip.shape', 'rect')} />
          <IconButton icon={Circle} title="Ellipse" aria-pressed={layer.clip.shape === 'ellipse'}
            onClick={() => changeProperty('clip.shape', 'ellipse')} />
        </div>
        <PropertySlider label="Crop feather" path="clip.feather" value={layer.clip.feather} min={0} max={.5} step={.01} />
      </>}
    </Section>
    <OutputSection />
  </>;
}
function RelativeProperties() {
  const selected = useEditor(s => s.selected);
  const state = useEditor(s => s.state)!;
  const layers = state.layers.filter(l => selected.includes(l.id));
  const active = layers.find(l => l.id === selected.at(-1)) || layers[0];
  const blend = layers.every(l => l.blend === active.blend) ? active.blend : '';
  const [values, setValues] = useState<Record<string, number>>({});
  return <>
    <Section label={`${selected.length} LAYERS`}>
      <div className="blend-control">
        <div className="control-host" data-control="select" data-label="Blend">
          <Select label="Blend" value={blend}
            options={[...(!blend ? [{ value: '', label: 'Mixed' }] : []), ...blends.map(value => ({
              value, label: value.charAt(0).toUpperCase() + value.slice(1),
            }))]} onChange={value => { if (value) editorStore.getState().absolute({ blend: value as Blend }); }} />
        </div>
        <BlendHelp current={blend} onApply={value => editorStore.getState().absolute({ blend: value })} />
      </div>
      {relativeAdjustments.map(a => <div className="control-host" data-control="slider" key={a.key}>
        <Slider label={a.label} value={values[a.key] || 0} min={a.min} max={a.max} step={a.unit === '%' ? 1 : .01}
          defaultValue={0} unit={a.unit}
          onInput={v => {
            setValues(previous => ({ ...previous, [a.key]: v }));
            editorStore.getState().relative(a.key, v, false);
          }}
          onChange={v => {
            editorStore.getState().relative(a.key, v, true);
            setValues(previous => ({ ...previous, [a.key]: 0 }));
          }} />
      </div>)}
      {([
        ['feather', 'Edge softness', 400, 1, 'px'],
        ['irregularity', 'Edge irregularity', 1, .01, ''],
      ] as const).map(([key, label, max, step, unit]) => <div className="control-host" data-control="slider" key={key}>
        <Slider label={label} value={active.edge[key]} min={0} max={max} step={step} unit={unit} defaultValue={0}
          onInput={value => editorStore.getState().absolute({ edge: { [key]: value } }, false)}
          onChange={value => editorStore.getState().absolute({ edge: { [key]: value } })} />
      </div>)}
      <button className="subtle" onClick={() => {
        const id = selected.at(-1); if (id) editorStore.getState().select(id);
      }}><X />Select one layer</button>
    </Section>
    <OutputSection />
  </>;
}
export type PropertyTab = 'color' | 'geometry';
export function Properties({ tab, meta, resetCrop, onCaption, onLayerCrop, cropping }: {
  tab: PropertyTab; meta: ProjectMeta; resetCrop: () => void;
  onCaption: () => void;
  onLayerCrop: () => void; cropping: boolean;
}) {
  const state = useEditor(s => s.state)!;
  const selected = useEditor(s => s.selected);
  const layer = state.layers.find(l => l.id === selected.at(-1));
  if (tab === 'color') {
    return <>
      {selected.length > 1 ? <RelativeProperties />
        : layer ? <ColorProperties layer={layer} meta={meta} onLayerCrop={onLayerCrop} cropping={cropping} /> : <div className="muted">No layer selected</div>}
      <Section label="CAPTION"><button id="caption" className="subtle" onClick={onCaption}>
        <Captions />Colour caption</button></Section>
    </>;
  }
  if (tab === 'geometry') return <>
    <Section label="CANVAS">
      <PropertySlider label="Rotation" path="global.rotation" value={state.global.rotation} min={-180} max={180} step={1} />
      <Toggle label="Flip horizontal" path="global.flipX" checked={state.global.flipX} />
      <button id="reset-crop" className="subtle" onClick={resetCrop}>Reset crop</button>
      <div className="field"><span>Crop</span><span className="mono">
        {Math.round(state.global.crop.w * 100)} × {Math.round(state.global.crop.h * 100)}%</span></div>
    </Section>
    {layer && <Section label="LAYER TRANSFORM">
      <PropertySlider label="Scale" path="transform.scale" value={layer.transform.scale} min={.1} max={3} />
      <PropertySlider label="Rotation" path="transform.rotation" value={layer.transform.rotation} min={-180} max={180} step={1} />
      <PropertySlider label="Horizontal" path="transform.x" value={layer.transform.x} min={-1} max={1} />
      <PropertySlider label="Vertical" path="transform.y" value={layer.transform.y} min={-1} max={1} />
      <button className="subtle" onClick={() => changeProperty('transform', { x: 0, y: 0, scale: 1, rotation: 0 })}>
        <RotateCcw />Reset transform</button>
    </Section>}
  </>;
  return null;
}
