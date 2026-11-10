import { useEffect, useRef, useState } from 'react';
import { Bookmark, BookmarkCheck, Check, ChevronLeft, ChevronRight, EyeOff, LoaderCircle, RefreshCw, Star, TriangleAlert, X } from 'lucide-react';
import type { Group, Session } from '../../api/types';
import { IconButton } from '../../ui';
import { previewUrl, waveLabel } from './utils';

export function Preview({ group, view, index, selected, favorites, dismissed, onIndex, onToggle, onTriage, onClose,
  bookmarked, onBookmark, bookmarkBusy, busy }: {
  group: Group; view: Session['view']; index: number; selected: Set<string>; onIndex: (index: number) => void;
  favorites: Set<string>; dismissed: Set<string>; onToggle: (id: string) => void;
  onTriage: (ids: string[], kind: 'favorite' | 'dismiss') => void; onClose: () => void;
  bookmarked: Set<string>; onBookmark: (ids: string[]) => void; bookmarkBusy: boolean;
  busy: boolean;
}) {
  const dialog = useRef<HTMLDialogElement>(null);
  const observation = group.observations[index];
  const [loaded, setLoaded] = useState('');
  const [failed, setFailed] = useState('');
  const [revision, setRevision] = useState(0);
  const source = previewUrl(observation, 'large', view, revision);
  const thumbnail = previewUrl(observation, 'thumb', view);
  const unavailable = failed === source;
  const loading = loaded !== source && !unavailable;
  const canBookmark = ['HST', 'JWST'].includes(observation.collection);
  const isBookmarked = bookmarked.has(observation.obs_id);
  const dismiss = () => {
    if (!dismissed.has(observation.obs_id)) onTriage([observation.obs_id], 'dismiss');
    if (index < group.observations.length - 1) onIndex(index + 1);
    else onClose();
  };
  useEffect(() => { dialog.current?.showModal(); }, []);
  return <dialog ref={dialog} id="preview-dialog" className="preview-dialog" aria-labelledby="preview-title" onClose={onClose}
    onKeyDown={(event) => {
      if (event.ctrlKey || event.metaKey || event.altKey) return;
      if ([' ', 'Enter', 'ArrowLeft', 'ArrowRight'].includes(event.key) || ['f', 'd', 'b'].includes(event.key.toLowerCase())) event.preventDefault();
      if (event.repeat && !['ArrowLeft', 'ArrowRight'].includes(event.key)) return;
      if (event.key === 'ArrowLeft') onIndex(Math.max(0, index - 1));
      if (event.key === 'ArrowRight') onIndex(Math.min(group.observations.length - 1, index + 1));
      if (!busy && (event.key === ' ' || event.key === 'Enter')) onToggle(observation.obs_id);
      if (!busy && event.key.toLowerCase() === 'f') onTriage([observation.obs_id], 'favorite');
      if (event.key.toLowerCase() === 'b' && canBookmark && !bookmarkBusy) onBookmark([observation.obs_id]);
      if (!busy && event.key.toLowerCase() === 'd') dismiss();
    }}>
    <div className="dialog-bar"><div><strong id="preview-title">{observation.filter} · {observation.target || observation.instrument}</strong>
      <span>{observation.collection} · {observation.instrument} · {waveLabel(observation.wavelength_nm ?? 550)}</span>
    </div><IconButton icon={X} title="Close preview (Esc)" onClick={onClose} /></div>
    <div className="preview-image-wrap">
      <IconButton icon={ChevronLeft} title="Previous image (←)" disabled={index === 0} onClick={() => onIndex(index - 1)} />
      <img id="preview-image" alt={`${observation.target || observation.instrument} · ${observation.filter}`}
        decoding="async" src={loaded === source ? source : thumbnail} />
      <img key={source} src={source} alt="" aria-hidden style={{ display: 'none' }}
        onLoad={() => setLoaded(source)} onError={() => setFailed(source)} />
      {(loading || unavailable) && <div className="preview-status" role={unavailable ? 'alert' : 'status'}>
        {unavailable ? <TriangleAlert aria-hidden /> : <LoaderCircle className="export-spinner" aria-hidden />}
        <span>{unavailable ? 'Large preview unavailable' : 'Loading preview…'}</span>
        {unavailable && <button className="subtle" onKeyDown={event => event.stopPropagation()}
          onClick={() => setRevision(value => value + 1)}><RefreshCw />Retry</button>}
      </div>}
      <IconButton icon={ChevronRight} title="Next image (→)" disabled={index === group.observations.length - 1}
        onClick={() => onIndex(index + 1)} />
    </div>
    <div className="preview-footer"><span>{[observation.info?.line, observation.info?.shows].filter(Boolean).join(' · ')}</span>
      <IconButton icon={Check} title={`${selected.has(observation.obs_id) ? 'Deselect' : 'Select'} image (Space / Enter)`}
        aria-pressed={selected.has(observation.obs_id)} className={selected.has(observation.obs_id) ? 'active' : ''} disabled={busy}
        onClick={() => onToggle(observation.obs_id)} />
      <IconButton icon={Star} title="Favorite image (F)" aria-pressed={favorites.has(observation.obs_id)} disabled={busy}
        className={favorites.has(observation.obs_id) ? 'favorited' : ''} onClick={() => onTriage([observation.obs_id], 'favorite')} />
      {canBookmark && <IconButton icon={isBookmarked ? BookmarkCheck : Bookmark}
        title={`${isBookmarked ? 'Remove bookmark' : 'Bookmark image'} (B)`}
        aria-pressed={isBookmarked} disabled={bookmarkBusy} className={isBookmarked ? 'bookmarked' : ''}
        onClick={() => onBookmark([observation.obs_id])} />}
      <IconButton icon={EyeOff} title="Dismiss image and advance (D)" disabled={busy} onClick={dismiss} /></div>
  </dialog>;
}
