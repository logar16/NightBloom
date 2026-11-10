import { prepareSessionChange, resumeEditorSession } from '../editor/lifecycle';
import { api } from './client';
import { useShell } from './shell';

export async function sessionAction<T>(path: string, method = 'POST', body?: unknown): Promise<T> {
  const session = useShell.getState().project;
  try {
    await prepareSessionChange();
    return await api<T>(path, method, body, session ?? undefined);
  } catch (error) {
    resumeEditorSession();
    throw error;
  }
}
