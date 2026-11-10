import { useCallback, useEffect, useRef, useState } from 'react';
import { api, projectKey, queryClient } from '../../api/client';
import { toast, useStatus } from '../../api/hooks';
import type { Session } from '../../api/types';
import { editorStore, useEditor } from '../../editor/store';
import { bindEditorPersistence, trackEditorSave } from '../../editor/lifecycle';
import { markerStore } from './markers';

export function useEditorPersistence(sessionId: string) {
  const state = useEditor(s => s.state);
  const message = useEditor(s => s.toast);
  const { data: status } = useStatus();
  const [saveError, setSaveError] = useState(false);
  const saved = useRef(state);
  const savedSerialized = useRef<string | undefined>(undefined);
  const timer = useRef<ReturnType<typeof setTimeout> | null>(null);
  const saving = useRef<Promise<void> | null>(null);
  const paused = useRef(false);
  const sessionRef = useRef(sessionId); sessionRef.current = sessionId;
  const flush = useCallback(async (force = false) => {
    if (timer.current) clearTimeout(timer.current);
    if (paused.current && !force) return;
    if (saving.current) await saving.current;
    const current = editorStore.getState();
    if (!current.state || current.sessionId !== sessionId) return;
    const snapshot = current.state;
    if (snapshot === saved.current) return;
    const serialized = JSON.stringify(snapshot);
    savedSerialized.current ??= JSON.stringify(saved.current);
    if (serialized === savedSerialized.current) { saved.current = snapshot; return; }
    const operation = (async () => {
      const response = await api<{ revision?: number }>('/edits', 'PUT', JSON.parse(serialized) as unknown, sessionId);
      if (sessionRef.current !== sessionId || editorStore.getState().sessionId !== sessionId) return;
      saved.current = snapshot; savedSerialized.current = serialized;
      if (response?.revision) editorStore.getState().savedRevision(response.revision);
      queryClient.setQueryData<Session>(projectKey('session', sessionId), old => old?.id === sessionId
        ? { ...old, edits: JSON.parse(serialized) as Record<string, unknown> } : old);
      setSaveError(false);
    })();
    saving.current = operation;
    trackEditorSave(operation);
    try { await operation; }
    catch (error) { setSaveError(true); throw error; }
    finally { if (saving.current === operation) saving.current = null; }
  }, [sessionId]);
  useEffect(() => {
    if (paused.current) return;
    if (state === saved.current) return;
    timer.current = setTimeout(() => {
      void flush().catch((e: Error) => toast(e.message));
    }, 600);
    return () => { if (timer.current) clearTimeout(timer.current); };
  }, [state, flush]);
  useEffect(() => {
    if (paused.current) return;
    const revision = status?.edits;
    if (!revision || status.session !== sessionId) return;
    if (revision.source === 'assistant' && revision.revision > (editorStore.getState().knownRevision ?? revision.revision)) {
      if (timer.current) clearTimeout(timer.current);
    }
    void editorStore.getState().observeRevision(revision, () => api<unknown>('/edits', 'GET', undefined, sessionId)).then(adopted => {
      if (!adopted) return;
      const adoptedState = editorStore.getState().state;
      saved.current = adoptedState; savedSerialized.current = undefined;
      queryClient.setQueryData<Session>(projectKey('session', sessionId), old => old?.id === sessionId
        ? { ...old, edits: adoptedState as unknown as Record<string, unknown> } : old);
    }).catch((e: Error) => toast(e.message));
  }, [status?.edits?.revision, status?.session, sessionId]);
  useEffect(() => {
    if (message) { toast(message); editorStore.getState().clearToast(); }
  }, [message]);
  useEffect(() => bindEditorPersistence({
    prepare: async () => {
      paused.current = true;
      if (timer.current) clearTimeout(timer.current);
      markerStore.getState().pause();
      await Promise.all([flush(true), markerStore.getState().flush(true)]);
    },
    resume: () => {
      paused.current = false;
      markerStore.getState().resume();
      void flush().catch((e: Error) => toast(e.message));
    },
  }), [flush]);
  useEffect(() => () => {
    if (timer.current) clearTimeout(timer.current);
    const pending = Promise.all([flush(), markerStore.getState().flush()]).then(() => {});
    trackEditorSave(pending);
    void pending.catch((e: Error) => toast(e.message));
  }, [flush]);
  const flushAll = useCallback(async () => {
    await Promise.all([flush(), markerStore.getState().flush()]);
  }, [flush]);
  return { flush: flushAll, saveError };
}
