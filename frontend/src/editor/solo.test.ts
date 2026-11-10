import { expect, it } from 'vitest';
import { createSoloStore, soloVisible } from './solo';

it('latches a selection, temporarily overrides it, and restores it on Q release', () => {
  const store = createSoloStore(), a = store.getState();
  a.latch('a', ['a', 'b']); expect(store.getState().latched).toEqual(['a', 'b']);
  a.hold(['c']); a.hold(['d']);
  expect(store.getState().momentary).toEqual(['c']);
  a.release(); expect(store.getState().latched).toEqual(['a', 'b']);
  a.latch('b', ['b', 'a']); expect(store.getState().latched).toBeNull();
  a.latch('c', ['a', 'b']); expect(store.getState().latched).toEqual(['c']);
  a.hold(['a']); a.exit();
  expect(store.getState().latched).toBeNull(); expect(store.getState().momentary).toBeNull();
});
it('explicit solo includes hidden layers without changing their eye state', () => {
  expect(soloVisible('hidden', false, ['hidden'])).toBe(true);
  expect(soloVisible('other', true, ['hidden'])).toBe(false);
  expect(soloVisible('hidden', false, null)).toBe(false);
  expect(soloVisible('other', true, null)).toBe(true);
});
