import { createStore } from 'zustand/vanilla';

interface MarkerBase { id: string; n: number; note?: string; created: string }
export interface PointMarker extends MarkerBase { kind: 'point'; x: number; y: number; radius: number }
export interface RectMarker extends MarkerBase { kind: 'rect'; x1: number; y1: number; x2: number; y2: number }
export type Marker = PointMarker | RectMarker;
export interface MarkerTransport {
  read: (sessionId: string) => Promise<Marker[]>;
  write: (markers: Marker[], sessionId?: string) => Promise<unknown>;
}
export interface MarkerNumbers {
  get: (sessionId: string) => number;
  set: (sessionId: string, number: number) => void;
}
interface MarkerState {
  sessionId: string | null;
  items: Marker[];
  ready: boolean;
  error: string | null;
  nextNumber: number;
  load: (sessionId: string) => Promise<void>;
  point: (x: number, y: number) => void;
  rect: (x1: number, y1: number, x2: number, y2: number) => void;
  note: (id: string, note: string) => void;
  remove: (id: string) => void;
  clear: () => void;
  flush: (force?: boolean) => Promise<void>;
  pause: () => void;
  resume: () => void;
}
export function markerCoordinates(marker: Marker) {
  return marker.kind === 'point' ? `point ${marker.x},${marker.y} r=${marker.radius}`
    : `rect ${marker.x1},${marker.y1} ${marker.x2},${marker.y2}`;
}
export function markerText(markers: Marker[]) {
  return markers.map(marker => `#${marker.n} ${markerCoordinates(marker)}${marker.note ? ` · ${marker.note}` : ''}`).join('\n');
}
export function createMarkerStore(transport: MarkerTransport, numbers?: MarkerNumbers) {
  const remembered = new Map<string, number>();
  let timer: ReturnType<typeof setTimeout> | null = null;
  let writing: Promise<unknown> | null = null;
  let generation = 0, paused = false, saved = '[]';
  return createStore<MarkerState>((set, get) => {
    function remember(sessionId: string, n: number) {
      remembered.set(sessionId, n); numbers?.set(sessionId, n);
    }
    function schedule(items: Marker[]) {
      set({ items, error: null });
      if (timer) clearTimeout(timer);
      if (!paused) timer = setTimeout(() => { void get().flush().catch(() => {}); }, 600);
    }
    function base(): MarkerBase | null {
      const { sessionId, nextNumber, ready } = get();
      if (!sessionId || !ready || paused) return null;
      remember(sessionId, nextNumber);
      set({ nextNumber: nextNumber + 1 });
      return { id: crypto.randomUUID(), n: nextNumber, created: new Date().toISOString() };
    }
    return {
      sessionId: null, items: [], ready: false, error: null, nextNumber: 1,
      async load(sessionId) {
        if (timer) clearTimeout(timer);
        const request = ++generation;
        paused = false;
        set({ sessionId, items: [], ready: false, error: null });
        try {
          if (writing) await writing;
          const items = await transport.read(sessionId);
          if (request !== generation || paused) return;
          const n = Math.max(remembered.get(sessionId) || 0, numbers?.get(sessionId) || 0, ...items.map(m => m.n));
          remember(sessionId, n); saved = JSON.stringify(items);
          set({ items, ready: true, nextNumber: n + 1 });
        } catch (error) {
          if (request === generation) set({ error: error instanceof Error ? error.message : String(error) });
        }
      },
      point(x, y) {
        const marker = base();
        if (marker) schedule([...get().items, { ...marker, kind: 'point', x: Math.round(x), y: Math.round(y), radius: 4 }]);
      },
      rect(x1, y1, x2, y2) {
        const marker = base();
        if (!marker) return;
        const left = Math.round(Math.min(x1, x2)), top = Math.round(Math.min(y1, y2));
        schedule([...get().items, {
          ...marker, kind: 'rect', x1: left, y1: top,
          x2: Math.max(left + 1, Math.round(Math.max(x1, x2))), y2: Math.max(top + 1, Math.round(Math.max(y1, y2))),
        }]);
      },
      note(id, note) { schedule(get().items.map(m => m.id === id ? { ...m, note } : m)); },
      remove(id) { schedule(get().items.filter(m => m.id !== id)); },
      clear() { schedule([]); },
      async flush(force = false) {
        if (timer) clearTimeout(timer);
        const sessionId = get().sessionId;
        if (writing) await writing;
        if ((paused && !force) || !get().ready || get().sessionId !== sessionId) return;
        const items = structuredClone(get().items), serialized = JSON.stringify(items);
        if (serialized === saved) return;
        const operation = transport.write(items, sessionId ?? undefined);
        writing = operation;
        try {
          await operation;
          if (get().sessionId === sessionId) { saved = serialized; set({ error: null }); }
        } catch (error) {
          if (get().sessionId === sessionId) set({ error: error instanceof Error ? error.message : String(error) });
          throw error;
        } finally { if (writing === operation) writing = null; }
      },
      pause() { paused = true; if (timer) clearTimeout(timer); },
      resume() {
        paused = false;
        const { sessionId, ready } = get();
        if (sessionId && !ready) void get().load(sessionId);
        else schedule(get().items);
      },
    };
  });
}
