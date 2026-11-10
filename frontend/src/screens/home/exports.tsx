import { useEffect, useRef, useState } from 'react';
import { ChevronLeft, ChevronRight, Copy, Download,
  FolderOpen, ImageOff, LoaderCircle, Maximize2, PanelsTopLeft, Pencil, X } from 'lucide-react';
import { useIsMutating } from '@tanstack/react-query';
import { toast, useOpenSession, useRestoreExportLook } from '../../api/hooks';
import type { GalleryImage, GalleryProject } from '../../api/types';
import { isDesktop, revealExportFile, saveDownload } from '../../api/desktop';
import { IconButton } from '../../ui';
import './exports.css';

type GalleryView = 'thumbnail' | 'preview' | 'clipboard' | 'original';

export function galleryUrl(project: string, image: GalleryImage, view: GalleryView = 'thumbnail') {
  return `/api/gallery/${encodeURIComponent(project)}/${image.filename.split('/').map(encodeURIComponent).join('/')}?view=${view}&v=${encodeURIComponent(image.version)}`;
}
function resolution(image: GalleryImage) {
  return image.width && image.height ? `${image.width.toLocaleString()} × ${image.height.toLocaleString()}` : 'Unavailable';
}
function filename(image: GalleryImage) { return image.filename.split('/').at(-1)!; }

export function ExportImage({ project, image, view = 'thumbnail', loading }: {
  project: string; image: GalleryImage; view?: 'thumbnail' | 'preview'; loading?: 'lazy' | 'eager';
}) {
  const [failed, setFailed] = useState(false);
  return failed || image.error ? <span className="gallery-image-error" role="status" title={image.error ?? 'Image unavailable'}>
    <ImageOff /><span>Image unavailable</span>
  </span> : <img src={galleryUrl(project, image, view)} alt={filename(image)}
    loading={loading ?? (view === 'thumbnail' ? 'lazy' : 'eager')} decoding="async" onError={() => setFailed(true)} />;
}

export function ImageActions({ project, image, onView, revealFolder = true }: {
  project: string; image: GalleryImage; onView?: () => void; revealFolder?: boolean;
}) {
  const [copying, setCopying] = useState(false);
  const restore = useRestoreExportLook();
  const open = useOpenSession();
  const changing = useIsMutating({ mutationKey: ['session-open'] }) > 0;
  const clipboard = typeof ClipboardItem !== 'undefined' && !!navigator.clipboard?.write;
  const copy = async () => {
    setCopying(true);
    try {
      const png = fetch(galleryUrl(project, image, 'clipboard')).then(async response => {
        if (!response.ok) {
          const error = await response.json() as { detail?: string };
          throw new Error(error.detail ?? `Cannot copy image (${response.status})`);
        }
        return response.blob();
      });
      await Promise.all([png, navigator.clipboard.write([new ClipboardItem({ 'image/png': png })])]);
      toast(`Copied ${filename(image)}`);
    } catch (error) { toast((error as Error).message); }
    finally { setCopying(false); }
  };
  return <div className="gallery-actions">
    {onView && <IconButton icon={Maximize2} title="View all exports" onClick={onView} />}
    <IconButton icon={Pencil} aria-label="Edit exported version"
      title={image.checkpoint ? 'Edit exported version (current edits will be checkpointed)'
        : image.checkpoint_error ?? 'No saved edit checkpoint for this export'}
      disabled={!image.checkpoint || changing}
      onClick={() => restore.mutate({ project, filename: image.filename })} />
    <IconButton icon={copying ? LoaderCircle : Copy}
      className={copying ? 'export-spinner' : ''} disabled={copying || !clipboard || !!image.error}
      aria-label="Copy image"
      title={clipboard ? 'Copy image (full-size, 8-bit PNG)' : 'Image clipboard unavailable in this browser'}
      onClick={() => void copy()} />
    <IconButton icon={Download} title="Save original image"
      onClick={() => saveDownload(galleryUrl(project, image, 'original'), filename(image))} />
    <IconButton icon={PanelsTopLeft} title="Open current project" disabled={changing}
      onClick={() => open.mutate(project)} />
    {revealFolder && <IconButton icon={FolderOpen} title={isDesktop() ? 'Show image in folder' : 'Show in folder (desktop app only)'}
      disabled={!isDesktop()} onClick={() => {
        void revealExportFile(project, image.filename).catch((error: Error) => toast(error.message));
      }} />}
  </div>;
}

export function ExportViewer({ project, image, onSelect, onClose }: {
  project: GalleryProject; image: GalleryImage;
  onSelect: (filename: string) => void; onClose: () => void;
}) {
  const dialog = useRef<HTMLDialogElement>(null);
  const current = project.images.findIndex(item => item.filename === image.filename);
  useEffect(() => {
    const previous = document.activeElement;
    const element = dialog.current!;
    element.showModal();
    return () => { element.close(); if (previous instanceof HTMLElement) previous.focus(); };
  }, []);
  const move = (index: number) => { if (index >= 0 && index < project.images.length) onSelect(project.images[index].filename); };
  return <dialog ref={dialog} className="gallery-viewer" aria-label="Export viewer"
    onCancel={event => { event.preventDefault(); onClose(); }}
    onKeyDown={event => {
      if (['ArrowLeft', 'ArrowRight', 'Home', 'End'].includes(event.key)) {
        event.preventDefault();
        move(event.key === 'Home' ? 0 : event.key === 'End' ? project.images.length - 1
          : current + (event.key === 'ArrowLeft' ? -1 : 1));
      }
    }}>
    <header className="gallery-viewer-heading">
      <div><small>{project.name}</small><h2 title={image.filename}>{filename(image)}</h2></div>
      <ImageActions key={image.filename} project={project.id} image={image} />
      <IconButton icon={X} title="Close viewer" onClick={onClose} />
    </header>
    <div className="gallery-stage">
      <IconButton icon={ChevronLeft} title="Previous image" disabled={current === 0} onClick={() => move(current - 1)} />
      <div className="gallery-stage-image"><ExportImage key={image.version + image.filename} project={project.id} image={image} view="preview" /></div>
      <IconButton icon={ChevronRight} title="Next image" disabled={current === project.images.length - 1} onClick={() => move(current + 1)} />
    </div>
    <footer className="gallery-viewer-meta"><span>{resolution(image)}</span>
      <span>{filename(image).split('.').at(-1)?.toUpperCase()}</span>
      <span>{new Date(image.modified * 1000).toLocaleString()}</span>
      <span className="gallery-position" aria-live="polite">{current + 1} / {project.images.length}</span>
    </footer>
    <div className="gallery-filmstrip" aria-label="Export images">
      {project.images.map(item => <button key={item.filename} aria-label={`View ${filename(item)}`}
        aria-current={item.filename === image.filename ? 'true' : undefined}
        onClick={() => onSelect(item.filename)}>
        <ExportImage key={item.version} project={project.id} image={item} />
      </button>)}
    </div>
  </dialog>;
}
