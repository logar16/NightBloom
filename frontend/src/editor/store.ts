import { createStore } from 'zustand/vanilla';
import { useStore } from 'zustand';
import {
  adjustRelative, defaultState, reconcileState, snapshot,
  clamp, type Blend, type EditorSnapshot, type EditorState, type Layer, type ProjectMeta, type RelativeKey,
} from './state';

export interface Revision { revision: number; source?: string; note?: string | null }
interface EditorStore {
  sessionId: string | null;
  meta: ProjectMeta | null;
  state: EditorState | null;
  selected: string[];
  history: EditorSnapshot[];
  historyIndex: number;
  knownRevision: number | null;
  toast: string | null;
  dirty: number;
  relativeBaseline: Layer[] | null;
  load: (sessionId: string, meta: ProjectMeta, edits: unknown, revision?: number) => void;
  change: (edit: (state: EditorState) => void, commit?: boolean) => void;
  commit: () => void;
  undo: () => void;
  redo: () => void;
  select: (id: string, modifiers?: { ctrlKey?: boolean; metaKey?: boolean; shiftKey?: boolean }) => void;
  relative: (key: RelativeKey, value: number, commit: boolean) => void;
  toggleVisibility: (id: string) => void;
  duplicateSelection: () => void;
  deleteSelection: () => void;
  absolute: (patch: { blend?: Blend; edge?: Partial<Layer['edge']> }, commit?: boolean) => void;
  createCheckpoint: (name?: string) => void;
  restoreCheckpoint: (id: string) => void;
  renameCheckpoint: (id: string, name: string) => void;
  deleteCheckpoint: (id: string) => void;
  reset: () => void;
  observeRevision: (info: Revision, fetchEdits: () => Promise<unknown>) => Promise<boolean>;
  savedRevision: (revision: number) => void;
  clearToast: () => void;
}

export function createEditorStore() {
  return createStore<EditorStore>((set, get) => {
    function commit() {
      const { state, history, historyIndex } = get();
      if (!state) return;
      const current = snapshot(state);
      if (historyIndex >= 0 && JSON.stringify(history[historyIndex]) === JSON.stringify(current)) return;
      const next = [...history.slice(0, historyIndex + 1), current].slice(-200);
      set({ history: next, historyIndex: next.length - 1 });
    }
    function restore(value: EditorSnapshot, preserveSelection = false) {
      const current = get().state;
      if (!current) return;
      const state = { ...structuredClone(value), checkpoints: current.checkpoints };
      const available = new Set(state.layers.map(l => l.id));
      let selected = preserveSelection ? get().selected.filter(id => available.has(id)) : [];
      if (!selected.length) {
        const fallback = state.layers.find(l => l.id === state.selectedLayer)?.id || state.layers[0]?.id;
        selected = fallback ? [fallback] : [];
      }
      if (preserveSelection) state.selectedLayer = selected.at(-1);
      set({ state, selected, relativeBaseline: null, dirty: get().dirty + 1 });
    }
    return {
      sessionId: null, meta: null, state: null, selected: [], history: [], historyIndex: -1,
      knownRevision: null, toast: null, dirty: 0, relativeBaseline: null,
      load(sessionId, meta, edits, revision) {
        const state = reconcileState(meta, edits);
        const selected = state.layers.find(l => l.id === state.selectedLayer)?.id || state.layers[0]?.id;
        set({
          sessionId, meta, state, selected: selected ? [selected] : [], history: [snapshot(state)],
          historyIndex: 0, knownRevision: revision ?? null, toast: null, dirty: 0, relativeBaseline: null,
        });
      },
      change(edit, committed = true) {
        const current = get().state;
        if (!current) return;
        const state = structuredClone(current);
        edit(state);
        set({ state, dirty: get().dirty + 1 });
        if (committed) commit();
      },
      commit,
      undo() {
        const { historyIndex, history } = get();
        if (historyIndex <= 0) return;
        set({ historyIndex: historyIndex - 1 });
        restore(history[historyIndex - 1], true);
      },
      redo() {
        const { historyIndex, history } = get();
        if (historyIndex >= history.length - 1) return;
        set({ historyIndex: historyIndex + 1 });
        restore(history[historyIndex + 1], true);
      },
      select(id, modifiers = {}) {
        const { state, selected } = get();
        if (!state) return;
        let next = [id];
        const active = selected.at(-1);
        if (modifiers.ctrlKey || modifiers.metaKey || modifiers.shiftKey) {
          const chosen = new Set(selected);
          if (modifiers.shiftKey && active) {
            const ids = state.layers.map(l => l.id), a = ids.indexOf(active), b = ids.indexOf(id);
            ids.slice(Math.min(a, b), Math.max(a, b) + 1).forEach(x => chosen.add(x));
          } else if (chosen.has(id) && chosen.size > 1) chosen.delete(id);
          else chosen.add(id);
          next = [...chosen].filter(x => x !== id || !chosen.has(id));
          if (chosen.has(id)) next.push(id);
        }
        set({ selected: next, relativeBaseline: null, state: { ...state, selectedLayer: next.at(-1) } });
      },
      relative(key, value, committed) {
        const { state, selected, relativeBaseline } = get();
        if (!state) return;
        const baseline = relativeBaseline || state.layers.filter(l => selected.includes(l.id)).map(l => structuredClone(l));
        set({ relativeBaseline: baseline });
        get().change(s => {
          s.layers = s.layers.map(l => {
            const base = baseline.find(b => b.id === l.id);
            return base ? adjustRelative(base, key, value) : l;
          });
        }, committed);
        if (committed || value === 0) set({ relativeBaseline: null });
      },
      toggleVisibility(id) {
        const { state, selected } = get(), clicked = state?.layers.find(l => l.id === id);
        if (!clicked) return;
        const ids = selected.includes(id) ? selected : [id], visible = !clicked.visible;
        get().change(s => { s.layers.forEach(l => { if (ids.includes(l.id)) l.visible = visible; }); });
      },
      duplicateSelection() {
        const { state, selected } = get();
        if (!state) return;
        const originals = state.layers.filter(l => selected.includes(l.id));
        if (!originals.length) return;
        const copies = originals.map(l => ({ ...structuredClone(l), id: crypto.randomUUID(), name: `${l.name} copy` }));
        get().change(s => {
          s.layers.splice(s.layers.findIndex(l => l.id === originals[0].id), 0, ...copies);
          s.selectedLayer = copies.at(-1)!.id;
        }, false);
        set({ selected: copies.map(l => l.id), relativeBaseline: null }); commit();
      },
      deleteSelection() {
        const { state, selected } = get();
        if (!state || !selected.length) return;
        get().change(s => {
          s.layers = s.layers.filter(l => !selected.includes(l.id));
          s.selectedLayer = s.layers[0]?.id;
        }, false);
        const id = get().state?.layers[0]?.id;
        set({ selected: id ? [id] : [], relativeBaseline: null }); commit();
      },
      absolute(patch, committed = true) {
        const selected = get().selected;
        set({ relativeBaseline: null });
        get().change(s => {
          for (const l of s.layers) if (selected.includes(l.id)) {
            if (patch.blend !== undefined) l.blend = patch.blend;
            if (patch.edge) {
              l.edge = { ...l.edge, ...patch.edge };
              l.edge.feather = clamp(l.edge.feather, 0, 400);
              l.edge.irregularity = clamp(l.edge.irregularity, 0, 1);
            }
          }
        }, committed);
      },
      createCheckpoint(name) {
        get().change(s => {
          const created = new Date().toISOString();
          const label = name || `Checkpoint ${(s.checkpoints?.length || 0) + 1} · ${new Date(created).toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' })}`;
          s.checkpoints = [...s.checkpoints || [], { id: crypto.randomUUID(), name: label, created, state: snapshot(s) }];
        });
      },
      restoreCheckpoint(id) {
        const point = get().state?.checkpoints?.find(c => c.id === id);
        if (!point) return;
        const meta = get().meta;
        const restored = meta ? reconcileState(meta, point.state) : structuredClone(point.state);
        // Upgrade old snapshots without resurrecting layers intentionally deleted at checkpoint time.
        const ids = new Set(point.state.layers.map(l => l.id));
        restored.layers = restored.layers.filter(l => ids.has(l.id));
        restore(restored); commit();
        set({ toast: 'Checkpoint restored' });
      },
      renameCheckpoint(id, name) {
        if (!name.trim()) return;
        get().change(s => { const c = s.checkpoints?.find(c => c.id === id); if (c) c.name = name.trim(); });
      },
      deleteCheckpoint(id) { get().change(s => { s.checkpoints = s.checkpoints?.filter(c => c.id !== id); }); },
      reset() { const meta = get().meta; if (meta) { restore(defaultState(meta)); commit(); } },
      async observeRevision(info, fetchEdits) {
        const { knownRevision, sessionId, meta } = get();
        if (knownRevision === null || !meta) { set({ knownRevision: info.revision }); return false; }
        if (info.revision <= knownRevision) return false;
        set({ knownRevision: info.revision });
        if (info.source !== 'assistant') return false;
        let edits: unknown;
        try { edits = await fetchEdits(); }
        catch (error) {
          // Un-see this revision so the next status poll retries instead of silently skipping it.
          if (get().knownRevision === info.revision && get().sessionId === sessionId) set({ knownRevision });
          throw error;
        }
        if (!edits || get().sessionId !== sessionId || get().meta !== meta || get().knownRevision !== info.revision) return false;
        restore(reconcileState(meta, edits)); commit();
        set({ toast: info.note ? `Assistant · ${info.note}` : 'Assistant updated the edits' });
        return true;
      },
      savedRevision(revision) { set({ knownRevision: Math.max(get().knownRevision ?? 0, revision) }); },
      clearToast() { set({ toast: null }); },
    };
  });
}
export const editorStore = createEditorStore();
export const useEditor = <T,>(selector: (store: EditorStore) => T) => useStore(editorStore, selector);
