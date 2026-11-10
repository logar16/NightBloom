import { useMutation, useQuery } from '@tanstack/react-query';
import { api, projectKey, queryClient } from './client';
import { useShell, type Screen } from './shell';
export { useShell } from './shell';
import type { BuildPlan, CoordinateCapture, GalleryProject, Job, Session, SessionSummary, Status } from './types';
import { sessionAction } from './session-actions';
import { resumeEditorSession } from '../editor/lifecycle';
import { editorStore } from '../editor/store';
import { orderGallery } from './gallery';

export const useStatus = () => {
  const id = useShell(s => s.project);
  return useQuery({
    queryKey: projectKey('status', id), queryFn: () => api<Status>('/status', 'GET', undefined, id ?? undefined), refetchInterval: 500,
  });
};
export const useSession = (enabled = true) => {
  const id = useShell(s => s.project);
  return useQuery({
    queryKey: projectKey('session', id), queryFn: () => api<Session>('/session', 'GET', undefined, id ?? undefined), enabled,
  });
};
export const useSessions = (enabled = true) => useQuery({
  queryKey: ['sessions'], queryFn: () => api<SessionSummary[]>('/sessions'), enabled,
});
export const useGallery = () => useQuery({
  queryKey: ['gallery'], queryFn: () => api<GalleryProject[]>('/gallery'), select: orderGallery, staleTime: 30_000,
});
export const useJob = () => {
  const id = useShell(s => s.project);
  return useQuery({
    queryKey: projectKey('job', id), queryFn: () => api<Job | null>('/job', 'GET', undefined, id ?? undefined), refetchInterval: 500,
  });
};
export const useBuildPlan = (enabled = true) => {
  const id = useShell(s => s.project);
  return useQuery({
    queryKey: projectKey('plan', id), queryFn: () => api<BuildPlan>('/build/plan', 'GET', undefined, id ?? undefined), enabled,
  });
};
export function refreshSession() {
  void queryClient.invalidateQueries({ queryKey: ['session'] });
  void queryClient.invalidateQueries({ queryKey: ['sessions'] });
  void queryClient.invalidateQueries({ queryKey: ['gallery'] });
  void queryClient.invalidateQueries({ queryKey: ['plan'] });
}
export function sessionScreen(session: Pick<SessionSummary, 'built' | 'selected'>): Screen {
  return session.built ? 'edit' : session.selected > 0 ? 'build' : 'sky';
}
export function useOpenSession() {
  return useMutation({
    mutationKey: ['session-open'],
    mutationFn: (id: string) => sessionAction<Session>(`/sessions/${encodeURIComponent(id)}/open`),
    onSuccess: (session) => {
      const same = useShell.getState().project === session.id;
      queryClient.setQueryData(projectKey('session', session.id), session);
      if (same) resumeEditorSession();
      refreshSession();
      useShell.getState().setProject(session.id, sessionScreen({ built: !!session.meta, selected: session.selection.obs_ids.length }));
    },
    onError: (error: Error) => toast(error.message),
  });
}
export function useRestoreExportLook() {
  return useMutation({
    mutationKey: ['session-open'],
    mutationFn: (look: { project: string; filename: string } | { project: string; checkpoint: string }) =>
      'filename' in look
        ? sessionAction<Session>(`/gallery/${encodeURIComponent(look.project)}/restore`, 'POST', { filename: look.filename })
        : sessionAction<Session>(`/sessions/${encodeURIComponent(look.project)}/export-checkpoints/${encodeURIComponent(look.checkpoint)}/restore`),
    onSuccess: session => {
      // Reload even when the restored look uses the already-open build.
      editorStore.setState({ sessionId: null, meta: null, state: null });
      queryClient.setQueryData(projectKey('session', session.id), session);
      refreshSession();
      useShell.getState().setProject(session.id, 'edit');
      toast('Checkpoint restored');
    },
    onError: (error: Error) => toast(error.message),
  });
}
export function useDeleteSession() {
  return useMutation({
    mutationKey: ['session-open'],
    mutationFn: (id: string) => api<{ current: string | null }>(`/sessions/${encodeURIComponent(id)}`, 'DELETE'),
    onSuccess: (result, id) => {
      queryClient.removeQueries({ queryKey: projectKey('session', null), exact: true });
      queryClient.removeQueries({ queryKey: projectKey('session', id), exact: true });
      if (useShell.getState().project === id) {
        useShell.setState({ project: result.current });
        useShell.getState().setScreen('home');
      }
      void queryClient.invalidateQueries({ queryKey: ['sessions'] });
      refreshSession();
      toast('Session moved to trash');
    },
    onError: (error: Error) => toast(error.message),
  });
}
export function useCapture() {
  return useMutation({
    mutationKey: ['capture'],
    mutationFn: (coordinates?: CoordinateCapture) => sessionAction<Job>(
      coordinates ? '/capture/coordinates' : '/capture', 'POST', coordinates),
    onSuccess: (job, coordinates) => {
      queryClient.setQueryData(projectKey('job', job.session), job);
      const shell = useShell.getState();
      if (shell.screen !== 'sky') useShell.setState({ captureReturn: shell.screen });
      shell.setProject(job.session, 'sky');
      refreshSession();
      toast(coordinates ? 'Capturing coordinate frame' : 'Capturing Aladin view');
    },
    onError: (error: Error) => toast(error.message),
  });
}
export function useCancelJob() {
  return useMutation({
    mutationFn: (id?: string) => api<Job | null>(`/job/cancel${id ? `?id=${encodeURIComponent(id)}` : ''}`, 'POST'),
    onSuccess: (job) => {
      if (job) queryClient.setQueryData(projectKey('job', job.session), job);
      void queryClient.invalidateQueries({ queryKey: ['status'] });
    },
    onError: (error: Error) => toast(error.message),
  });
}
export const toast = (message: string, project?: string, exportPath?: string) =>
  useShell.getState().toast(message, project, exportPath);
/** Leave the Sky screen after an abandoned capture; a successful one keeps the user on Sky. */
export function endCapture(abandoned: boolean) {
  const { captureReturn } = useShell.getState();
  useShell.setState({ captureReturn: null });
  if (abandoned && captureReturn) useShell.getState().setScreen(captureReturn);
}
