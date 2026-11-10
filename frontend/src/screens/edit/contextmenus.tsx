import { ArrowDown, ArrowDownToLine, ArrowUp, ArrowUpToLine, Blend, Copy, Eye, EyeOff, Focus, Pencil, RotateCcw, Trash2 } from 'lucide-react';
import { ContextMenu, type MenuEntry, type MenuPosition } from '../../ui/ContextMenu';
import { BLENDS, type Layer } from '../../editor/state';
import { editorStore, useEditor } from '../../editor/store';
import { soloStore } from '../../editor/solo';

export function moveSelection(direction: 'top' | 'up' | 'down' | 'bottom') {
  const { selected, change } = editorStore.getState();
  change(state => {
    const chosen = new Set(selected), layers = state.layers;
    if (direction === 'top' || direction === 'bottom') {
      const moving = layers.filter(l => chosen.has(l.id)), rest = layers.filter(l => !chosen.has(l.id));
      state.layers = direction === 'top' ? [...moving, ...rest] : [...rest, ...moving];
    } else if (direction === 'up') {
      for (let i = 1; i < layers.length; i++) {
        if (chosen.has(layers[i].id) && !chosen.has(layers[i - 1].id)) [layers[i - 1], layers[i]] = [layers[i], layers[i - 1]];
      }
    } else {
      for (let i = layers.length - 2; i >= 0; i--) {
        if (chosen.has(layers[i].id) && !chosen.has(layers[i + 1].id)) [layers[i], layers[i + 1]] = [layers[i + 1], layers[i]];
      }
    }
  });
}

export function LayerContextMenu({ position, target, onRename, onClose }: {
  position: MenuPosition; target: string; onRename: (layer: Layer) => void; onClose: () => void;
}) {
  const state = useEditor(s => s.state)!, selected = useEditor(s => s.selected);
  const layer = state.layers.find(l => l.id === target);
  if (!layer) return null;
  const actions = editorStore.getState(), ids = new Set(selected);
  const othersHidden = state.layers.every(l => ids.has(l.id) || !l.visible);
  const canUp = state.layers.some((l, i) => ids.has(l.id) && i > 0 && !ids.has(state.layers[i - 1].id));
  const canDown = state.layers.some((l, i) => ids.has(l.id) && i < state.layers.length - 1 && !ids.has(state.layers[i + 1].id));
  const items: MenuEntry[] = [
    { label: 'Rename', icon: Pencil, shortcut: 'F2', disabled: selected.length !== 1, action: () => onRename(layer) },
    { label: 'Duplicate', icon: Copy, shortcut: 'Ctrl+D', action: actions.duplicateSelection },
    { label: 'Delete', icon: Trash2, shortcut: 'Del', danger: true, action: actions.deleteSelection },
    'separator',
    { label: layer.visible ? 'Hide' : 'Show', icon: layer.visible ? EyeOff : Eye, action: () => actions.toggleVisibility(target) },
    { label: othersHidden ? 'Show all' : 'Hide others', icon: Eye, action: () => actions.change(s => {
      s.layers.forEach(l => { l.visible = othersHidden || ids.has(l.id); });
    }) },
    { label: 'Solo preview', icon: Focus, shortcut: 'Q', action: () => soloStore.getState().latch(target, selected) },
    'separator',
    { label: 'Move to top', icon: ArrowUpToLine, disabled: !canUp, action: () => moveSelection('top') },
    { label: 'Move up', icon: ArrowUp, disabled: !canUp, action: () => moveSelection('up') },
    { label: 'Move down', icon: ArrowDown, disabled: !canDown, action: () => moveSelection('down') },
    { label: 'Move to bottom', icon: ArrowDownToLine, disabled: !canDown, action: () => moveSelection('bottom') },
    'separator',
    { label: 'Blend', icon: Blend, children: BLENDS.map(blend => ({
      label: `${blend === layer.blend ? '✓ ' : ''}${blend === 'softlight' ? 'Soft light' : blend[0].toUpperCase() + blend.slice(1)}`,
      action: () => actions.absolute({ blend }),
    })) },
    { label: 'Reset transform', icon: RotateCcw, action: () => actions.change(s => {
      s.layers.forEach(l => { if (ids.has(l.id)) l.transform = { x: 0, y: 0, scale: 1, rotation: 0 }; });
    }) },
  ];
  return <ContextMenu label="Layer actions" position={position} items={items} onClose={onClose} />;
}
