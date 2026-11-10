import { afterEach, expect, it, vi } from 'vitest';
import { api } from './client';
import { prepareSessionChange } from '../editor/lifecycle';
import { sessionAction } from './session-actions';
import { useShell } from './shell';

vi.mock('./client', () => ({ api: vi.fn() }));
vi.mock('../editor/lifecycle', () => ({
  prepareSessionChange: vi.fn(), resumeEditorSession: vi.fn(),
}));

afterEach(() => {
  vi.resetAllMocks();
  useShell.setState({ project: null });
});

it('keeps a pending build bound to its original project while autosave flushes', async () => {
  let finish!: () => void;
  vi.mocked(prepareSessionChange).mockReturnValue(new Promise<void>(resolve => { finish = resolve; }));
  useShell.setState({ project: 'orion' });
  const action = sessionAction('/build');
  useShell.setState({ project: 'carina' });
  finish();
  await action;
  expect(api).toHaveBeenCalledWith('/build', 'POST', undefined, 'orion');
});
