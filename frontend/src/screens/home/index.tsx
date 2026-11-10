import { useEffect, useRef, useState } from 'react';
import { FolderPlus, ImageOff, Images, Layers, LoaderCircle, Orbit, PanelLeftClose, PanelLeftOpen, Trash2 } from 'lucide-react';
import { useIsMutating } from '@tanstack/react-query';
import { sessionScreen, useDeleteSession, useGallery, useOpenSession, useSessions, useShell, useStatus } from '../../api/hooks';
import type { Job, SessionSummary } from '../../api/types';
import { galleryCover } from '../../api/gallery';
import { activeJob, projectColor, projectHref } from '../../api/shell';
import { ExportImage, ExportViewer, ImageActions, galleryUrl } from './exports';
import { ExportImagePicker, useExportImageOpen } from './open-image';
import './home.css';

function ProjectProgress({ job }: { job: Job }) {
  return <span className="home-job" style={{ '--project-color': projectColor(job.session) } as React.CSSProperties}>
    <span className="home-job-label"><i className="project-tag" />
      {job.state === 'queued' ? `Queued · ${job.message.replace(/^Waiting/, 'waiting')}` : job.stage || job.kind}
      <span className="mono">{Math.round(job.progress * 100)}%</span></span>
    <span className="home-job-progress" role="progressbar" aria-label={`${job.name} ${job.kind} progress`}
      aria-valuemin={0} aria-valuemax={100} aria-valuenow={Math.round(job.progress * 100)}>
      <i style={{ width: `${job.progress * 100}%` }} /></span>
  </span>;
}

function relativeTime(value: string) {
  const seconds = Math.max(0, (Date.now() - new Date(value).getTime()) / 1000);
  if (seconds < 60) return 'Just now';
  const [amount, unit] = seconds < 3600 ? [seconds / 60, 'minute']
    : seconds < 86400 ? [seconds / 3600, 'hour'] : seconds < 604800 ? [seconds / 86400, 'day']
      : seconds < 2678400 ? [seconds / 604800, 'week'] : [seconds / 2678400, 'month'];
  return new Intl.RelativeTimeFormat(undefined, { numeric: 'auto' })
    .format(-Math.floor(amount as number), unit as Intl.RelativeTimeFormatUnit);
}

function Thumbnail({ session, version, eager = false }: { session: SessionSummary; version: number; eager?: boolean }) {
  const [failed, setFailed] = useState(false);
  return <div className={`home-thumbnail${failed ? ' missing' : ''}`}>
    {failed ? <Orbit aria-hidden="true" /> : <img
      src={`/api/sessions/${encodeURIComponent(session.id)}/thumbnail.jpg?v=${encodeURIComponent(session.updated || session.created)}&refresh=${version}`}
      alt="" loading={eager ? 'eager' : 'lazy'} onError={() => setFailed(true)} />}
  </div>;
}

function DeleteSession({ session, disabled }: { session: SessionSummary; disabled: boolean }) {
  const remove = useDeleteSession();
  const [armed, setArmed] = useState(false);
  useEffect(() => {
    if (!armed) return;
    const timer = setTimeout(() => setArmed(false), 3000);
    return () => clearTimeout(timer);
  }, [armed]);
  return <button className={`home-delete${armed ? ' armed' : ''}`} disabled={disabled || remove.isPending}
    title={armed ? 'Click again to delete' : 'Delete session'} aria-label={armed ? `Confirm delete ${session.name}` : `Delete ${session.name}`}
    onClick={event => { event.stopPropagation(); if (armed) remove.mutate(session.id); else setArmed(true); }}>
    <Trash2 />{armed && <span>Delete?</span>}
  </button>;
}

export function HomeScreen() {
  const images = useExportImageOpen();
  const { data: sessions, dataUpdatedAt, isPending, isError, refetch } = useSessions();
  const gallery = useGallery();
  const [viewing, setViewing] = useState<{ project: string; filename: string } | null>(null);
  const [hovered, setHovered] = useState<string | null>(null);
  const [projectsOpen, setProjectsOpen] = useState(false);
  const rail = useRef<HTMLElement>(null);
  const projectToggle = useRef<HTMLButtonElement>(null);
  useEffect(() => {
    if (!projectsOpen) return;
    const outside = (event: Event) => {
      if (event.target instanceof Node && !rail.current?.contains(event.target)) setProjectsOpen(false);
    };
    document.addEventListener('pointerdown', outside, true);
    document.addEventListener('focusin', outside);
    return () => {
      document.removeEventListener('pointerdown', outside, true);
      document.removeEventListener('focusin', outside);
    };
  }, [projectsOpen]);
  const [, tick] = useState(0);
  useEffect(() => {
    const timer = setInterval(() => tick(value => value + 1), 60_000);
    return () => clearInterval(timer);
  }, []);
  const { data: status } = useStatus();
  const { setScreen } = useShell();
  const open = useOpenSession();
  const changing = useIsMutating({ mutationKey: ['session-open'] }) > 0;
  const saving = useIsMutating({ mutationKey: ['session-write'] }) > 0;
  const busy = changing || saving;
  const openLink = (event: React.MouseEvent, id: string) => {
    if (event.button || event.ctrlKey || event.metaKey || event.shiftKey || event.altKey) return;
    event.preventDefault();
    if (!busy) open.mutate(id);
  };
  const albums = gallery.data ?? [];
  const ambient = albums.find(album => album.id === hovered) ?? albums[0];
  const selected = albums.find(album => album.id === viewing?.project);
  const selectedImage = selected?.images.find(image => image.filename === viewing?.filename);
  return <section id="home" className="screen active" aria-label="Home" {...images.handlers}>
    <h1 className="home-title">Home</h1>
    <div className="home-ambient" aria-hidden="true" style={ambient ? {
      backgroundImage: `url("${galleryUrl(ambient.id, galleryCover(ambient.images))}")`,
    } : undefined} />
    <aside ref={rail} className={`home-rail${projectsOpen ? ' expanded' : ''}`} aria-label="Project library"
      onPointerEnter={event => { if (event.pointerType === 'mouse') setProjectsOpen(true); }}
      onPointerLeave={event => {
        if (event.pointerType === 'mouse' && !(event.currentTarget.contains(document.activeElement)
          && document.activeElement?.matches(':focus-visible'))) setProjectsOpen(false);
      }}
      onFocusCapture={event => {
        if (event.target !== projectToggle.current && event.target.matches(':focus-visible')) setProjectsOpen(true);
      }}
      onKeyDown={event => {
        if (event.key === 'Escape' && projectsOpen) {
          event.preventDefault(); setProjectsOpen(false); projectToggle.current?.focus();
        }
      }}>
      <div className="home-rail-tools">
        <button className="home-rail-action home-new" title="New project" aria-label="New project"
          disabled={busy} onClick={() => setScreen('new')}>
          <span className="home-action-icon"><FolderPlus /></span><span className="home-rail-label">New project</span>
        </button>
        <ExportImagePicker opener={images} />
      </div>
      <div className="home-projects" id="home-project-list">
        <div className="home-project-heading"><h2>Projects</h2><span className="mono">{sessions?.length ?? 0}</span></div>
        <nav aria-label="Projects" className="home-project-list">
          {isPending || isError ? <div className="home-project-empty" role="status">
            {isPending ? <LoaderCircle className="export-spinner" /> : <Orbit />}
            <span>{isPending ? 'Loading projects…' : 'Projects unavailable'}</span>
            {isError && <button onClick={() => void refetch()}>Retry</button>}
          </div> : sessions?.length ? <ul>{sessions.map((session, index) => {
            const album = albums.find(album => album.id === session.id);
            const job = status?.jobs?.find(job => job.session === session.id && activeJob(job));
            const updated = session.updated || session.created;
            return <li className="home-project-row" key={session.id}>
              <a className="home-project-link" href={projectHref(session.id, sessionScreen(session))}
                title={session.name} aria-label={`Open ${session.name}`} aria-disabled={busy} onClick={event => openLink(event, session.id)}>
                <Thumbnail key={`${session.id}:${dataUpdatedAt}`} session={session} version={dataUpdatedAt} eager={index < 10} />
                <span className="home-project-details"><strong title={session.name}>{session.name}</strong>
                  {job ? <ProjectProgress job={job} /> : <span className="home-project-meta"><time dateTime={updated} title={new Date(updated).toLocaleString()}>
                    {relativeTime(updated)}</time><span>{gallery.error ? 'Exports unavailable' : gallery.isPending ? '…'
                      : album ? `${album.images.length} ${album.images.length === 1 ? 'export' : 'exports'}` : 'No exports'}</span></span>}
                </span>
              </a>
              <DeleteSession session={session} disabled={busy || !!job} />
            </li>;
          })}</ul> : <div className="home-project-empty"><Orbit /><span>No projects yet</span></div>}
        </nav>
      </div>
      <button ref={projectToggle} className="icon home-project-toggle"
        title={projectsOpen ? 'Hide projects' : 'Show projects'} aria-label={projectsOpen ? 'Hide projects' : 'Show projects'}
        aria-expanded={projectsOpen} aria-controls="home-project-list" onClick={() => setProjectsOpen(!projectsOpen)}>
        {projectsOpen ? <PanelLeftClose /> : <PanelLeftOpen />}
      </button>
    </aside>
    <div className="home-art-surface">
      {gallery.isPending || gallery.error || !albums.length ? <div className="home-empty" role="status">
        {gallery.isPending ? <LoaderCircle className="export-spinner" /> : gallery.error ? <ImageOff /> : <Images />}
        <h2>{gallery.isPending ? 'Loading exports…' : gallery.error ? 'Gallery unavailable' : 'No exports yet'}</h2>
        {gallery.error && <><p>{gallery.error.message}</p><button onClick={() => void gallery.refetch()}>Retry</button></>}
      </div> : <div className="home-bento" aria-label="Export projects">
        {albums.map((album, index) => {
          const image = galleryCover(album.images);
          const show = () => setViewing({ project: album.id, filename: image.filename });
          return <article className="home-art" key={album.id} aria-label={album.name}
            onMouseEnter={() => setHovered(album.id)} onFocus={() => setHovered(album.id)}>
            <button className="home-art-open" aria-label={`View exports from ${album.name}`} onClick={show}>
              <ExportImage key={image.filename + image.version} project={album.id} image={image}
                view="preview" loading={index === 0 ? 'eager' : 'lazy'} />
            </button>
            <span className="home-art-chip" aria-hidden="true"><strong>{album.name}</strong>
              <span><Layers />{album.images.length}</span></span>
            <div className="home-art-overlay">
              <div className="home-art-caption"><h2 title={album.name}>{album.name}</h2>
                <small>{image.width && image.height ? `${image.width} × ${image.height} · ` : ''}
                  {album.images.length} {album.images.length === 1 ? 'export' : 'exports'}</small></div>
              <ImageActions project={album.id} image={image} onView={show} revealFolder={false} />
            </div>
          </article>;
        })}
      </div>}
    </div>
    {selected && selectedImage && <ExportViewer project={selected} image={selectedImage}
      onSelect={filename => setViewing({ project: selected.id, filename })} onClose={() => setViewing(null)} />}
  </section>;
}
