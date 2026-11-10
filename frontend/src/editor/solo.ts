import { createStore } from 'zustand/vanilla';
import { useStore } from 'zustand';

interface SoloState {
  latched: string[] | null;
  momentary: string[] | null;
  latch: (row: string, selection: string[]) => void;
  hold: (selection: string[]) => void;
  release: () => void;
  exit: () => void;
}
export function createSoloStore() {
  return createStore<SoloState>((set, get) => ({
    latched: null, momentary: null,
    latch(row, selection) {
      const ids = selection.includes(row) ? [...selection] : [row], previous = get().latched;
      set({ latched: previous?.length === ids.length && ids.every(id => previous.includes(id)) ? null : ids });
    },
    hold(selection) { if (!get().momentary && selection.length) set({ momentary: [...selection] }); },
    release() { set({ momentary: null }); },
    exit() { set({ latched: null, momentary: null }); },
  }));
}
export const soloStore = createSoloStore();
export const useSolo = () => useStore(soloStore, s => s.momentary ?? s.latched);
export function soloVisible(id: string, visible: boolean, solo: string[] | null) {
  return solo ? solo.includes(id) : visible;
}
