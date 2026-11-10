declare global {
  interface Window { __astroimpPerf?: boolean; __astroimpPerfSequential?: boolean }
}

let sequence = 0;
/** Opt-in User Timing; no marks or GPU synchronization in normal use. */
export function perfStart(name: string) {
  if (typeof window === 'undefined' || !window.__astroimpPerf) return () => {};
  const mark = `astroimp:${name}:${sequence++}`;
  performance.mark(mark);
  return () => {
    performance.measure(`astroimp:${name}`, mark);
    performance.clearMarks(mark);
  };
}
