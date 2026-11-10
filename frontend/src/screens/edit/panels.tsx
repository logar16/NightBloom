import { useCallback, useEffect, useRef, useState } from 'react';
import { Brush, Copy, Eye, EyeOff, Feather, Flag, Focus, Orbit, Pencil, Plus, Trash2, X } from 'lucide-react';
import { api } from '../../api/client';
import { toast, useRestoreExportLook, useShell } from '../../api/hooks';
import { GradientBar, IconButton, Popover } from '../../ui';
import { Compositor, gradientCSS, logPosition, waveLabel, wavelengthColor } from '../../editor/compositor';
import { editorStore, useEditor } from '../../editor/store';
import { makeLayer, type Layer, type ProjectMeta } from '../../editor/state';
import { soloStore, useSolo } from '../../editor/solo';
import { LayerHoverPreview, LayerThumbnail, useLayerThumbnails } from './thumbnails';
import { LayerContextMenu } from './contextmenus';

export function layerSwatch(layer: Layer) {
  return layer.mode === 'tint' ? layer.color : gradientCSS(layer.mode === 'gradient' ? layer.gradient
    : layer.spectral.stops.map(s => ({ t: logPosition(s.nm), color: s.color })));
}
export function LayerPanel({ meta, flush, compositor }: { meta: ProjectMeta; flush: () => Promise<void>; compositor: Compositor }) {
  const state = useEditor(s => s.state)!;
  const selected = useEditor(s => s.selected);
  const solo = useSolo();
  const images = useLayerThumbnails(compositor, state);
  const [preview, setPreview] = useState<{ id: string; anchor: HTMLElement } | null>(null);
  const [rename, setRename] = useState<{ id: string; name: string } | null>(null);
  const [menu, setMenu] = useState<{ x: number; y: number; target: string } | null>(null);
  const renameCancelled = useRef(false);
  const closePreview = useCallback(() => setPreview(null), []);
  const layer = state.layers.find(l => l.id === selected.at(-1));
  const source = meta.layers.find(s => s.key === layer?.source);
  const change = editorStore.getState().change;
  const startRename = useCallback((target: Layer) => {
    renameCancelled.current = false; setRename({ id: target.id, name: target.name });
  }, []);
  useEffect(() => {
    const key = (event: KeyboardEvent) => {
      if (event.key !== 'F2' || event.defaultPrevented || event.ctrlKey || event.metaKey || event.altKey
        || (event.target instanceof Element && event.target.closest('input,textarea,select,[role=combobox],[contenteditable=true],.ui-popover'))
        || document.querySelector('dialog[open]')) return;
      if (layer) { event.preventDefault(); startRename(layer); }
    };
    document.addEventListener('keydown', key);
    return () => document.removeEventListener('keydown', key);
  }, [layer, startRename]);
  const finishRename = () => {
    if (!rename || renameCancelled.current) return;
    const name = rename.name.trim();
    if (name && name !== state.layers.find(l => l.id === rename.id)?.name) change(s => {
      const target = s.layers.find(l => l.id === rename.id);
      if (target) target.name = name;
    });
    setRename(null);
  };
  return <aside className="layer-panel">
    <div className="panel-heading"><span>LAYERS</span><div>
      <IconButton id="show-layer" icon={Orbit} className="small" title="Show layer in Aladin"
        disabled={!layer || layer.source === 'lum'} onClick={() => {
          const keys = state.layers.filter(l => selected.includes(l.id) && l.source !== 'lum').map(l => l.source);
          void api('/aladin/layers', 'POST', { keys }).then(() => toast('Layers sent to Aladin')).catch((e: Error) => toast(e.message));
        }} />
      <IconButton id="add-layer" icon={Plus} className="small" title="Add layer" onClick={() => {
        const added = makeLayer();
        change(s => { s.layers.unshift(added); }, false);
        editorStore.getState().select(added.id); editorStore.getState().commit();
      }} />
      <IconButton id="duplicate-layer" icon={Copy} className="small" title="Duplicate layer (Ctrl+D)" disabled={!layer} onClick={() => {
        editorStore.getState().duplicateSelection();
      }} />
      <IconButton id="delete-layer" icon={Trash2} className="small" title="Delete layer (Delete / Backspace)" disabled={!layer} onClick={() => {
        editorStore.getState().deleteSelection();
      }} />
    </div></div>
    <div id="layer-list" role="grid" aria-label="Layers" aria-multiselectable="true">{state.layers.map(l => <div key={l.id} data-testid="layer-row" data-layer={l.id}
      className={`layer-row ${l.id === selected.at(-1) ? 'active' : ''} ${selected.length > 1 && selected.includes(l.id) ? 'multi' : ''} ${solo && !solo.includes(l.id) ? 'solo-muted' : ''}`}
      draggable={rename?.id !== l.id} tabIndex={0} role="row" aria-label={l.name} aria-selected={selected.includes(l.id)}
      aria-current={l.id === selected.at(-1) ? 'true' : undefined}
      onClick={e => editorStore.getState().select(l.id, e)}
      onContextMenu={e => {
        if ((e.target as Element).closest('input')) return;
        e.preventDefault(); setPreview(null);
        if (!editorStore.getState().selected.includes(l.id)) editorStore.getState().select(l.id);
        e.currentTarget.focus({ preventScroll: true });
        setMenu({ x: e.clientX, y: e.clientY, target: l.id });
      }}
      onKeyDown={e => {
        if (e.target !== e.currentTarget) return;
        if (e.key === 'Enter' || e.key === ' ') {
          e.preventDefault(); editorStore.getState().select(l.id, e);
        } else if (['ArrowUp', 'ArrowDown', 'Home', 'End'].includes(e.key)) {
          e.preventDefault();
          const index = state.layers.indexOf(l);
          const next = e.key === 'Home' ? 0 : e.key === 'End' ? state.layers.length - 1
            : Math.max(0, Math.min(state.layers.length - 1, index + (e.key === 'ArrowUp' ? -1 : 1)));
          editorStore.getState().select(state.layers[next].id, e);
          (e.currentTarget.parentElement?.children[next] as HTMLElement)?.focus();
        }
      }}
      onDragStart={e => { e.dataTransfer.setData('text/plain', l.id); e.dataTransfer.effectAllowed = 'move'; }}
      onDragOver={e => { e.preventDefault(); e.currentTarget.classList.add('drag-over'); }}
      onDragLeave={e => e.currentTarget.classList.remove('drag-over')}
      onDrop={e => {
        e.preventDefault(); e.currentTarget.classList.remove('drag-over');
        const id = e.dataTransfer.getData('text/plain');
        change(s => {
          const from = s.layers.findIndex(x => x.id === id), to = s.layers.findIndex(x => x.id === l.id);
          if (from >= 0 && to >= 0) s.layers.splice(to, 0, s.layers.splice(from, 1)[0]);
        });
      }}>
      <div role="gridcell" className="layer-view-buttons"><IconButton className={`small eye ${l.visible ? '' : 'off'}`} icon={l.visible ? Eye : EyeOff}
        title={`${l.visible ? 'Hide' : 'Show'} ${l.name}`} onClick={e => {
          e.stopPropagation(); editorStore.getState().toggleVisibility(l.id);
        }} />
        <IconButton icon={Focus} title="Show only this layer (Q)" aria-pressed={!!solo?.includes(l.id)}
          className={`small layer-solo ${solo?.includes(l.id) ? 'active' : ''}`} onClick={e => {
            e.stopPropagation(); soloStore.getState().latch(l.id, selected);
          }} />
      </div>
      <div role="gridcell" className="layer-identity">
        <LayerThumbnail layer={l} image={images[l.id]} swatch={layerSwatch(l)}
          onPreview={(id, anchor) => setPreview({ id, anchor })} />
      <div className="layer-text">{rename?.id === l.id
        ? <input autoFocus aria-label="Layer name" value={rename.name}
          onFocus={e => e.currentTarget.select()} onClick={e => e.stopPropagation()}
          onChange={e => setRename({ ...rename, name: e.target.value })} onBlur={finishRename}
          onKeyDown={e => {
            e.stopPropagation();
            if (e.key === 'Enter') { e.preventDefault(); finishRename(); }
            if (e.key === 'Escape') { e.preventDefault(); renameCancelled.current = true; setRename(null); }
          }} />
        : <strong title="Rename layer (F2)" onDoubleClick={e => { e.stopPropagation(); startRename(l); }}>{l.name}</strong>}
        <small>{l.mode} · {l.blend}</small></div>
      {l.edge.feather > 0 && <span className="edge-indicator" title={`Softened edges · ${l.edge.feather} px`}
        aria-label={`Softened edges · ${l.edge.feather} px`}><Feather /></span>}
      {l.brush.strokes.length > 0 && <span className="edge-indicator" title={`Feather brush · ${l.brush.strokes.length} strokes`}
        aria-label={`Feather brush · ${l.brush.strokes.length} strokes`}><Brush /></span>}
      </div>
    </div>)}</div>
    <div className="layer-footer"><button className="subtle" id="add-images" onClick={() => {
      void flush().then(() => useShell.getState().setScreen('sky')).catch((e: Error) => toast(e.message));
    }}><Plus />Add images</button><span id="layer-count" className="mono">{state.layers.length}</span></div>
    <div id="filter-info" className="filter-info">{source ? <>
      <strong>{source.info?.line || source.filter}</strong><div>{source.info?.shows || source.info?.band || source.instrument}</div>
      <div className="info-wave">{source.collection} · {waveLabel(source.wavelength_nm || 550)}</div>
      {source.fwhm_arcsec != null && <div className="info-wave">Detail ≈ {source.fwhm_arcsec}″</div>}
    </> : <><strong>Luminance</strong><div>All source filters</div></>}</div>
    {preview && <LayerHoverPreview {...preview} compositor={compositor} meta={meta} state={state} onClose={closePreview} />}
    {menu && <LayerContextMenu position={menu} target={menu.target} onRename={startRename} onClose={() => setMenu(null)} />}
  </aside>;
}
export function SpectralRail({ meta }: { meta: ProjectMeta }) {
  const state = useEditor(s => s.state)!;
  const selected = useEditor(s => s.selected);
  const layer = state.layers.find(l => l.id === selected.at(-1)), spectral = layer?.mode === 'spectral';
  const update = (stops: { t: number; color: string }[], committed: boolean) => editorStore.getState().change(s => {
    const target = s.layers.find(l => l.id === layer?.id);
    if (target) target.spectral.stops = stops.map(stop => ({ nm: stop.t, color: stop.color }));
  }, committed);
  return <div className="spectral-rail">
    <div className="rail-heading"><span id="rail-label">{spectral ? 'SPECTRAL GRADIENT' : 'WAVELENGTH'}</span>
      <span className="mono" id="rail-value">{spectral ? '' : layer ? waveLabel(meta.layers.find(l => l.key === layer.source)?.wavelength_nm || 550) : ''}</span></div>
    <div id="spectral-track" className={`spectral-track ${spectral ? 'editable' : ''}`}>
      {spectral && layer ? <GradientBar stops={layer.spectral.stops.map(s => ({ t: s.nm, color: s.color }))}
        axis={{ min: 200, max: 25000, log: true, format: waveLabel }}
        markers={meta.layers.map(l => ({
          value: l.wavelength_nm || 550, color: wavelengthColor(l.wavelength_nm), label: l.filter, title: l.info?.shows || l.filter,
        }))} onInput={stops => update(stops, false)} onChange={stops => update(stops, true)} />
        : meta.layers.map(l => <button key={l.key} className={`rail-marker ${l.key === layer?.source ? 'active' : ''}`}
          style={{ left: `${logPosition(l.wavelength_nm) * 100}%`, '--wave': wavelengthColor(l.wavelength_nm) } as React.CSSProperties}
          title={`${l.filter} · ${waveLabel(l.wavelength_nm || 550)} · ${l.info?.shows || ''}`}
          aria-label={`${l.filter} wavelength`} onClick={() => {
            const found = state.layers.find(x => x.source === l.key); if (found) editorStore.getState().select(found.id);
          }} />)}
    </div>
    {!spectral && <div className="rail-ticks">
      {[[0, '200 nm'], [18.98, '500 nm'], [33.33, '1 µm'], [66.67, '5 µm'], [100, '25 µm']].map(([left, label]) =>
        <span key={label} style={{ left: `${left}%` }}>{label}</span>)}
    </div>}
  </div>;
}
export function Checkpoints() {
  const anchor = useRef<HTMLButtonElement>(null);
  const [open, setOpen] = useState(false), [renaming, setRenaming] = useState<string | null>(null);
  const [name, setName] = useState('');
  const checkpoints = useEditor(s => s.state?.checkpoints);
  const project = useShell(s => s.project);
  const restore = useRestoreExportLook();
  const actions = editorStore.getState();
  const finishRename = () => { if (renaming) actions.renameCheckpoint(renaming, name); setRenaming(null); };
  return <>
    <button className="icon" id="checkpoints" ref={anchor} title="Checkpoints" aria-label="Checkpoints"
      aria-haspopup="true" aria-expanded={open} onClick={() => setOpen(!open)}><Flag /></button>
    {open && <Popover anchor={anchor} onClose={() => setOpen(false)}>
      <div className="checkpoint-list"><div className="popover-title">Checkpoints</div>
        <button className="subtle" id="create-checkpoint" onClick={() => {
          actions.createCheckpoint(); toast('Checkpoint saved');
        }}><Plus />Save checkpoint</button>
        {(checkpoints || []).map(point => <div key={point.id} className="checkpoint-row">
          {renaming === point.id ? <input autoFocus aria-label="Checkpoint name" value={name}
            onChange={e => setName(e.target.value)} onBlur={finishRename}
            onKeyDown={e => { if (e.key === 'Enter') finishRename(); if (e.key === 'Escape') setRenaming(null); }} />
            : <button title="Restore checkpoint" disabled={restore.isPending} onClick={() => {
              if (point.exportCheckpoint && project)
                restore.mutate({ project, checkpoint: point.exportCheckpoint });
              else actions.restoreCheckpoint(point.id);
              setOpen(false);
            }}>{point.name}</button>}
          <IconButton className="small" icon={Pencil} title="Rename checkpoint" onClick={() => {
            setRenaming(point.id); setName(point.name);
          }} />
          <IconButton className="small" icon={X} title="Delete checkpoint" onClick={() => actions.deleteCheckpoint(point.id)} />
        </div>)}
        <button className="subtle danger" id="reset-all" onClick={() => {
          if (confirm('Reset all edits? Checkpoints will be kept.')) { actions.reset(); setOpen(false); }
        }}>Reset all edits…</button>
      </div>
    </Popover>}
  </>;
}
