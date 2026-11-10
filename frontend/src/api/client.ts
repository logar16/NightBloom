import createClient from 'openapi-fetch';
import { QueryClient } from '@tanstack/react-query';
import type { paths } from './schema';
import { useShell } from './shell';
export { projectKey } from './shell';

export function projectUrl(path: string, session = useShell.getState().project) {
  if (!session) return path;
  return `${path}${path.includes('?') ? '&' : '?'}session=${encodeURIComponent(session)}`;
}
export function projectHeaders(session = useShell.getState().project): Record<string, string> {
  return session ? { 'X-AstroImp-Session': session } : {};
}

export const client = createClient<paths>({ baseUrl: '' });
export const queryClient = new QueryClient({
  defaultOptions: { queries: { retry: false, refetchOnWindowFocus: false } },
});

// FastAPI's existing unannotated JSON responses have no OpenAPI response schema.
// Domain types narrow those unknown payloads at the endpoint boundary.
export async function api<T>(path: string, method = 'GET', body?: unknown, session?: string): Promise<T> {
  const headers: Record<string, string> = {};
  if (body !== undefined) headers['Content-Type'] = 'application/json';
  // Session-scoped writes name their session so the server refuses them after a session switch.
  const project = session ?? useShell.getState().project;
  if (project) headers['X-AstroImp-Session'] = project;
  const response = await client.request(
    method.toLowerCase() as 'get' | 'post' | 'put' | 'patch' | 'delete',
    `/api${path}` as never, {
    body,
    headers: Object.keys(headers).length ? headers : undefined,
    } as never);
  if (response.error || !response.response.ok) {
    const error: unknown = response.error;
    const detail = error && typeof error === 'object' && 'detail' in error ? error.detail : error;
    throw new Error(typeof detail === 'string' ? detail
      : JSON.stringify(detail ?? response.response.statusText));
  }
  return response.data as T;
}
