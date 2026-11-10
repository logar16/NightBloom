import { afterEach, describe, expect, it, vi } from 'vitest';
import { createMarkerStore, markerText, type Marker, type MarkerNumbers } from './markers';

afterEach(() => { vi.useRealTimers(); });
function fixture(initial: Marker[] = [], numbers?: MarkerNumbers) {
  let persisted = structuredClone(initial);
  const write = vi.fn(async (items: Marker[]) => { persisted = structuredClone(items); });
  const transport = { read: async () => structuredClone(persisted), write };
  const store = createMarkerStore(transport, numbers);
  return { store, write, transport, persisted: () => persisted };
}
describe('problem area markers', () => {
  it('never reuses numbers after delete, clear, reload, or returning to a session', async () => {
    const memory = new Map<string, number>();
    const numbers = { get: (id: string) => memory.get(id) || 0, set: (id: string, n: number) => { memory.set(id, n); } };
    const { store, transport } = fixture([], numbers), actions = store.getState();
    await actions.load('m16');
    actions.point(1558.2, 1015.8);
    expect(store.getState().items[0]).toMatchObject({ n: 1, kind: 'point', x: 1558, y: 1016, radius: 4 });
    actions.clear(); await actions.flush();
    const reloaded = createMarkerStore(transport, numbers);
    await reloaded.getState().load('m16');
    reloaded.getState().point(12, 15);
    expect(reloaded.getState().items[0].n).toBe(2);
    reloaded.getState().remove(reloaded.getState().items[0].id);
    reloaded.getState().point(20, 30);
    expect(reloaded.getState().items[0].n).toBe(3);
    reloaded.getState().clear(); await reloaded.getState().flush();
    await reloaded.getState().load('jwst');
    expect(reloaded.getState().items).toEqual([]);
    expect(reloaded.getState().nextNumber).toBe(1);
    await reloaded.getState().load('m16');
    expect(reloaded.getState().nextNumber).toBe(4);
    actions.pause(); reloaded.getState().pause();
  });
  it('debounces full-list writes, includes notes, normalizes rects, and copies image coordinates', async () => {
    vi.useFakeTimers();
    const { store, write, persisted } = fixture(), actions = store.getState();
    await actions.load('m16');
    actions.point(1558, 1016);
    actions.rect(90, 70, 10, 20);
    actions.note(store.getState().items[1].id, 'Dark edge');
    await vi.advanceTimersByTimeAsync(599);
    expect(write).not.toHaveBeenCalled();
    await vi.advanceTimersByTimeAsync(1);
    expect(write).toHaveBeenCalledTimes(1);
    expect(persisted()[1]).toMatchObject({ n: 2, kind: 'rect', x1: 10, y1: 20, x2: 90, y2: 70, note: 'Dark edge' });
    expect(markerText(persisted())).toBe('#1 point 1558,1016 r=4\n#2 rect 10,20 90,70 · Dark edge');
    actions.remove(store.getState().items[0].id);
    await actions.flush();
    expect(persisted()).toHaveLength(1);
    actions.pause();
  });
  it('loads existing numbers and discards marker responses from a previous session', async () => {
    let finish!: (items: Marker[]) => void;
    let calls = 0;
    const existing: Marker = { id: 'existing', n: 8, kind: 'point', x: 1, y: 2, radius: 4, created: '2026-10-23T00:00:00Z' };
    const store = createMarkerStore({
      read: () => ++calls === 1 ? new Promise(resolve => { finish = resolve; }) : Promise.resolve([existing]),
      write: async () => {},
    });
    const previous = store.getState().load('m16');
    await store.getState().load('jwst');
    finish([]); await previous;
    expect(store.getState().sessionId).toBe('jwst');
    expect(store.getState().items).toEqual([existing]);
    expect(store.getState().nextNumber).toBe(9);
  });
  it('flushes before session transitions, pauses future writes, and retains failed writes for retry', async () => {
    vi.useFakeTimers();
    const { store, write } = fixture(), actions = store.getState();
    await actions.load('m16');
    actions.point(1, 2); actions.pause();
    await vi.advanceTimersByTimeAsync(1000);
    expect(write).not.toHaveBeenCalled();
    await actions.flush(true);
    expect(write).toHaveBeenCalledTimes(1);
    actions.resume(); actions.point(3, 4);
    write.mockRejectedValueOnce(Error('Offline'));
    await expect(actions.flush()).rejects.toThrow('Offline');
    expect(store.getState().error).toBe('Offline');
    await actions.flush();
    expect(store.getState().error).toBeNull();
    expect(write).toHaveBeenLastCalledWith(store.getState().items, store.getState().sessionId);
    actions.pause();
  });
});
