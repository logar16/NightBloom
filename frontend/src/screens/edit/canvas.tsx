import { useCallback, useEffect, useRef, useState, type RefObject } from 'react';
import { Brush, Crop as CropIcon, Expand, Hand, MapPin, Maximize, Move3d, Pipette, RotateCcw, Shrink, X } from 'lucide-react';
import { useStore } from 'zustand';
import { IconButton } from '../../ui';
import { Compositor } from '../../editor/compositor';
import { clamp, type Crop, type Layer, type ProjectMeta } from '../../editor/state';
import { editorStore, useEditor } from '../../editor/store';
import { bindEditorLoad } from '../../editor/lifecycle';
import { markerHover, MarkerOverlay, markerStore, type MarkerRectangle } from './markers';
import type { BrushOptions } from './brush';
import { soloStore, useSolo } from '../../editor/solo';
import { CanvasInteractions } from './info';
import { perfStart } from '../../editor/perf';
import { invertBrushMode } from '../../editor/brush';
import { LayerClipOverlay } from './layer-clip';
import { viewFrame } from '../../editor/view';

export type Tool = 'pan' | 'layer' | 'crop' | 'clip' | 'rotate' | 'mark' | 'brush' | 'info';
function layerPoint(p: { x: number; y: number }, layer: Layer, meta: ProjectMeta): [number, number] {
  const x = (p.x - .5 - layer.transform.x) * meta.width, y = (p.y - .5 - layer.transform.y) * meta.height;
  const r = layer.transform.rotation * Math.PI / 180, c = Math.cos(r), s = Math.sin(r), scale = Math.max(.001, layer.transform.scale);
  return [Math.round(((c * x + s * y) / scale + meta.width / 2) * 2) / 2,
    Math.round(((-s * x + c * y) / scale + meta.height / 2) * 2) / 2];
}
interface Rectangle { x1: number; y1: number; x2: number; y2: number }
interface Drag {
  x: number; y: number; lastX: number; lastY: number; start: { x: number; y: number };
  tool: Tool; moved: boolean; angle: number; rotation: number;
}
export interface CanvasHandle {
  compositor: Compositor | null;
  fit: () => void;
  actualSize: () => void;
  render: () => void;
}
interface Props {
  meta: ProjectMeta;
  handle: RefObject<CanvasHandle | null>;
  tool: Tool;
  setTool: (tool: Tool) => void;
  aspect: number;
  pending: Crop | null;
  onPending: (crop: Crop | null) => void;
  onReady: (compositor: Compositor) => void;
  onStatus: (status: { hint?: string; pixel?: string; zoom?: string }) => void;
  zoom: string;
  brush: BrushOptions;
  inverted: boolean;
  spacePan: boolean;
  onExport: () => void;
  exportDisabled: boolean;
  onCheckpoint: () => void;
  infoSize: number;
}
export function CanvasView({ meta, handle, tool, setTool, aspect, pending, onPending, onReady, onStatus, zoom, brush, inverted, spacePan, onExport, exportDisabled, onCheckpoint, infoSize }: Props) {
  const canvas = useRef<HTMLCanvasElement>(null), compositor = useRef<Compositor | null>(null);
  const frame = useRef(0), drag = useRef<Drag | null>(null), rectRef = useRef<Rectangle | null>(null);
  const [rectangle, setRectangle] = useState<Rectangle | null>(null), [message, setMessage] = useState('Loading composition…');
  const [loadVersion, reload] = useState(0);
  const [contextLost, setContextLost] = useState(false);
  const [markDraft, setMarkDraft] = useState<MarkerRectangle | null>(null), [hoveredMarker, setHoveredMarker] = useState('');
  const [size, setSize] = useState({ width: 1, height: 1 });
  const [cursor, setCursor] = useState<{ x: number; y: number } | null>(null);
  const areaRef = useRef<HTMLDivElement>(null), [fullscreen, setFullscreen] = useState(false);
  const toggleFullscreen = useCallback(() => {
    if (document.fullscreenElement) void document.exitFullscreen();
    else void areaRef.current?.requestFullscreen().catch(() => undefined);
  }, []);
  useEffect(() => {
    const change = () => {
      setFullscreen(document.fullscreenElement === areaRef.current);
      // Refit once the element has its new size.
      requestAnimationFrame(() => requestAnimationFrame(() => handle.current?.fit()));
    };
    document.addEventListener('fullscreenchange', change);
    return () => document.removeEventListener('fullscreenchange', change);
  }, [handle]);
  const firstRender = useRef<(() => void) | null>(null);
  const selected = useEditor(s => s.selected);
  const solo = useSolo();
  const state = useEditor(s => s.state)!;
  const markersReady = useStore(markerStore, s => s.ready);
  const onStatusRef = useRef(onStatus); onStatusRef.current = onStatus;
  const onReadyRef = useRef(onReady); onReadyRef.current = onReady;
  const render = useCallback(() => {
    if (frame.current) return;
    frame.current = requestAnimationFrame(() => {
      frame.current = 0;
      const comp = compositor.current, current = editorStore.getState().state;
      if (!comp?.ready || !current) return;
      const color = document.createElement('canvas').getContext('2d');
      if (color && canvas.current) {
        color.fillStyle = getComputedStyle(canvas.current).getPropertyValue('--focus').trim();
        color.fillRect(0, 0, 1, 1);
        comp.brushTint = [...color.getImageData(0, 0, 1, 1).data].slice(0, 3).map(v => v / 255);
      }
      const endFrame = perfStart('frame');
      comp.resize(); comp.compose(current);
      endFrame();
      firstRender.current?.(); firstRender.current = null;
      const bounds = canvas.current!.getBoundingClientRect(), b = viewFrame(meta, current.global);
      const scale = Math.min(bounds.width / b.width, bounds.height / b.height);
      onStatusRef.current({ zoom: `${Math.round(comp.view.zoom * scale * 100)}%` });
    });
  }, [meta]);
  useEffect(() => {
    if (!canvas.current) return;
    const controller = new AbortController();
    const element = canvas.current;
    const lost = (event: Event) => {
      event.preventDefault(); controller.abort();
      if (comp) comp.ready = false;
      setContextLost(true); setMessage('GPU context lost. Reload the editor to continue; saved edits are preserved.');
    };
    element.addEventListener('webglcontextlost', lost);
    firstRender.current = perfStart('first-render');
    const unbindLoad = bindEditorLoad(() => {
      if (!compositor.current?.ready) controller.abort();
    }, () => { if (controller.signal.aborted) reload(v => v + 1); });
    let comp: Compositor | null = null;
    try {
      comp = new Compositor(canvas.current); compositor.current = comp;
      handle.current = {
        compositor: comp, render,
        fit: () => { if (comp) { comp.view = { zoom: 1, x: 0, y: 0 }; render(); } },
        actualSize: () => {
          if (!comp?.ready) return;
          const bounds = canvas.current!.getBoundingClientRect(), b = viewFrame(meta, editorStore.getState().state!.global);
          comp.view = { zoom: 1 / Math.min(bounds.width / b.width, bounds.height / b.height), x: 0, y: 0 };
          render();
        },
      };
      void comp.load(meta, setMessage, controller.signal).then(() => {
        if (controller.signal.aborted || !comp) return;
        setMessage('');
        onStatusRef.current({ hint: `WebGL2 · ${comp.float ? '16-bit float' : '8-bit fallback'}` });
        onReadyRef.current(comp); render();
      }).catch((error: Error) => { if (!controller.signal.aborted) setMessage(error.message); });
    } catch (error) { setMessage(error instanceof Error ? error.message : String(error)); }
    const observer = new ResizeObserver(() => {
      const bounds = canvas.current?.getBoundingClientRect();
      if (bounds) setSize({ width: bounds.width, height: bounds.height });
      render();
    });
    observer.observe(canvas.current);
    return () => {
      controller.abort(); unbindLoad(); observer.disconnect(); cancelAnimationFrame(frame.current); frame.current = 0;
      element.removeEventListener('webglcontextlost', lost);
      comp?.dispose(); compositor.current = null; handle.current = null;
    };
  }, [meta, handle, render, loadVersion]);
  useEffect(() => { render(); }, [state, render]);
  useEffect(() => {
    // Rotation changes the fitted bounding box; discard stale pan/zoom offsets.
    if (compositor.current) compositor.current.view = { zoom: 1, x: 0, y: 0 };
    render();
  }, [state.global.rotation, render]);
  useEffect(() => {
    if (compositor.current) compositor.current.solo = solo;
    render();
  }, [solo, render]);
  useEffect(() => {
    const comp = compositor.current;
    if (comp) comp.showBrush = tool === 'brush' && brush.showMask ? selected.at(-1) || null : null;
    render();
  }, [tool, brush.showMask, selected, render]);
  useEffect(() => {
    compositor.current?.previewBrush.clear(); render();
    drag.current = null; rectRef.current = null; setRectangle(null); setMarkDraft(null); onPending(null);
  }, [tool, onPending, render]);
  useEffect(() => { if (!pending && !drag.current) { rectRef.current = null; setRectangle(null); } }, [pending]);
  useEffect(() => {
    const element = canvas.current;
    const wheel = (e: WheelEvent) => {
      const comp = compositor.current; if (!comp?.ready) return;
      e.preventDefault(); comp.view.zoom = clamp(comp.view.zoom * Math.exp(-e.deltaY * .001), .1, 12); render();
    };
    element?.addEventListener('wheel', wheel, { passive: false });
    return () => element?.removeEventListener('wheel', wheel);
  }, [render]);
  const endDrag = () => {
    const current = drag.current, comp = compositor.current;
    if (!current || !comp?.ready) return;
    if (current.tool === 'brush') {
      const strokes = comp.previewBrush;
      editorStore.getState().change(s => {
        for (const layer of s.layers) {
          const stroke = strokes.get(layer.id);
          if (stroke) layer.brush.strokes.push(structuredClone(stroke));
        }
      });
      strokes.clear();
    } else if (current.tool === 'mark') {
      const start = current.start, end = comp.screenToImage(current.lastX, current.lastY);
      if (current.moved) markerStore.getState().rect(start.x * meta.width, start.y * meta.height, end.x * meta.width, end.y * meta.height);
      else markerStore.getState().point(start.x * meta.width, start.y * meta.height);
      setMarkDraft(null);
    } else if (current.tool === 'crop' && current.moved && rectRef.current) {
      const r = rectRef.current, a = comp.screenToOutput(r.x1, r.y1), b = comp.screenToOutput(r.x2, r.y2);
      const frame = viewFrame(meta, state.global);
      const minW = Math.min(.01, frame.w), minH = Math.min(.01, frame.h);
      const x = clamp(Math.min(a.x, b.x), frame.x, frame.x + frame.w - minW);
      const y = clamp(Math.min(a.y, b.y), frame.y, frame.y + frame.h - minH);
      onPending({ x, y, w: clamp(Math.max(a.x, b.x), x + minW, frame.x + frame.w) - x,
        h: clamp(Math.max(a.y, b.y), y + minH, frame.y + frame.h) - y });
    }
    if (current.tool === 'layer' || current.tool === 'rotate') editorStore.getState().commit();
    if (current.tool !== 'crop' || !current.moved) { rectRef.current = null; setRectangle(null); }
    drag.current = null;
    render();
  };
  const comp = compositor.current;
  const active = state.layers.find(l => l.id === selected.at(-1));
  const origin = comp?.ready && comp.state ? comp.imageToScreen(.5, .5) : null;
  const cursorSize = tool === 'info' ? infoSize : brush.size * (active?.transform.scale || 1);
  const offset = comp?.ready && comp.state ? comp.imageToScreen(.5 + cursorSize / meta.width / 2, .5) : null;
  const brushRadius = origin && offset ? Math.hypot(offset.x - origin.x, offset.y - origin.y) : 0;
  const angle = state.global.rotation * Math.PI / 180, radius = Math.min(size.width, size.height) * .32;
  const cx = size.width / 2, cy = size.height / 2;
  return <div ref={areaRef} className="canvas-area" id="canvas-area" data-testid="canvas-area" data-tool={spacePan ? 'pan' : tool}>
    {solo && <button className="solo-badge" onClick={() => soloStore.getState().exit()}
      aria-label={`Exit solo · ${solo.length}`} title="Exit solo (Esc)">Solo · {solo.length}<X /></button>}
    <div className="canvas-view-controls" aria-label="View controls">
      <span id="zoom-value" className="mono" aria-label="Zoom">{zoom}</span>
      <IconButton id="fit" icon={Maximize} title="Fit image (F)" onClick={() => handle.current?.fit()} />
      <IconButton id="fullscreen" icon={fullscreen ? Shrink : Expand}
        title={fullscreen ? 'Exit fullscreen (Esc)' : 'Fullscreen preview (Shift+F)'} onClick={toggleFullscreen} />
    </div>
    <div className="canvas-tools">
      {([
        ['pan', 'Pan (H)', Hand], ['layer', 'Move layer (V)', Move3d], ['crop', 'Crop (C)', CropIcon],
        ['rotate', 'Rotate (R)', RotateCcw], ['mark', 'Mark (M)', MapPin],
        ['brush', 'Feather brush (B)', Brush], ['info', 'Info (I)', Pipette],
      ] as const).map(([name, title, icon]) => <IconButton key={name} icon={icon}
        title={title} data-tool={name}
        className={tool === name ? 'active' : ''} disabled={name === 'mark' && !markersReady} onClick={() => setTool(name)}
        onDoubleClick={name === 'crop' ? () => {
          onPending(null); editorStore.getState().change(s => { s.global.crop = { x: 0, y: 0, w: 1, h: 1 }; });
          handle.current?.fit();
        } : undefined} />)}
    </div>
    <canvas id="composite" ref={canvas} aria-label="Astronomical composition" tabIndex={0} title={hoveredMarker || undefined}
      onMouseDown={e => { if (e.button === 1) e.preventDefault(); }}
      onPointerDown={e => {
        const comp = compositor.current;
        // Middle-button drag always pans, whatever tool is active.
        const middle = e.button === 1;
        if (!comp?.ready || (e.button !== 0 && !middle)) return;
        e.preventDefault(); e.currentTarget.focus(); e.currentTarget.setPointerCapture(e.pointerId);
        const bounds = e.currentTarget.getBoundingClientRect(), x = e.clientX - bounds.left, y = e.clientY - bounds.top;
        drag.current = {
          x, y, lastX: x, lastY: y, start: comp.screenToImage(x, y),
          tool: spacePan || middle ? 'pan' : e.altKey && tool !== 'brush' ? 'layer' : tool, moved: false,
          angle: Math.atan2(y - bounds.height / 2, x - bounds.width / 2), rotation: state.global.rotation,
        };
        if (drag.current.tool === 'brush') {
          const p = comp.screenToImage(x, y), ids = editorStore.getState().selected;
          const mode = e.altKey ? invertBrushMode(brush.mode) : brush.mode;
          for (const layer of state.layers) if (ids.includes(layer.id)) {
            comp.previewBrush.set(layer.id, { mode, size: brush.size, hardness: brush.hardness, strength: brush.strength,
              points: [layerPoint(p, layer, meta)] });
          }
          setCursor({ x, y }); render();
        }
        if (drag.current.tool === 'crop') {
          onPending(null); rectRef.current = { x1: x, y1: y, x2: x, y2: y }; setRectangle(rectRef.current);
        }
      }}
      onPointerMove={e => {
        const comp = compositor.current;
        if (!comp?.ready) return;
        const bounds = e.currentTarget.getBoundingClientRect(), x = e.clientX - bounds.left, y = e.clientY - bounds.top;
        const p = comp.screenToImage(x, y), current = drag.current;
        if (tool === 'brush' || tool === 'info') setCursor({ x, y });
        onStatusRef.current({ pixel: `${Math.round(p.x * meta.width)}, ${Math.round(p.y * meta.height)}` });
        setHoveredMarker(current ? '' : markerHover(comp, meta, x, y));
        if (tool === 'info' && !current) return;
        if (current) {
          const dx = x - current.lastX, dy = y - current.lastY;
          current.moved ||= Math.hypot(x - current.x, y - current.y) > 3;
          if (current.tool === 'pan') { comp.view.x += dx; comp.view.y += dy; render(); }
          if (current.tool === 'brush') {
            for (const layer of state.layers) {
              const stroke = comp.previewBrush.get(layer.id);
              if (!stroke) continue;
              const point = layerPoint(p, layer, meta), last = stroke.points.at(-1)!;
              if (Math.hypot(point[0] - last[0], point[1] - last[1]) >= Math.max(.5, stroke.size * .1)) stroke.points.push(point);
            }
            render();
          }
          if (current.tool === 'layer') {
            const previous = comp.screenToImage(current.lastX, current.lastY), selected = editorStore.getState().selected.at(-1);
            editorStore.getState().change(s => {
              const layer = s.layers.find(l => l.id === selected);
              if (layer) { layer.transform.x += p.x - previous.x; layer.transform.y += p.y - previous.y; }
            }, false);
          }
          if (current.tool === 'rotate') {
            const a = Math.atan2(y - bounds.height / 2, x - bounds.width / 2);
            editorStore.getState().change(s => {
              s.global.rotation = ((current.rotation + (a - current.angle) * 180 / Math.PI + 540) % 360) - 180;
            }, false);
          }
          if (current.tool === 'crop' && rectRef.current) {
            const rect = rectRef.current;
            rectRef.current = {
              ...rect, x2: x, y2: aspect ? rect.y1 + Math.sign(y - rect.y1 || 1) * Math.abs(x - rect.x1) / aspect : y,
            };
            setRectangle(rectRef.current);
          }
          if (current.tool === 'mark' && current.moved) setMarkDraft({
            x1: current.start.x * meta.width, y1: current.start.y * meta.height,
            x2: p.x * meta.width, y2: p.y * meta.height,
          });
          current.lastX = x; current.lastY = y;
        }
      }}
      onPointerUp={endDrag}
      onPointerCancel={() => {
        compositor.current?.previewBrush.clear(); render();
        if (drag.current?.tool === 'rotate' || drag.current?.tool === 'layer') editorStore.getState().commit();
        drag.current = null; rectRef.current = null; setRectangle(null); setMarkDraft(null); onPending(null);
      }}
      onPointerLeave={() => {
        setCursor(null);
        setHoveredMarker('');
      }} />
    <svg id="editor-overlay" viewBox={`0 0 ${size.width} ${size.height}`} aria-hidden="true">
      {(tool === 'brush' && active || tool === 'info') && !spacePan && cursor && <g className={`brush-cursor ${tool === 'brush' && inverted ? 'inverted' : ''}`}>
        <circle cx={cursor.x} cy={cursor.y} r={brushRadius} />
        {tool === 'brush' && <circle className="brush-hardness" cx={cursor.x} cy={cursor.y} r={brushRadius * brush.hardness} />}
        <path d={`M${cursor.x - 3} ${cursor.y}h6M${cursor.x} ${cursor.y - 3}v6`} />
      </g>}
      {rectangle && <rect className="crop-outline" x={Math.min(rectangle.x1, rectangle.x2)}
        y={Math.min(rectangle.y1, rectangle.y2)} width={Math.abs(rectangle.x2 - rectangle.x1)}
        height={Math.abs(rectangle.y2 - rectangle.y1)} />}
      {tool === 'rotate' && <>
        <circle className="rotation-dial" cx={cx} cy={cy} r={radius} />
        <path className="rotation-hand" d={`M${cx} ${cy}L${cx + Math.sin(angle) * radius} ${cy - Math.cos(angle) * radius}`} />
        <circle className="rotation-knob" cx={cx + Math.sin(angle) * radius} cy={cy - Math.cos(angle) * radius} r={6} />
        <text className="rotation-label" x={cx} y={cy + 25}>{state.global.rotation.toFixed(1)}°</text>
      </>}
    </svg>
    <MarkerOverlay compositor={comp} meta={meta} width={size.width} height={size.height}
      left={canvas.current?.offsetLeft || 0} draft={markDraft} />
    {comp?.ready && comp.state && active && (active.clip || tool === 'clip') && <LayerClipOverlay
      key={active.id} compositor={comp} layer={active} width={size.width} height={size.height}
      left={canvas.current?.offsetLeft || 0} editing={tool === 'clip' && !spacePan} />}
    <CanvasInteractions canvas={canvas} compositor={comp} meta={meta} tool={tool} setTool={setTool}
      spacePan={spacePan} handle={handle} onExport={onExport} exportDisabled={exportDisabled} onCheckpoint={onCheckpoint} infoSize={infoSize} />
    <div id="render-message" className="render-message">{message}
      {contextLost && <button onClick={() => window.location.reload()}>Reload editor</button>}
    </div>
    <span id="canvas-badge" className="canvas-badge mono">{comp?.previewScale === 2 ? 'Preview at ½ resolution · ' : ''}{meta.layers.length} FILTERS</span>
  </div>;
}
