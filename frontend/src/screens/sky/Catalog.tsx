import { useState, type MouseEvent } from 'react';
import { Bookmark, BookmarkCheck, Check, CheckCheck, ChevronDown, Eye, EyeOff, Star, ZoomIn } from 'lucide-react';
import type { Group, Observation, Session } from '../../api/types';
import { IconButton } from '../../ui';
import { previewUrl, waveStyle } from './utils';
import { ContextMenu, type MenuEntry } from '../../ui/ContextMenu';

export interface CatalogGroup extends Group { originalIndex: number; drawer?: boolean }
export interface CatalogProps {
  view: Session['view'];
  groups: CatalogGroup[]; selected: Set<string>; favorites: Set<string>; dismissed: Set<string>;
  showDismissed: boolean; onDismissed: (open: boolean) => void;
  showSoft: boolean; onSoft: (open: boolean) => void;
  onToggle: (id: string) => void; onTriage: (ids: string[], kind: 'favorite' | 'dismiss') => void;
  onHover: (index: number | string | null) => void; onPreview: (observation: Observation) => void;
  onGroupToggle: (ids: string[]) => void;
  bookmarked: Set<string>; onBookmark: (ids: string[]) => void; bookmarkBusy: boolean;
  flash: string | null;
  busy: boolean;
}
export function Catalog(props: CatalogProps) {
  const { groups, selected, favorites, dismissed, onToggle, onTriage, onHover, onPreview } = props;
  const [menu, setMenu] = useState<{ x: number; y: number; items: MenuEntry[] } | null>(null);
  const context = (event: MouseEvent, ids: string[], observation?: Observation) => {
    event.preventDefault(); event.stopPropagation();
    event.currentTarget.querySelector<HTMLButtonElement>('button')?.focus({ preventScroll: true });
    const selectable = ids.filter(id => !dismissed.has(id)), allSelected = selectable.length > 0 && selectable.every(id => selected.has(id));
    const restore = ids.every(id => dismissed.has(id)), favorite = ids.every(id => favorites.has(id));
    setMenu({ x: event.clientX, y: event.clientY, items: [
      { label: observation ? selected.has(observation.obs_id) ? 'Deselect' : 'Select'
        : allSelected ? 'Deselect group' : 'Select all in group', icon: observation ? Check : CheckCheck,
      disabled: props.busy || !observation && !selectable.length, action: () => observation ? onToggle(observation.obs_id) : props.onGroupToggle(selectable) },
      { label: observation ? favorite ? 'Unfavourite' : 'Favourite' : favorite ? 'Unfavourite group' : 'Favourite group',
        icon: Star, disabled: props.busy, action: () => onTriage(ids, 'favorite') },
      { label: `${restore ? 'Restore' : 'Dismiss'}${observation ? '' : ' group'}`, icon: restore ? Eye : EyeOff,
        disabled: props.busy, action: () => onTriage(ids, 'dismiss') },
      ...(observation ? ['separator' as const, { label: 'Open preview', icon: ZoomIn, action: () => onPreview(observation) }] : []),
    ] });
  };
  const normal = groups.filter((group) => group.observations.some((o) => !dismissed.has(o.obs_id)));
  const primary = normal.map(group => ({ ...group,
    observations: group.observations.filter(o => !(o.collection === 'Survey' && o.soft)),
  })).filter(group => group.observations.some(o => !dismissed.has(o.obs_id)));
  const softer = normal.map(group => ({ ...group,
    observations: group.observations.filter(o => o.collection === 'Survey' && o.soft),
  })).filter(group => group.observations.some(o => !dismissed.has(o.obs_id)));
  const softCount = softer.reduce((count, group) =>
    count + group.observations.filter(o => !dismissed.has(o.obs_id)).length, 0);
  const hidden = groups.filter((group) => group.observations.every((o) => dismissed.has(o.obs_id)));
  const partial = normal.filter((group) => group.observations.some((o) => dismissed.has(o.obs_id)))
    .map((group) => ({ ...group, drawer: true, observations: group.observations.filter((o) => dismissed.has(o.obs_id)) }));
  const hiddenCount = groups.reduce((n, group) => n + group.observations.filter((o) => dismissed.has(o.obs_id)).length, 0);
  const triageButtons = (items: Observation[], group = false) => {
    const ids = items.map(item => item.obs_id);
    const archiveIds = items.filter(item => ['HST', 'JWST'].includes(item.collection)).map(item => item.obs_id);
    const bookmarked = archiveIds.length > 0 && archiveIds.every(id => props.bookmarked.has(id));
    const restore = ids.every((id) => dismissed.has(id));
    const selectable = ids.filter(id => !dismissed.has(id));
    const allSelected = selectable.every(id => selected.has(id));
    return <div className={group ? 'group-triage' : 'thumb-triage'}>
      {group && !restore && <IconButton icon={CheckCheck} className="small"
        title={allSelected ? 'Deselect group' : 'Select all in group'} aria-pressed={allSelected} disabled={props.busy}
        onClick={() => props.onGroupToggle(selectable)} />}
      <IconButton icon={Star} title={`Favorite ${group ? 'group' : 'image'}`} disabled={props.busy}
        className={`small ${ids.some((id) => favorites.has(id)) ? 'favorited' : ''}`}
        onClick={() => onTriage(ids, 'favorite')} />
      {archiveIds.length > 0 && <IconButton icon={bookmarked ? BookmarkCheck : Bookmark}
        title={`${bookmarked ? 'Remove bookmark for' : 'Bookmark'} ${group ? 'group' : 'image'}`}
        aria-pressed={bookmarked} disabled={props.bookmarkBusy}
        className={`small ${bookmarked ? 'bookmarked' : ''}`}
        onClick={() => props.onBookmark(archiveIds)} />}
      <IconButton icon={restore ? Eye : EyeOff} title={`${restore ? 'Restore' : 'Dismiss'} ${group ? 'group' : 'image'}`}
        className="small" disabled={props.busy} onClick={() => onTriage(ids, 'dismiss')} />
    </div>;
  };
  const card = (group: CatalogGroup, variant: 'main' | 'soft' | 'dismissed' = 'main') =>
    <article className="group-card" key={`${group.id}-${variant}`}
      id={`group-${group.originalIndex}${variant === 'main' ? '' : `-${variant}`}`} data-group={group.originalIndex}
      onMouseEnter={() => onHover(group.originalIndex)} onMouseLeave={() => onHover(null)}>
      <div className="group-header" onContextMenu={event => context(event, group.observations.map(o => o.obs_id))}><div><h3>{group.targets?.join(' · ') || group.field || group.title || 'Untitled field'}</h3>
        <div className="group-meta">{group.collection} · {group.instrument}{group.proposal ? ` · ${group.proposal}` : ''}</div></div>
        {triageButtons(group.observations, true)}
        <time>{(group.date ?? '').slice(0, 10)}</time>
      </div>
      <div className="thumbnails">
        {group.observations.filter((o) => variant === 'dismissed' || !dismissed.has(o.obs_id)).map((o) =>
          <div key={o.obs_id} data-testid="observation" data-obs={o.obs_id} style={waveStyle(o.wavelength_nm)}
            onContextMenu={event => context(event, [o.obs_id], o)}
            onMouseEnter={() => onHover(o.obs_id)} onMouseLeave={() => onHover(group.originalIndex)}
            onFocus={() => onHover(o.obs_id)} onBlur={() => onHover(null)}
            className={`observation ${o.soft ? 'soft-survey' : ''} ${selected.has(o.obs_id) ? 'selected' : ''} ${dismissed.has(o.obs_id) ? 'dismissed' : ''} ${props.flash === o.obs_id ? 'jump-flash' : ''}`}>
            <button className="thumb-select" title={`${o.filter} · ${o.info?.shows || o.target}${o.soft_reason ? ` · ${o.soft_reason}` : ''}`}
              aria-label={`Select ${o.filter} ${o.obs_id}`} aria-pressed={selected.has(o.obs_id)} disabled={props.busy}
              onClick={() => onToggle(o.obs_id)}>
              <img src={previewUrl(o, 'thumb', props.view)} loading="lazy" decoding="async" alt={o.filter}
                onLoad={(event) => {
                  event.currentTarget.classList.remove('unavailable');
                  event.currentTarget.parentElement?.classList.remove('preview-unavailable');
                }}
                onError={(event) => {
                  event.currentTarget.classList.add('unavailable');
                  event.currentTarget.parentElement?.classList.add('preview-unavailable');
                }} />
              <span className="check">{selected.has(o.obs_id) && <Check size={12} />}</span>
              {favorites.has(o.obs_id) && <span className="favorite-badge"><Star size={14} fill="currentColor" /></span>}
            </button>
            {triageButtons([o])}
            <div className="thumb-label"><span className="filter-chip">{o.filter}</span>
              <IconButton icon={ZoomIn} title={`Preview ${o.filter}`} onClick={() => onPreview(o)} />
            </div>
          </div>)}
      </div>
    </article>;
  return <div id="groups" className="groups">
    {primary.map((group) => card(group))}
    {softCount > 0 && <details className="soft-surveys" open={props.showSoft}
      onToggle={event => props.onSoft(event.currentTarget.open)}>
      <summary><ChevronDown aria-hidden />Lower-resolution surveys <span className="mono">{softCount}</span></summary>
      {softer.map(group => card(group, 'soft'))}
    </details>}
    {hiddenCount > 0 && <details id="dismissed-groups" data-testid="dismissed-groups" open={props.showDismissed}
      onToggle={(event) => props.onDismissed(event.currentTarget.open)}>
      <summary>Dismissed ({hiddenCount})</summary>{[...hidden, ...partial].map((group) => card(group, 'dismissed'))}
    </details>}
    {!groups.length && <div className="catalog-empty">No matching observations</div>}
    {menu && <ContextMenu label="Observation actions" position={menu} items={menu.items} onClose={() => setMenu(null)} />}
  </div>;
}
