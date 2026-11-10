import { useEffect, useRef, useState } from 'react';
import { FolderOpen, History, Image, RefreshCw } from 'lucide-react';
import appIcon from './assets/nightbloom.png';
import { useIsMutating } from '@tanstack/react-query';
import { api, projectKey, queryClient } from './api/client';
import { endCapture, refreshSession, toast, useOpenSession, useSession, useSessions, useShell, useStatus } from './api/hooks';
import { EditScreen } from './screens/edit';
import { SkyScreen } from './screens/sky';
import { BuildScreen } from './screens/build';
import { HomeScreen } from './screens/home';
import { NewProjectScreen } from './screens/new';
import { IconButton, Popover } from './ui';
import { Settings } from './theme/Settings';
import { StaleServerBanner } from './theme/StaleServerBanner';
import { useSystemTheme } from './theme/store';
import { activeJob, projectColor, route, useShell as shellStore } from './api/shell';
import { prepareSessionChange, resumeEditorSession } from './editor/lifecycle';
import type { Session } from './api/types';
import { JobsTray } from './JobsTray';
import { isDesktop, openExportImage, openProjectFolder, revealExportFile } from './api/desktop';
import { ProjectName } from './ProjectName';
import { StatusBar } from './StatusBar';

export function App() {
  useSystemTheme();
  const { screen, setScreen, message, project, toastProject, toastFile } = useShell();
  const [history, setHistory] = useState(false);
  const { data: status } = useStatus();
  const browsing = screen === 'home';
  const { data: session } = useSession(!browsing);
  const { data: sessions } = useSessions();
  const projectSession = screen === 'home' || screen === 'new' ? null : session;
  const open = useOpenSession();
  const saving = useIsMutating({ mutationKey: ['session-write'] }) > 0;
  const historyAnchor = useRef<HTMLButtonElement>(null);
  const previousJobs = useRef(new Map<string, string>());
  useEffect(() => {
    const update = async () => {
      const next = route(), previous = shellStore.getState();
      if (next.project && next.project !== previous.project) {
        try {
          await prepareSessionChange();
          const session = await api<Session>(`/sessions/${encodeURIComponent(next.project)}/open`, 'POST');
          queryClient.setQueryData(projectKey('session', session.id), session);
          shellStore.setState(next);
        } catch (error) { resumeEditorSession(); toast((error as Error).message); }
      } else shellStore.setState({ screen: next.screen });
    };
    window.addEventListener('hashchange', update);
    return () => window.removeEventListener('hashchange', update);
  }, []);
  useEffect(() => {
    if (project) void api(`/sessions/${encodeURIComponent(project)}/open`, 'POST')
      .catch((error: Error) => toast(error.message));
  }, [project]);
  useEffect(() => {
    if (!project && session && !browsing) shellStore.getState().setProject(session.id, screen);
    document.title = screen === 'home' ? 'Home — NightBloom'
      : screen === 'new' ? 'New project — NightBloom'
      : `${session?.name ?? 'Project'} · ${screen[0].toUpperCase() + screen.slice(1)} — NightBloom`;
  }, [project, session, screen, browsing]);
  useEffect(() => {
    if (screen === 'build') void queryClient.invalidateQueries({ queryKey: ['plan'] });
  }, [screen]);
  useEffect(() => {
    for (const job of status?.jobs ?? (status?.job ? [status.job] : [])) {
      const previous = previousJobs.current.get(job.id);
      if ((previous === 'running' || previous === 'queued') && !activeJob(job)) {
        refreshSession();
        if (job.session === project) {
          if (job.state === 'done' && job.kind === 'build' && screen === 'build') setScreen('edit');
          if (job.kind === 'capture') endCapture(job.state !== 'done');
        }
        toast(`${job.name || session?.name || 'Project'} · ${job.kind.startsWith('export') ? 'export' : job.kind} ${
          job.state === 'done' ? 'finished' : job.state === 'error' ? `failed: ${job.error ?? job.message}` : 'cancelled'}`, job.session,
          isDesktop() && job.state === 'done' && job.kind.startsWith('export') ? job.result?.path : undefined);
      }
      previousJobs.current.set(job.id, job.state);
    }
  }, [status, session?.name, project, screen, setScreen]);
  return <>
    <header className={`topbar studio-header${projectSession ? ' project-header' : ''}`}>
      <a className="wordmark" href="#" aria-label="NightBloom Home" onClick={(event) => {
        event.preventDefault(); setScreen('home');
      }}><img src={appIcon} width={32} height={32} alt="" /><span>NightBloom</span></a>
      <div className="project-workspace">
      {projectSession && <>
      <div className="project-identity" title={projectSession.name}
        style={{ '--project-color': projectColor(projectSession.id) } as React.CSSProperties}>
        <i className="project-tag" />
        <ProjectName key={projectSession.id} name={projectSession.name} shortcut={screen === 'sky'} />
        {isDesktop() && <IconButton icon={FolderOpen} title="Open project folder"
          onClick={() => { void openProjectFolder(projectSession.id).catch((error: Error) => toast(error.message)); }} />}
      </div>
      <nav className="pipeline" aria-label="Workspace">
        {(['sky', 'build', 'edit'] as const).map((tab, index) =>
          <span key={tab} style={{ display: 'contents' }}>
            {index > 0 && <span className="connector" />}
            <button className={screen === tab ? 'active' : ''} onClick={() => setScreen(tab)}>
              <span className="state-dot" />{tab[0].toUpperCase() + tab.slice(1)}
            </button>
          </span>)}
      </nav>
      </>}
      </div>
      <div className="top-right">
        <JobsTray />
        <button ref={historyAnchor} id="sessions" className="icon" title="Session history" aria-label="Session history"
          disabled={saving || open.isPending}
          onClick={() => setHistory(!history)}><History /></button>
        <IconButton id="refresh" icon={RefreshCw} title="Refresh session" onClick={refreshSession} />
        <Settings />
        {history && <Popover anchor={historyAnchor} onClose={() => setHistory(false)} className="session-history">
          <div className="popover-title">Session history</div>
          {sessions?.map((item) => <button key={item.id} className={item.id === session?.id ? 'current' : ''} onClick={() => {
            setHistory(false);
            open.mutate(item.id);
          }}><strong>{item.name}</strong><small>{new Date(item.updated || item.created).toLocaleDateString()}
              {' · '}{item.observations} images{item.built && <b>Built</b>}</small></button>)}
        </Popover>}
      </div>
      <div className="job-strip"><span style={{
        width: activeJob(status?.job) ? `${status!.job!.progress * 100}%` : '0',
      }} /></div>
    </header>
    {status?.stale && <StaleServerBanner restartable={!!status.restartable} />}
    <main>
      {screen === 'home' && <HomeScreen />}
      {screen === 'new' && <NewProjectScreen />}
      {!browsing && <><SkyScreen key={`sky:${project}`} />
        <BuildScreen key={`build:${project}`} /></>}
      {screen === 'edit' && <EditScreen />}
    </main>
    {screen !== 'edit' && <StatusBar />}
    <div id="toast" role="status" className={message ? 'visible' : ''}>{message}
      {isDesktop() && toastFile ? <button title={`Open ${toastFile.filename} in the default image app`}
        onClick={() => { void openExportImage(toastFile.project, toastFile.filename)
          .catch((error: Error) => toast(error.message)); }}><Image aria-hidden />Open image</button>
        : toastProject && (toastProject !== project || screen === 'home' || screen === 'new') &&
          <button onClick={() => open.mutate(toastProject)}>Open project</button>}
      {isDesktop() && toastFile && <button title={`Show ${toastFile.filename} in the library exports folder`}
        onClick={() => { void revealExportFile(toastFile.project, toastFile.filename)
          .catch((error: Error) => toast(error.message)); }}><FolderOpen aria-hidden />Show in folder</button>}
    </div>
  </>;
}
