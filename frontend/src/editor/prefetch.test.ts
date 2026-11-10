import { expect, it } from 'vitest';
import { orderedPrefetch } from './prefetch';

it('bounds requests and buffered results while retaining upload order', async () => {
  let active = 0, peak = 0;
  const uploaded: number[] = [];
  await orderedPrefetch([0, 1, 2, 3, 4, 5], async n => {
    peak = Math.max(peak, ++active);
    await new Promise(resolve => setTimeout(resolve, n === 0 ? 20 : 1));
    active--; return n;
  }, (n, result) => { expect(result).toBe(n); uploaded.push(n); }, 3);
  expect(peak).toBe(3);
  expect(uploaded).toEqual([0, 1, 2, 3, 4, 5]);
});

it('cancels in-flight fetches and never uploads after cancellation', async () => {
  const controller = new AbortController(), uploaded: number[] = [];
  let aborted = false;
  await expect(orderedPrefetch([0, 1, 2], async (n, signal) => {
    if (n === 0) return n;
    return new Promise<number>((_, reject) => signal.addEventListener('abort', () => {
      aborted = true; reject(signal.reason);
    }, { once: true }));
  }, n => { uploaded.push(n); controller.abort(); }, 2, controller.signal)).rejects.toThrow();
  expect(uploaded).toEqual([0]); expect(aborted).toBe(true);
});

it('handles a later rejected fetch without an unhandled rejection', async () => {
  const uploaded: number[] = [];
  await expect(orderedPrefetch([0, 1], async n => {
    if (n === 1) throw Error('fetch failed');
    await new Promise(resolve => setTimeout(resolve, 10)); return n;
  }, n => { uploaded.push(n); })).rejects.toThrow('fetch failed');
  expect(uploaded).toEqual([0]);
});
