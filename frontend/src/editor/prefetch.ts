/** Bound both requests and buffered results, consuming in source order. */
export async function orderedPrefetch<T, R>(
  items: readonly T[], fetchItem: (item: T, signal: AbortSignal) => Promise<R>,
  consume: (item: T, result: R) => void | Promise<void>, concurrency = 4, signal?: AbortSignal,
) {
  if (!Number.isInteger(concurrency) || concurrency < 1) throw Error('Invalid prefetch concurrency');
  const controller = new AbortController();
  const combined = signal ? AbortSignal.any([signal, controller.signal]) : controller.signal;
  const pending = new Map<number, Promise<{ value: R } | { error: unknown }>>();
  const start = (index: number) => {
    if (index >= items.length) return;
    pending.set(index, Promise.resolve().then(() => {
      combined.throwIfAborted();
      return fetchItem(items[index], combined);
    }).then(value => ({ value }), error => ({ error })));
  };
  try {
    for (let i = 0; i < Math.min(concurrency, items.length); i++) start(i);
    for (let i = 0; i < items.length; i++) {
      const result = await pending.get(i)!;
      pending.delete(i);
      combined.throwIfAborted();
      if ('error' in result) throw result.error;
      await consume(items[i], result.value);
      start(i + concurrency);
    }
  } finally { controller.abort(); }
}
