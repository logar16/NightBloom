import { Check, FlipHorizontal2, RotateCcw } from 'lucide-react';
import { IconButton, Segmented } from '../../ui';
import { Compositor } from '../../editor/compositor';
import { useEditor } from '../../editor/store';
import type { Tool } from './canvas';
import { changeProperty, PropertySlider } from './properties';
import { MarkerOptions } from './markers';
import { FeatherOptions, type BrushOptions } from './brush';
import { InfoOptions } from './info';

export function ToolOptions({ tool, aspect, setAspect, applyCrop, resetCrop, pending, brush, setBrush, inverted, infoSize, setInfoSize }: {
  tool: Tool; aspect: number; setAspect: (aspect: number) => void;
  applyCrop: () => void; resetCrop: () => void; pending: boolean; compositor: Compositor | null;
  brush: BrushOptions; setBrush: (brush: BrushOptions) => void; inverted: boolean;
  infoSize: number; setInfoSize: (size: number) => void;
}) {
  const state = useEditor(s => s.state)!;
  const selected = useEditor(s => s.selected);
  const layer = state.layers.find(l => l.id === selected.at(-1));
  return <div id="tool-options" className="tool-options">
    {tool === 'info' ? <InfoOptions size={infoSize} onSize={setInfoSize} /> : tool === 'brush' ? <FeatherOptions brush={brush} setBrush={setBrush} inverted={inverted} /> : tool === 'mark' ? <MarkerOptions /> : tool === 'crop' ? <>
      <strong>Crop</strong><div id="crop-aspects"><Segmented value={String(aspect)}
        options={[{ value: '0', label: 'Free' }, { value: '1', label: '1:1' }, { value: '0.8', label: '4:5' },
          { value: '1.7777778', label: '16:9' }, { value: '1.5', label: '3:2' }]}
        onChange={v => setAspect(Number(v))} /></div>
      <IconButton icon={Check} title="Apply crop (Enter)" id="apply-crop" disabled={!pending} onClick={applyCrop} />
      <IconButton icon={RotateCcw} title="Reset crop" id="context-reset-crop" onClick={resetCrop} />
    </> : tool === 'clip' ? <strong>Crop layer · {layer?.name}</strong> : tool === 'rotate' ? <>
      <strong>Rotate</strong>
      <PropertySlider label="Angle" path="global.rotation" value={state.global.rotation} min={-180} max={180} step={.1} />
      <IconButton icon={RotateCcw} title="Reset rotation" id="context-reset-rotate" onClick={() => changeProperty('global.rotation', 0)} />
      <IconButton icon={FlipHorizontal2} title="Flip horizontally" id="context-flip"
        aria-pressed={state.global.flipX} onClick={() => changeProperty('global.flipX', !state.global.flipX)} />
    </> : tool === 'layer' ? <>
      <strong>Move layer</strong><span className="muted">{layer?.name || 'No layer'}</span>
      <IconButton icon={RotateCcw} title="Reset layer transform" id="context-reset-layer"
        onClick={() => changeProperty('transform', { x: 0, y: 0, scale: 1, rotation: 0 })} />
    </> : <strong>Pan</strong>}
  </div>;
}
