import { create } from 'zustand';

export type Screen = 'home' | 'new' | 'sky' | 'build' | 'edit';
export function route() {
  const match = typeof location === 'undefined' ? null
    : location.hash.match(/^#\/p\/([^/]+)\/(sky|build|edit)$/);
  return match ? { project: decodeURIComponent(match[1]), screen: match[2] as Screen }
    : { project: null, screen: (typeof location !== 'undefined' && location.hash === '#/new' ? 'new' : 'home') as Screen };
}
export function projectHref(id: string, screen: Screen) {
  return screen === 'home' ? '#/' : screen === 'new' ? '#/new' : `#/p/${encodeURIComponent(id)}/${screen}`;
}
interface ShellState {
  project: string | null; screen: Screen; message: string;
  toastProject: string | null;
  toastFile: { project: string; filename: string } | null;
  captureReturn: Screen | null;
  setScreen: (screen: Screen) => void;
  setProject: (project: string, screen: Screen) => void;
  toast: (message: string, project?: string, exportPath?: string) => void;
}
let timer: ReturnType<typeof setTimeout>;
export const useShell = create<ShellState>((set, get) => ({
  ...route(), message: '', toastProject: null, toastFile: null, captureReturn: null,
  setScreen: screen => {
    const project = get().project;
    if (typeof location !== 'undefined' && (project || screen === 'home' || screen === 'new'))
      location.hash = projectHref(project ?? '', screen);
    set({ screen });
  },
  setProject: (project, screen) => {
    if (typeof location !== 'undefined') location.hash = projectHref(project, screen);
    set({ project, screen });
  },
  toast: (message, project, exportPath) => {
    clearTimeout(timer);
    const filename = exportPath?.split(/[\\/]/).at(-1);
    set({ message, toastProject: project ?? null,
      toastFile: project && filename ? { project, filename } : null });
    timer = setTimeout(() => set({ message: '', toastProject: null, toastFile: null }), 7000);
  },
}));
export const projectKey = (key: string, id = useShell.getState().project) => [key, id];
export const activeJob = (job: { state: string } | null | undefined) =>
  job?.state === 'running' || job?.state === 'queued';
const palette = ['#ca8745', '#4eacbb', '#a78ac4', '#77a36d', '#cb7c90', '#8e9ed4'];
export function projectColor(id: string) {
  let hash = 0;
  for (const char of id) hash = (hash * 31 + char.charCodeAt(0)) >>> 0;
  return palette[hash % palette.length];
}
