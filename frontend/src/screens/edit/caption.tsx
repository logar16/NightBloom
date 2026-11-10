import { useEffect, useRef, useState } from 'react';
import { Copy, X } from 'lucide-react';
import { IconButton } from '../../ui';
import { toast } from '../../api/hooks';
import { rgb } from '../../editor/compositor';
import type { EditorState, ProjectMeta } from '../../editor/state';

function colorName(hex: string) {
  const colors: Record<string, string> = {
    Red: '#e64b45', Orange: '#e89252', Gold: '#e4c66c', Green: '#6ec879', Cyan: '#5fbfc2',
    Blue: '#527fe1', Violet: '#a172cb', Magenta: '#d259a4', White: '#e0dcd1', Gray: '#858585', Black: '#151515',
  };
  const target = rgb(hex);
  const distance = (name: string) => rgb(colors[name]).reduce((sum, value, i) => sum + (value - target[i]) ** 2, 0);
  return Object.keys(colors).sort((a, b) => distance(a) - distance(b))[0];
}
export function makeCaption(state: EditorState, meta: ProjectMeta) {
  return state.layers.filter(l => l.visible).map(l => {
    const source = meta.layers.find(s => s.key === l.source), info = source?.info;
    const color = l.mode === 'tint' ? `${colorName(l.color)} (${l.color})`
      : l.mode === 'spectral' ? 'Wavelength gradient' : 'Intensity gradient';
    return `${color}: ${info?.shows || source?.filter || 'combined luminance'}${info?.line ? ` (${info.line})` : ''} · ${l.name}; ${l.blend}, ${Math.round(l.opacity * 100)}%.`;
  }).join('\n');
}
export function CaptionDialog({ caption, onClose }: { caption: string; onClose: () => void }) {
  const dialog = useRef<HTMLDialogElement>(null), [text, setText] = useState(caption);
  useEffect(() => { dialog.current?.showModal(); }, []);
  return <dialog id="caption-dialog" ref={dialog} onClose={onClose}>
    <div className="dialog-bar"><h2>Color caption</h2><IconButton icon={X} title="Close caption"
      id="caption-close" onClick={() => dialog.current?.close()} /></div>
    <textarea id="caption-text" aria-label="Color caption" rows={6} value={text} onChange={e => setText(e.target.value)} />
    <button className="action" id="copy-caption" onClick={() => {
      void navigator.clipboard.writeText(text).then(() => toast('Caption copied')).catch((e: Error) => toast(e.message));
    }}><Copy />Copy</button>
  </dialog>;
}
