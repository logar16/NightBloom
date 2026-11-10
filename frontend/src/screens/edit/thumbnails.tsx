import { useEffect, useRef, useState, type CSSProperties } from 'react';
import { createPortal } from 'react-dom';
import { Compositor, waveLabel } from '../../editor/compositor';
import { previewPlacement } from '../../editor/preview';
import { editorStore } from '../../editor/store';
import type { EditorState, Layer, ProjectMeta } from '../../editor/state';
import { perfStart } from '../../editor/perf';
import { viewFrame } from '../../editor/view';

export function useLayerThumbnails(compositor: Compositor, state: EditorState) {
  const cache = useRef(new Map<string, string>());
  const [images, setImages] = useState<Record<string, string>>({});
  useEffect(() => {
    let frame = 0;
    const timer = setTimeout(() => {
      frame = requestAnimationFrame(() => {
        if (!compositor.ready) return;
        const global = state.global;
        const view = JSON.stringify([global.crop, global.rotation, global.flipX]);
        const queue = state.layers.flatMap(layer => {
          const signature = JSON.stringify({ ...layer, visible: true, opacity: 1, blend: 'add', name: '' }) + view;
          return cache.current.get(layer.id) === signature ? [] : [{ layer, signature }];
        });
        const ids = new Set(state.layers.map(l => l.id));
        for (const id of cache.current.keys()) if (!ids.has(id)) cache.current.delete(id);
        const endRefresh = perfStart('thumbnail-refresh');
        let index = 0;
        const next = () => {
          if (!compositor.ready) return;
          const item = queue[index++];
          if (item) {
            const end = perfStart('thumbnails');
            const { layer, signature } = item;
            const image = compositor.renderLayerPreview(layer, state, Math.round(128 * Math.min(devicePixelRatio || 1, 2))).canvas.toDataURL();
            cache.current.set(layer.id, signature);
            setImages(previous => ({ ...previous, [layer.id]: image }));
            end();
          }
          if (index < queue.length) frame = requestAnimationFrame(next);
          else {
            setImages(previous => Object.fromEntries(Object.entries(previous).filter(([id]) => ids.has(id))));
            endRefresh();
          }
        };
        next();
      });
    }, 150);
    return () => { clearTimeout(timer); cancelAnimationFrame(frame); };
  }, [compositor, state]);
  return images;
}

export function LayerThumbnail({ layer, image, swatch, onPreview }: {
  layer: Layer; image?: string; swatch: string; onPreview: (id: string, anchor: HTMLElement) => void;
}) {
  const timer = useRef<ReturnType<typeof setTimeout> | null>(null);
  const cancel = () => { if (timer.current) clearTimeout(timer.current); timer.current = null; };
  useEffect(() => cancel, []);
  return <button className="layer-thumbnail" aria-label={`Preview ${layer.name}`} title={`Preview ${layer.name} (Space)`}
    style={{ '--swatch': swatch } as CSSProperties}
    onPointerEnter={event => {
      const anchor = event.currentTarget;
      cancel(); timer.current = setTimeout(() => onPreview(layer.id, anchor), 300);
    }}
    onPointerLeave={cancel} onBlur={cancel}
    onClick={event => { event.stopPropagation(); editorStore.getState().select(layer.id, event); }}
    onKeyDown={event => {
      if (event.key !== ' ') return;
      event.preventDefault(); event.stopPropagation(); onPreview(layer.id, event.currentTarget);
    }}>
    {image && <img src={image} alt={`${layer.name} layer thumbnail`} draggable={false} />}
  </button>;
}

export function LayerHoverPreview({ id, anchor, compositor, meta, state, onClose }: {
  id: string; anchor: HTMLElement; compositor: Compositor; meta: ProjectMeta; state: EditorState; onClose: () => void;
}) {
  const layer = state.layers.find(l => l.id === id);
  const [image, setImage] = useState<{ src: string; gain: number } | null>(null), [version, setVersion] = useState(0);
  const frame = viewFrame(meta, state.global);
  const panel = (anchor.closest('.layer-panel') || anchor).getBoundingClientRect();
  const placement = previewPlacement(frame.width, frame.height,
    { width: innerWidth, height: innerHeight }, { left: panel.left, right: panel.right, top: anchor.getBoundingClientRect().top });
  useEffect(() => {
    if (!layer || !compositor.ready) return;
    const frame = requestAnimationFrame(() => {
      const preview = compositor.renderLayerPreview(layer, state, Math.round(500 * Math.min(devicePixelRatio || 1, 2)));
      setImage({ src: preview.canvas.toDataURL(), gain: preview.gain });
    });
    return () => cancelAnimationFrame(frame);
  }, [layer, state, compositor]);
  useEffect(() => {
    const key = (event: KeyboardEvent) => {
      if (event.key === 'Escape') { event.preventDefault(); event.stopPropagation(); onClose(); }
    };
    const leave = (event: PointerEvent) => {
      if (event.target instanceof Node && !anchor.contains(event.target)
        && !(event.target instanceof Element && event.target.closest('.layer-hover-preview'))) onClose();
    };
    const resize = () => setVersion(v => v + 1);
    document.addEventListener('keydown', key, true); document.addEventListener('pointermove', leave);
    window.addEventListener('resize', resize); window.addEventListener('scroll', onClose, true);
    return () => {
      document.removeEventListener('keydown', key, true); document.removeEventListener('pointermove', leave);
      window.removeEventListener('resize', resize); window.removeEventListener('scroll', onClose, true);
    };
  }, [anchor, onClose]);
  if (!layer) return null;
  const source = meta.layers.find(l => l.key === layer.source);
  return createPortal(<div className="layer-hover-preview" role="dialog" aria-label={`${layer.name} preview`}
    data-layout-version={version} style={{ left: placement.left, top: placement.top, width: placement.panelWidth }}
    onPointerLeave={onClose}>
    <div className="layer-preview-image" style={{ width: placement.width, height: placement.height }}>
      {image && <img src={image.src} alt={`${layer.name} enlarged preview`} />}
    </div>
    <div className="layer-preview-caption">
      <strong>{layer.name}</strong><span className="mono">{source?.filter || 'Luminance'} · {source?.wavelength_nm ? waveLabel(source.wavelength_nm) : 'All wavelengths'}</span>
      <span>{source?.info?.shows || source?.info?.line || 'Combined source filters'}</span>
      <small>{layer.blend} · opacity {Math.round(layer.opacity * 100)}%</small>
      {image && <small>Preview auto-exposed ×{image.gain.toFixed(1)}</small>}
    </div>
  </div>, document.body);
}
