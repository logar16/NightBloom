import { Eraser, ScanEye } from 'lucide-react';
import { IconButton, Segmented, Slider } from '../../ui';
import { editorStore, useEditor } from '../../editor/store';
import type { Stroke } from '../../editor/state';
import { invertBrushMode } from '../../editor/brush';

export interface BrushOptions {
  mode: Stroke['mode']; size: number; hardness: number; strength: number; showMask: boolean;
}
export const defaultBrush: BrushOptions = { mode: 'hide', size: 180, hardness: .25, strength: .8, showMask: false };
export function FeatherOptions({ brush, setBrush, inverted }: {
  brush: BrushOptions; setBrush: (value: BrushOptions) => void; inverted: boolean;
}) {
  const state = useEditor(s => s.state)!, selected = useEditor(s => s.selected);
  const active = state.layers.find(l => l.id === selected.at(-1));
  return <div className="brush-options">
    <Segmented label="Brush mode" value={inverted ? invertBrushMode(brush.mode) : brush.mode}
      options={[{ value: 'hide', label: 'Hide', title: 'Hide · hold Alt to reveal' },
        { value: 'reveal', label: 'Reveal', title: 'Reveal · hold Alt to hide' },
        { value: 'black', label: 'Black', title: 'Paint the monochrome source toward black · Alt: white' },
        { value: 'white', label: 'White', title: 'Paint the monochrome source toward white · Alt: black' }]}
      onChange={mode => setBrush({ ...brush, mode: inverted ? invertBrushMode(mode) : mode })} />
    <Slider label="Size" value={brush.size} min={1} max={1600} step={1} unit="px" log
      onInput={size => setBrush({ ...brush, size })} />
    <Slider label="Hardness" value={brush.hardness} min={0} max={1}
      onInput={hardness => setBrush({ ...brush, hardness })} />
    <Slider label="Strength" value={brush.strength} min={0} max={1}
      onInput={strength => setBrush({ ...brush, strength })} />
    <IconButton icon={ScanEye} title="Show mask" className={brush.showMask ? 'active' : ''} aria-pressed={brush.showMask}
      onClick={() => setBrush({ ...brush, showMask: !brush.showMask })} />
    <IconButton icon={Eraser} className="brush-clear" title="Clear brush" disabled={!active?.brush.strokes.length}
      onClick={() => editorStore.getState().change(s => {
        const layer = s.layers.find(l => l.id === active?.id);
        if (layer) layer.brush = { strokes: [] };
      })} />
  </div>;
}
