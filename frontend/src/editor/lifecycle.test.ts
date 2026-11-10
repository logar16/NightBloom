import { describe, expect, it } from 'vitest';
import { bindEditorLoad, bindEditorPersistence, prepareSessionChange, resumeEditorSession, trackEditorSave } from './lifecycle';

describe('session transitions', () => {
  it('pauses and flushes edits before switching, cancels loads, and resumes on failure', async () => {
    const events: string[] = [];
    const unbindLoad = bindEditorLoad(() => events.push('cancel-load'));
    const unbindPersistence = bindEditorPersistence({
      prepare: async () => { events.push('pause'); await Promise.resolve(); events.push('saved'); },
      resume: () => events.push('resume'),
    });
    await prepareSessionChange();
    events.push('switch-session');
    expect(events).toEqual(['cancel-load', 'pause', 'saved', 'switch-session']);
    resumeEditorSession();
    expect(events.at(-1)).toBe('resume');
    unbindLoad(); unbindPersistence();
  });
  it('awaits saves started by an editor that already unmounted', async () => {
    let finish!: () => void;
    trackEditorSave(new Promise<void>(resolve => { finish = resolve; }));
    let prepared = false;
    const pending = prepareSessionChange().then(() => { prepared = true; });
    await Promise.resolve();
    expect(prepared).toBe(false);
    finish(); await pending;
    expect(prepared).toBe(true);
  });
});
