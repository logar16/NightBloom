import { describe, expect, it } from 'vitest';
import { createEditorStore } from './store';
import { defaultOutput, defaultState, reconcileState, type ProjectMeta } from './state';

const meta: ProjectMeta = {
  width: 8, height: 6,
  layers: [
    { key: 'blue', filter: 'F502N', wavelength_nm: 502, file: 'blue.f16', starless: 'blue.starless.f16' },
    { key: 'red', filter: 'F673N', wavelength_nm: 673, file: 'red.f16', starless: 'red.starless.f16' },
  ],
  stars: { file: 'stars.u16', list: 'stars.json' },
};
function loaded() {
  const store = createEditorStore();
  store.getState().load('m16', meta, null, 1);
  return store;
}
it('adds neutral master tone to legacy edits and preserves manual tone through undo, save and reset', () => {
  const legacy = defaultState(meta);
  legacy.global.output = { exposure: .4, highlights: .2 };
  const store = createEditorStore();
  store.getState().load('legacy', meta, legacy);
  const actions = store.getState(), neutral = { ...defaultOutput, exposure: .4, highlights: .2 };
  expect(store.getState().state!.global.output).toEqual(neutral);
  actions.change(s => { Object.assign(s.global.output, { black: .08, white: .95, midtones: .7 }); });
  const toned = { ...neutral, black: .08, white: .95, midtones: .7 };
  expect(reconcileState(meta, JSON.parse(JSON.stringify(store.getState().state))).global.output).toEqual(toned);
  actions.undo(); expect(store.getState().state!.global.output).toEqual(neutral);
  actions.redo(); expect(store.getState().state!.global.output).toEqual(toned);
  actions.reset(); expect(store.getState().state!.global.output).toEqual(defaultOutput);
});
it('preserves crop anchors through undo/redo, checkpoints, and JSON save/load; reset removes the anchor', () => {
  const store = loaded(), actions = store.getState();
  const crop = { x: .1, y: .2, w: .5, h: .4, rotation: 23 };
  actions.change(s => { s.global.crop = crop; s.global.rotation = 60; });
  actions.createCheckpoint('Rotated crop');
  const checkpoint = store.getState().state!.checkpoints![0];
  actions.change(s => { s.global.crop = { x: 0, y: 0, w: 1, h: 1 }; });
  expect(store.getState().state!.global.crop.rotation).toBeUndefined();
  actions.undo(); expect(store.getState().state!.global.crop).toEqual(crop);
  actions.redo(); expect(store.getState().state!.global.crop.rotation).toBeUndefined();
  actions.restoreCheckpoint(checkpoint.id);
  const saved = JSON.parse(JSON.stringify(store.getState().state));
  expect(reconcileState(meta, saved).global).toMatchObject({ crop, rotation: 60 });
});
it('records a multi-layer brush stroke as one undo step, including clear and checkpoint restore', () => {
  const store = loaded(), actions = store.getState(), ids = store.getState().state!.layers.map(l => l.id);
  actions.select(ids[0]); actions.select(ids[1], { ctrlKey: true });
  const before = store.getState().history.length;
  actions.change(s => { for (const layer of s.layers) layer.brush.strokes.push({
    mode: 'hide', size: 40, hardness: .5, strength: .8, points: [[1.5, 2]],
  }); });
  expect(store.getState().history).toHaveLength(before + 1);
  actions.undo(); expect(store.getState().state!.layers.every(l => !l.brush.strokes.length)).toBe(true);
  expect(store.getState().selected).toEqual(ids);
  actions.redo(); expect(store.getState().state!.layers.every(l => l.brush.strokes.length === 1)).toBe(true);
  expect(store.getState().selected).toEqual(ids);
  actions.createCheckpoint('Paint');
  const checkpoint = store.getState().state!.checkpoints![0];
  actions.change(s => { s.layers[0].brush.strokes = []; });
  actions.undo(); expect(store.getState().state!.layers[0].brush.strokes).toHaveLength(1);
  actions.restoreCheckpoint(checkpoint.id);
  expect(store.getState().state!.layers[0].brush.strokes).toHaveLength(1);
  const legacy = structuredClone(store.getState().state!) as unknown as { layers: { brush?: unknown }[] };
  delete legacy.layers[0].brush;
  expect(reconcileState(meta, legacy).layers[0].brush).toEqual({ strokes: [] });
});
it('keeps the currently selected layer through cross-layer brush undo/redo and subsequent painting', () => {
  const store = loaded(), actions = store.getState(), ids = store.getState().state!.layers.map(l => l.id);
  const paint = () => actions.change(s => {
    for (const layer of s.layers) if (store.getState().selected.includes(layer.id)) layer.brush.strokes.push({
      mode: 'white', size: 40, hardness: .5, strength: .8, points: [[1.5, 2]],
    });
  });
  actions.select(ids[0]); paint();
  actions.select(ids[1]); paint();
  actions.undo();
  expect(store.getState().state!.layers.map(l => l.brush.strokes.length)).toEqual([1, 0]);
  expect(store.getState().selected).toEqual([ids[1]]);
  expect(store.getState().state!.selectedLayer).toBe(ids[1]);
  actions.redo();
  expect(store.getState().selected).toEqual([ids[1]]);
  actions.undo(); paint();
  expect(store.getState().state!.layers.map(l => l.brush.strokes.length)).toEqual([1, 1]);
  expect(store.getState().state!.selectedLayer).toBe(ids[1]);
});
it('uses a valid restored selection when undo removes the active layer', () => {
  const store = loaded(), actions = store.getState(), original = store.getState().selected[0];
  actions.duplicateSelection();
  const copy = store.getState().selected[0];
  actions.undo();
  expect(store.getState().selected).toEqual([original]);
  expect(store.getState().state!.selectedLayer).toBe(original);
  actions.redo();
  actions.select(copy);
  actions.deleteSelection();
  actions.undo();
  expect(store.getState().selected.every(id => store.getState().state!.layers.some(l => l.id === id))).toBe(true);
});
describe('reconcileState', () => {
  it('collapses tile edits onto a mosaic without resurrecting deleted source layers', () => {
    const old = defaultState(meta);
    old.layers[0].color = '#123456';
    const grouped: ProjectMeta = { ...meta, layers: [{ ...meta.layers[0], key: 'mosaic',
      tiles: meta.layers.map(l => ({ key: l.key, gain: 1, offset: 0 })) }] };
    const next = reconcileState(grouped, old);
    expect(next.layers).toHaveLength(1);
    expect(next.layers[0]).toMatchObject({ id: old.layers[0].id, color: '#123456', source: 'mosaic' });
    expect(next.sources).toEqual(['mosaic']);
    next.layers = [];
    expect(reconcileState(grouped, next).layers).toEqual([]);
    next.layers.push({ ...old.layers[0], source: 'lum' });
    expect(reconcileState(grouped, next).layers.map(l => l.source)).toEqual(['lum']);
    expect(reconcileState(grouped, reconcileState(grouped, old))).toEqual(reconcileState(grouped, old));
  });
  it('loads legacy layer tools as no-ops and keeps edits undoable', () => {
    const old = defaultState(meta);
    for (const layer of old.layers) {
      const partial = layer as Partial<typeof layer>;
      delete partial.invert; delete partial.curve; delete partial.detail; delete partial.clip;
    }
    const reconciled = reconcileState(meta, old), layer = reconciled.layers[0];
    expect(layer).toMatchObject({ invert: false, curve: [], detail: { amount: 0, radius: 8 }, clip: null });
    const store = createEditorStore();
    store.getState().load('old', meta, old);
    store.getState().change(s => { s.layers[0].invert = true; s.layers[0].curve = [{ x: .4, y: .7 }]; });
    store.getState().undo();
    expect(store.getState().state!.layers[0]).toEqual(layer);
  });
  it('ignores abandoned star state in old projects and checkpoints, idempotently', () => {
    // Design decision: keep ordinary edits, not the abandoned star-only look.
    const legacy = { ...defaultState(meta), global: { ...defaultState(meta).global, stars: { fg: 1.4, bg: .3 } },
      starClasses: { 1: false }, starOverrides: { 1: { gain: 0 } }, starWeights: { blue: 0 } };
    const state = reconcileState(meta, { ...legacy, checkpoints: [{ id: 'saved', name: 'Saved', created: '', state: legacy }] });
    expect(state).not.toHaveProperty('starClasses');
    expect(state).not.toHaveProperty('starOverrides');
    expect(state).not.toHaveProperty('starWeights');
    expect(state.global).not.toHaveProperty('stars');
    expect(state.checkpoints![0].state.global).not.toHaveProperty('stars');
    expect(state.layers).toEqual(legacy.layers);
    expect(reconcileState(meta, JSON.parse(JSON.stringify(state)))).toEqual(state);
    expect(defaultState({ width: 2, height: 2, layers: [] }).global).not.toHaveProperty('stars');
  });
  it('keeps edits and luminance, removes obsolete sources, adds new filters with nested defaults', () => {
    const edits = defaultState(meta);
    const kept = edits.layers[0];
    kept.intensity = .63;
    const legacy = {
      ...edits, sources: undefined, global: { ...edits.global, output: undefined, parallax: { on: true } },
      layers: [
        { ...kept, transform: { x: .2 }, mask: { source: 'blue' }, edge: { feather: 35 } },
        { ...edits.layers[1], source: 'removed' },
        { ...edits.layers[1], id: 'lum', source: 'lum' },
      ],
    };
    const state = reconcileState(meta, legacy);
    expect(state.layers.map(l => l.source)).toEqual(['blue', 'red', 'lum']);
    expect(state.layers[1].intensity).toBe(.63);
    expect(state.layers[1].transform).toEqual({ x: .2, y: 0, scale: 1, rotation: 0 });
    expect(state.layers[1].mask).toEqual({ source: 'blue', black: 0, white: 1, invert: false });
    expect(state.layers[1].edge).toEqual({ feather: 35, irregularity: 0 });
    expect(state.layers[0].edge).toEqual({ feather: 0, irregularity: 0 });
    expect(state.global.output).toEqual(defaultOutput);
    expect(state.global).not.toHaveProperty('parallax');
  });
  it('pins pre-existing crops at their saved rotation so later rotation turns the whole crop', () => {
    const edits = defaultState(meta);
    edits.global.rotation = -3;
    edits.global.crop = { x: .3, y: .3, w: .4, h: .4 };
    expect(reconcileState(meta, edits).global.crop.rotation).toBe(-3);
    edits.global.crop = { x: 0, y: 0, w: 1, h: 1 };
    expect(reconcileState(meta, edits).global.crop.rotation).toBeUndefined();
    edits.global.crop = { x: .3, y: .3, w: .4, h: .4, rotation: 10 };
    expect(reconcileState(meta, edits).global.crop.rotation).toBe(10);
  });
  it('keeps deleted layers deleted on reload, but adds sources from a later build', () => {
    const store = loaded(), actions = store.getState();
    actions.select(store.getState().state!.layers.find(l => l.source === 'blue')!.id);
    actions.deleteSelection();
    const saved = JSON.parse(JSON.stringify(store.getState().state));
    expect(reconcileState(meta, saved).layers.map(l => l.source)).toEqual(['red']);
    const rebuilt = { ...meta, layers: [...meta.layers,
      { key: 'ir', filter: 'F200W', wavelength_nm: 2000, file: 'ir.f16', starless: 'ir.starless.f16' }] };
    const next = reconcileState(rebuilt, saved);
    expect(next.layers.map(l => l.source)).toEqual(['ir', 'red']);
    expect(next.sources).toEqual(['blue', 'red', 'ir']);
  });
  it('preserves an empty composition, globals and checkpoints after deleting every layer and reloading', () => {
    const store = loaded(), actions = store.getState();
    actions.change(s => {
      s.global.crop = { x: .1, y: .2, w: .5, h: .4, rotation: 23 };
      s.global.rotation = 60;
      s.global.output = { exposure: 1.5, highlights: .3 };
    });
    const ids = store.getState().state!.layers.map(layer => layer.id);
    actions.select(ids[0]); actions.select(ids[1], { ctrlKey: true });
    actions.deleteSelection();
    expect(store.getState().state!.layers).toEqual([]);
    actions.createCheckpoint('Empty canvas');
    const saved = JSON.parse(JSON.stringify(store.getState().state));
    const reopened = createEditorStore();
    reopened.getState().load('m16', meta, saved, 2);
    expect(reopened.getState().state).toMatchObject({
      layers: [], global: saved.global, sources: saved.sources, checkpoints: saved.checkpoints,
    });
    reopened.getState().reset();
    reopened.getState().restoreCheckpoint(saved.checkpoints[0].id);
    expect(reopened.getState().state).toMatchObject({ layers: [], global: saved.global });
    const rebuilt = { ...meta, layers: [...meta.layers,
      { key: 'ir', filter: 'F200W', wavelength_nm: 2000, file: 'ir.f16', starless: 'ir.starless.f16' }] };
    expect(reconcileState(rebuilt, saved).layers.map(layer => layer.source)).toEqual(['ir']);
  });
  describe('bulk layer operations', () => {
    it('sets every selected eye to the clicked target state, without affecting unselected rows', () => {
      const store = loaded(), actions = store.getState();
      actions.change(s => { s.layers.push({ ...structuredClone(s.layers[0]), id: 'outside', visible: false }); });
      const ids = store.getState().state!.layers.map(l => l.id);
      actions.select(ids[0]); actions.select(ids[1], { ctrlKey: true });
      const before = store.getState().history.length;
      actions.toggleVisibility(ids[0]);
      expect(store.getState().state!.layers.map(l => l.visible)).toEqual([false, false, false]);
      expect(store.getState().history).toHaveLength(before + 1);
      actions.undo();
      expect(store.getState().state!.layers.map(l => l.visible)).toEqual([true, true, false]);
      actions.select(ids[0]); actions.select(ids[1], { shiftKey: true });
      actions.toggleVisibility('outside');
      expect(store.getState().state!.layers.map(l => l.visible)).toEqual([true, true, true]);
      expect(store.getState().selected).toHaveLength(2);
    });
    it('duplicates a selection in visual order above its originals and deletes it in one undo step', () => {
      const store = loaded(), actions = store.getState();
      actions.change(s => { s.layers.push({ ...structuredClone(s.layers[0]), id: 'third', name: 'Third' }); });
      const originals = structuredClone(store.getState().state!.layers), ids = originals.map(l => l.id);
      actions.select(ids[2]); actions.select(ids[0], { ctrlKey: true });
      const before = store.getState().history.length;
      actions.duplicateSelection();
      const after = store.getState().state!.layers, copies = after.slice(0, 2);
      expect(copies.map(l => l.name)).toEqual([`${originals[0].name} copy`, 'Third copy']);
      expect(after.slice(2)).toEqual(originals);
      expect(store.getState().selected).toEqual(copies.map(l => l.id));
      expect(store.getState().history).toHaveLength(before + 1);
      actions.deleteSelection();
      expect(store.getState().state!.layers).toEqual(originals);
      expect(store.getState().history).toHaveLength(before + 2);
      actions.undo(); expect(store.getState().state!.layers).toEqual(after);
      actions.undo(); expect(store.getState().state!.layers).toEqual(originals);
    });
    it('applies absolute blend and edge settings to all selected layers, committing sliders once', () => {
      const store = loaded(), actions = store.getState(), ids = store.getState().state!.layers.map(l => l.id);
      actions.select(ids[0]); actions.select(ids[1], { ctrlKey: true });
      actions.absolute({ edge: { feather: 20 } }, false);
      actions.absolute({ edge: { feather: 60 } }, false);
      actions.absolute({ edge: { feather: 60 } });
      expect(store.getState().history).toHaveLength(2);
      expect(store.getState().state!.layers.map(l => l.edge)).toEqual([
        { feather: 60, irregularity: 0 }, { feather: 60, irregularity: 0 },
      ]);
      actions.absolute({ blend: 'lighten', edge: { irregularity: .7 } });
      expect(store.getState().state!.layers.every(l => l.blend === 'lighten' && l.edge.irregularity === .7)).toBe(true);
      actions.undo();
      expect(store.getState().state!.layers.every(l => l.blend === 'screen' && l.edge.feather === 60 && !l.edge.irregularity)).toBe(true);
      actions.undo();
      expect(store.getState().state!.layers.every(l => !l.edge.feather)).toBe(true);
    });
  });
});
describe('history and checkpoints', () => {
  it('commits a whole gesture once, branches after undo, and caps at 200 states', () => {
    const store = loaded(), actions = store.getState();
    actions.change(s => { s.global.rotation = 5; }, false);
    actions.change(s => { s.global.rotation = 30; }, false);
    actions.commit();
    expect(store.getState().history).toHaveLength(2);
    actions.undo(); expect(store.getState().state?.global.rotation).toBe(0);
    actions.redo(); expect(store.getState().state?.global.rotation).toBe(30);
    actions.undo();
    actions.change(s => { s.global.rotation = 10; });
    actions.redo(); expect(store.getState().state?.global.rotation).toBe(10);
    for (let i = 0; i < 205; i++) actions.change(s => { s.global.rotation = i; });
    expect(store.getState().history).toHaveLength(200);
    expect(store.getState().historyIndex).toBe(199);
  });
  it('restores checkpoints undoably without recursive checkpoint snapshots', () => {
    const store = loaded(), actions = store.getState();
    actions.createCheckpoint('Original');
    const checkpoint = store.getState().state!.checkpoints![0];
    expect(checkpoint.state).not.toHaveProperty('checkpoints');
    actions.change(s => { Reflect.deleteProperty(s.checkpoints![0].state.global, 'output'); });
    actions.change(s => { s.global.rotation = 30; });
    actions.restoreCheckpoint(checkpoint.id);
    expect(store.getState().state?.global.rotation).toBe(0);
    expect(store.getState().state?.global.output).toEqual(defaultOutput);
    actions.undo();
    expect(store.getState().state?.global.rotation).toBe(30);
    expect(store.getState().state?.checkpoints).toHaveLength(1);
  });
  it('preserves intentional layer deletions when restoring a checkpoint', () => {
    const store = loaded(), actions = store.getState();
    actions.change(s => { s.layers.pop(); });
    actions.createCheckpoint('One layer');
    const checkpoint = store.getState().state!.checkpoints![0];
    actions.reset();
    expect(store.getState().state!.layers).toHaveLength(2);
    actions.restoreCheckpoint(checkpoint.id);
    expect(store.getState().state!.layers.map(l => l.id)).toEqual(checkpoint.state.layers.map(l => l.id));
  });
});
describe('multi-layer adjustments', () => {
  it('uses each layer’s baseline proportionally, not cumulative live input values', () => {
    const store = loaded(), actions = store.getState(), ids = store.getState().state!.layers.map(l => l.id);
    actions.change(s => { s.layers[0].intensity = .4; s.layers[1].intensity = .8; });
    actions.select(ids[0]); actions.select(ids[1], { ctrlKey: true });
    actions.relative('intensity', 10, false);
    actions.relative('intensity', 25, false);
    actions.relative('intensity', 25, true);
    expect(store.getState().state?.layers.map(l => l.intensity)).toEqual([.5, 1]);
    actions.relative('intensity', 20, true);
    expect(store.getState().state?.layers.map(l => l.intensity)).toEqual([.6, 1.2]);
    actions.undo();
    expect(store.getState().state?.layers.map(l => l.intensity)).toEqual([.5, 1]);
  });
  it('adds black offsets, bounds against each white point, and shift-selects a range', () => {
    const store = loaded(), actions = store.getState(), ids = store.getState().state!.layers.map(l => l.id);
    actions.change(s => { s.layers[0].black = .2; s.layers[0].white = .3; });
    actions.select(ids[0]); actions.select(ids[1], { shiftKey: true });
    actions.relative('black', .4, true);
    expect(store.getState().selected).toHaveLength(2);
    expect(store.getState().state!.layers[0].black).toBeCloseTo(.29);
    expect(store.getState().state!.layers[1].black).toBe(.4);
  });
  it('releases a canceled gesture baseline so subsequent edits are not overwritten', () => {
    const store = loaded(), actions = store.getState(), ids = store.getState().state!.layers.map(l => l.id);
    actions.select(ids[0]); actions.select(ids[1], { ctrlKey: true });
    actions.relative('intensity', 20, false);
    actions.relative('intensity', 0, false);
    actions.change(s => { s.layers[0].color = '#123456'; });
    actions.relative('white', 10, true);
    expect(store.getState().state!.layers[0].color).toBe('#123456');
  });
});
describe('assistant revision sync', () => {
  it('adopts only newer assistant revisions, toasts, and undoes adoption', async () => {
    const store = loaded(), actions = store.getState(), edits = structuredClone(store.getState().state!);
    edits.global.output.exposure = -.7;
    const fetchEdits = async () => edits;
    expect(await actions.observeRevision({ revision: 2, source: 'ui' }, fetchEdits)).toBe(false);
    expect(await actions.observeRevision({ revision: 3, source: 'assistant', note: 'Highlights' }, fetchEdits)).toBe(true);
    expect(store.getState().toast).toBe('Assistant · Highlights');
    expect(store.getState().state?.global.output.exposure).toBe(-.7);
    expect(await actions.observeRevision({ revision: 3, source: 'assistant' }, fetchEdits)).toBe(false);
    actions.undo(); expect(store.getState().state?.global.output.exposure).toBe(0);
    actions.redo(); expect(store.getState().state?.global.output.exposure).toBe(-.7);
  });
  it('resets revision tracking per session and discards in-flight results after a switch', async () => {
    const store = loaded(), actions = store.getState();
    let finish!: (value: unknown) => void;
    const pending = actions.observeRevision({ revision: 20, source: 'assistant' },
      () => new Promise(resolve => { finish = resolve; }));
    actions.load('jwst', meta, null, 1);
    finish(defaultState(meta));
    expect(await pending).toBe(false);
    expect(store.getState().history).toHaveLength(1);
    expect(store.getState().knownRevision).toBe(1);
    const edits = defaultState(meta); edits.global.rotation = 30;
    expect(await actions.observeRevision({ revision: 2, source: 'assistant' }, async () => edits)).toBe(true);
  });
  it('retries an assistant revision whose fetch failed', async () => {
    const store = loaded(), actions = store.getState();
    await actions.observeRevision({ revision: 1, source: 'ui' }, async () => null);
    await expect(actions.observeRevision({ revision: 2, source: 'assistant' },
      async () => { throw new Error('offline'); })).rejects.toThrow('offline');
    const edits = defaultState(meta); edits.global.rotation = 12;
    expect(await actions.observeRevision({ revision: 2, source: 'assistant' }, async () => edits)).toBe(true);
    expect(store.getState().state?.global.rotation).toBe(12);
  });
});
