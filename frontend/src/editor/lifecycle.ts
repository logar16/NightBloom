interface Persistence {
  prepare: () => Promise<void>;
  resume: () => void;
}
let persistence: Persistence | null = null;
let load: { cancel: () => void; resume: () => void } | null = null;
const saves = new Set<Promise<void>>();

export function bindEditorPersistence(value: Persistence) {
  persistence = value;
  return () => { if (persistence === value) persistence = null; };
}
export function bindEditorLoad(cancel: () => void, resume: () => void = () => {}) {
  const value = { cancel, resume };
  load = value;
  return () => { if (load === value) load = null; };
}
export function trackEditorSave(save: Promise<void>) {
  saves.add(save);
  void save.then(() => saves.delete(save), () => saves.delete(save));
}
/** Call before changing the server's current session or replacing its project. */
export async function prepareSessionChange() {
  load?.cancel();
  await persistence?.prepare();
  await Promise.all([...saves]);
}
/** Resume the current editor if the server-changing action failed. */
export function resumeEditorSession() {
  persistence?.resume();
  load?.resume();
}
