import { useEffect, useLayoutEffect, useRef, useState, type CSSProperties, type RefObject } from 'react';
import { createPortal } from 'react-dom';
import { Download, Flag, Focus, MapPin, Maximize, MousePointer2, Pin, Pipette, Redo2, Undo2, X, ZoomIn } from 'lucide-react';
import { ContextMenu, floatingPosition, type MenuEntry } from '../../ui/ContextMenu';
import { Slider } from '../../ui';
import { Compositor } from '../../editor/compositor';
import { editorStore, useEditor } from '../../editor/store';
import { snapshot, type EditorState, type ProjectMeta } from '../../editor/state';
import { soloStore } from '../../editor/solo';
import { toast } from '../../api/hooks';
import { projectHeaders } from '../../api/client';
import { markerStore } from './markers';
import { layerSwatch } from './panels';
import type { CanvasHandle, Tool } from './canvas';
import './info.css';

export interface ProbeLayer { id: string; name: string; share: number; added_rgb: number[] }
export interface ProbeResult { final_hex: string; final_rgb: number[]; layers: ProbeLayer[] }
interface Point { x: number; y: number; screenX: number; screenY: number; radius: number }

export function loadInfoSize() {
  try { return Math.max(1, Math.min(200, Number(localStorage.getItem('astroimp.info.size')) || 5)); }
  catch { return 5; }
}
export function InfoOptions({ size, onSize }: { size: number; onSize: (size: number) => void }) {
  return <><strong>Info</strong><div className="info-size"><Slider label="Size" value={size}
    min={1} max={200} step={1} unit="px" log onInput={onSize} /></div></>;
}

export async function probePixel(point: Pick<Point, 'x' | 'y' | 'radius'>, state: EditorState, signal?: AbortSignal): Promise<ProbeResult> {
  const response = await fetch('/api/probe', {
    method: 'POST', headers: { ...projectHeaders(editorStore.getState().sessionId), 'Content-Type': 'application/json' }, signal,
    body: JSON.stringify({ x: point.x, y: point.y, radius: point.radius, state: snapshot(state) }),
  });
  if (!response.ok) {
    const error = await response.json().catch(() => ({})) as { detail?: string };
    throw Error(error.detail || `Pixel inspection failed (${response.status})`);
  }
  return response.json() as Promise<ProbeResult>;
}

export function selectContributor(id: string) {
  if (!editorStore.getState().state?.layers.some(l => l.id === id)) return;
  editorStore.getState().select(id);
  document.querySelector<HTMLElement>(`.layer-row[data-layer="${CSS.escape(id)}"]`)
    ?.scrollIntoView({ block: 'nearest', behavior: 'instant' });
}

function InfoReadout({ point, pinned, onClose }: { point: Point; pinned: boolean; onClose: () => void }) {
  const state = useEditor(s => s.state)!;
  const [result, setResult] = useState<ProbeResult | null>(null), [loading, setLoading] = useState(true);
  const [error, setError] = useState('');
  const [style, setStyle] = useState<CSSProperties>({ visibility: 'hidden' });
  const panel = useRef<HTMLDivElement>(null);
  useEffect(() => {
    const controller = new AbortController();
    const loadingFrame = requestAnimationFrame(() => { setLoading(true); setError(''); });
    const timer = setTimeout(() => {
      void probePixel(point, state, controller.signal).then(data => {
        if (!controller.signal.aborted) { setResult(data); setLoading(false); }
      }).catch((failure: Error) => {
        if (!controller.signal.aborted) { setError(failure.message); setLoading(false); }
      });
    }, 120);
    return () => { cancelAnimationFrame(loadingFrame); clearTimeout(timer); controller.abort(); };
  }, [point.x, point.y, point.radius, state]);
  useLayoutEffect(() => {
    const position = () => {
      if (!panel.current) return;
      const bounds = panel.current.getBoundingClientRect();
      setStyle(floatingPosition(point.screenX, point.screenY, bounds.width, bounds.height, 22));
    };
    const observer = new ResizeObserver(position);
    if (panel.current) observer.observe(panel.current);
    position();
    window.addEventListener('resize', position);
    return () => { observer.disconnect(); window.removeEventListener('resize', position); };
  }, [point.screenX, point.screenY]);
  const top = result?.layers.filter(l => l.share >= .02).slice(0, 5) ?? [];
  const others = (result?.layers.length ?? 0) - top.length;
  return createPortal(<div ref={panel} className={`ui-popover pixel-info-panel ${pinned ? 'pinned' : ''}`}
    style={style} data-testid="pixel-inspector" data-pinned={pinned} role={pinned ? 'dialog' : 'status'}
    aria-label="Pixel inspector" aria-busy={loading}>
    <div className="pixel-info-heading"><Pipette /><strong>Pixel info</strong>
      <span className="pixel-coordinates">{point.x}, {point.y}</span>
      {pinned && <><Pin className="pixel-pin" /><button aria-label="Close pixel inspector" title="Close (Esc)" onClick={onClose}><X /></button></>}
    </div>
    <div className={`pixel-info-result ${loading ? 'loading' : ''}`}>
      {result && <><div className="pixel-output"><span className="pixel-final-swatch" style={{ background: result.final_hex }} />
        <strong>{result.final_hex.toUpperCase()}</strong><span>Ø {point.radius * 2} px</span></div>
        <div className="pixel-contributors" aria-label="Contributing layers">
          {top.map(entry => {
            const layer = state.layers.find(l => l.id === entry.id);
            return <button key={entry.id} className="pixel-contributor" data-layer={entry.id}
              title={`Select ${entry.name} · added RGB ${entry.added_rgb.join(', ')}`}
              onClick={() => selectContributor(entry.id)}>
              <span className="pixel-layer-strip" style={{ background: layer ? layerSwatch(layer) : 'var(--muted)' }} />
              <span className="pixel-contribution-body"><span className="pixel-contribution-label"><span>{entry.name}</span>
                <span className="pixel-share">{Math.round(entry.share * 100)}%</span></span>
                <span className="pixel-share-track"><span style={{ width: `${entry.share * 100}%`, background: layer ? layerSwatch(layer) : 'var(--focus)' }} /></span>
              </span>
              <span className="pixel-added-swatch" style={{ background: `rgb(${entry.added_rgb.map(v => Math.round(Math.max(0, Math.min(1, v)) * 255)).join(' ')})` }} />
            </button>;
          })}
          {!top.length && <div className="pixel-info-note">No layer light here</div>}
          {others > 0 && <div className="pixel-info-note">+{others} others</div>}
        </div></>}
    </div>
    {(!result || error) && <div className="pixel-info-note" role={error ? 'alert' : undefined}>{error || 'Inspecting…'}</div>}
  </div>, document.body);
}

export function CanvasInteractions({ canvas, compositor, meta, tool, setTool, spacePan, handle, onExport, exportDisabled, onCheckpoint, infoSize }: {
  canvas: RefObject<HTMLCanvasElement | null>; compositor: Compositor | null; meta: ProjectMeta;
  tool: Tool; setTool: (tool: Tool) => void; spacePan: boolean; handle: RefObject<CanvasHandle | null>;
  onExport: () => void; onCheckpoint: () => void;
  exportDisabled: boolean;
  infoSize: number;
}) {
  const [point, setPoint] = useState<Point | null>(null), [pinned, setPinned] = useState(false);
  const [menu, setMenu] = useState<Point | null>(null);
  const ready = !!compositor?.ready;
  const current = useRef({ tool, spacePan, pinned, infoSize }); current.current = { tool, spacePan, pinned, infoSize };
  const dismiss = () => { setPoint(null); setPinned(false); };
  useEffect(() => {
    if (tool !== 'info') { setPoint(null); setPinned(false); }
  }, [tool]);
  useEffect(() => { setPoint(p => p ? { ...p, radius: infoSize / 2 } : p); }, [infoSize]);
  useEffect(() => {
    const element = canvas.current;
    if (!element || !compositor?.ready) return;
    const at = (event: MouseEvent): Point | null => {
      const bounds = element.getBoundingClientRect();
      const p = compositor.screenToImage(event.clientX - bounds.left, event.clientY - bounds.top);
      if (p.x < 0 || p.y < 0 || p.x >= 1 || p.y >= 1) return null;
      return { x: Math.floor(p.x * meta.width), y: Math.floor(p.y * meta.height),
        screenX: event.clientX, screenY: event.clientY, radius: current.current.infoSize / 2 };
    };
    const context = (event: MouseEvent) => {
      event.preventDefault(); element.focus({ preventScroll: true });
      const bounds = element.getBoundingClientRect();
      setMenu(at(event) ?? { x: -1, y: -1, screenX: event.clientX || bounds.left + bounds.width / 2,
        screenY: event.clientY || bounds.top + bounds.height / 2, radius: current.current.infoSize / 2 });
    };
    const move = (event: PointerEvent) => {
      if (current.current.tool !== 'info' || current.current.spacePan) return;
      if (!current.current.pinned) setPoint(at(event));
    };
    const down = (event: PointerEvent) => {
      if (event.button !== 0 || current.current.tool !== 'info' || current.current.spacePan) return;
      event.preventDefault(); event.stopPropagation(); element.focus({ preventScroll: true });
      if (current.current.pinned) { setPinned(false); setPoint(null); }
      else { const next = at(event); setPoint(next); setPinned(!!next); }
    };
    const leave = () => { if (!current.current.pinned) setPoint(null); };
    element.addEventListener('contextmenu', context);
    element.addEventListener('pointermove', move, true);
    element.addEventListener('pointerdown', down, true);
    element.addEventListener('pointerleave', leave);
    return () => {
      element.removeEventListener('contextmenu', context);
      element.removeEventListener('pointermove', move, true);
      element.removeEventListener('pointerdown', down, true);
      element.removeEventListener('pointerleave', leave);
    };
  }, [canvas, compositor, meta, ready]);
  useEffect(() => {
    const key = (event: KeyboardEvent) => {
      if (event.key === 'Escape' && point && !menu) {
        event.preventDefault(); event.stopPropagation(); dismiss(); canvas.current?.focus();
      }
    };
    const outside = (event: PointerEvent) => {
      if (pinned && event.target !== canvas.current && !(event.target as Element).closest('.pixel-info-panel,.ui-context-menu')) dismiss();
    };
    document.addEventListener('keydown', key, true); document.addEventListener('keyup', key, true);
    document.addEventListener('pointerdown', outside);
    return () => {
      document.removeEventListener('keydown', key, true); document.removeEventListener('keyup', key, true);
      document.removeEventListener('pointerdown', outside);
    };
  }, [point, pinned, menu, canvas]);
  const actions = editorStore.getState(), valid = !!menu && menu.x >= 0;
  const items: MenuEntry[] = [
    { label: 'Inspect pixel here', icon: Pipette, shortcut: 'I', disabled: !valid, action: () => {
      setTool('info'); setPoint(menu); setPinned(true);
    } },
    { label: 'Select top contributing layer here', icon: MousePointer2, disabled: !valid, action: () => {
      if (menu && actions.state) void probePixel(menu, actions.state).then(result => {
        const first = result.layers.find(l => l.share > 0);
        if (first) selectContributor(first.id); else toast('No layer light here');
      }).catch((error: Error) => toast(error.message));
    } },
    { label: 'Add marker here', icon: MapPin, disabled: !valid || !markerStore.getState().ready,
      action: () => { if (menu) markerStore.getState().point(menu.x, menu.y); } },
    'separator',
    { label: 'Fit', icon: Maximize, shortcut: '0', action: () => handle.current?.fit() },
    { label: '100%', icon: ZoomIn, shortcut: '1', action: () => handle.current?.actualSize() },
    { label: 'Solo selected', icon: Focus, shortcut: 'Q', disabled: !actions.selected.length,
      action: () => { if (actions.selected.length) soloStore.getState().latch(actions.selected.at(-1)!, actions.selected); } },
    'separator',
    { label: 'Undo', icon: Undo2, shortcut: 'Ctrl+Z', disabled: actions.historyIndex <= 0, action: actions.undo },
    { label: 'Redo', icon: Redo2, shortcut: 'Ctrl+Shift+Z', disabled: actions.historyIndex >= actions.history.length - 1, action: actions.redo },
    { label: 'Save checkpoint', icon: Flag, shortcut: 'Ctrl+S', action: onCheckpoint },
    'separator',
    { label: 'Export image…', icon: Download, disabled: exportDisabled, action: onExport },
  ];
  return <>
    {point && <InfoReadout point={point} pinned={pinned} onClose={dismiss} />}
    {menu && <ContextMenu label="Viewport actions" position={{ x: menu.screenX, y: menu.screenY }}
      items={items} onClose={() => setMenu(null)} />}
  </>;
}
