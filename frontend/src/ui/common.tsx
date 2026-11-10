import { createContext, useContext, useId, useLayoutEffect, useRef, useState } from 'react';
import type { ButtonHTMLAttributes, CSSProperties, ReactNode, RefObject } from 'react';
import { createPortal } from 'react-dom';
import type { LucideIcon } from 'lucide-react';

export const clamp = (value: number, min = 0, max = 1) => Math.min(max, Math.max(min, value));
export interface ValueProps<T> {
  value: T;
  onInput?: (value: T) => void;
  onChange?: (value: T) => void;
}
export interface Option<T extends string = string> {
  value: T; label: string; icon?: ReactNode; title?: string;
  swatch?: string; description?: string;
}
const PopoverOwners = createContext<string[]>([]);
export function IconButton({ icon: Icon, title, ...props }:
  ButtonHTMLAttributes<HTMLButtonElement> & { icon: LucideIcon; title: string }) {
  return <button type="button" aria-label={title} title={title} {...props}
    className={`icon ${props.className ?? ''}`}><Icon /></button>;
}
export function Popover({ anchor, onClose, children, placement = 'bottom-start', className = '' }: {
  anchor: RefObject<HTMLElement | null>; onClose: () => void; children: ReactNode;
  placement?: string; className?: string;
}) {
  const ancestors = useContext(PopoverOwners);
  const id = useId().replace(/[^a-z0-9-]/gi, '');
  const panel = useRef<HTMLDivElement>(null);
  const close = useRef(onClose);
  close.current = onClose;
  const [style, setStyle] = useState<CSSProperties>({ visibility: 'hidden' });
  useLayoutEffect(() => {
    const position = () => {
      if (!anchor.current || !panel.current) return;
      const a = anchor.current.getBoundingClientRect();
      const p = panel.current.getBoundingClientRect();
      const below = innerHeight - a.bottom - 8;
      const above = a.top - 8;
      const top = placement.startsWith('top')
        ? above >= p.height || above > below : below < p.height && above > below;
      setStyle({
        left: clamp(placement.endsWith('end') ? a.right - p.width : a.left,
          8, Math.max(8, innerWidth - p.width - 8)),
        top: top ? Math.max(8, a.top - p.height - 7) : a.bottom + 7,
        maxHeight: Math.max(0, (top ? above : below) - 7),
      });
    };
    const outside = (event: Event) => {
      const target = event.target as Node;
      const nested = target instanceof Element && target.closest(`[data-popover-owners~="${id}"]`);
      if (!panel.current?.contains(target) && !anchor.current?.contains(target) && !nested) close.current();
    };
    const key = (event: KeyboardEvent) => {
      if (event.key === 'Escape' && !event.defaultPrevented) {
        if ([...document.querySelectorAll('.ui-popover')].at(-1) !== panel.current) return;
        event.preventDefault(); close.current(); anchor.current?.focus();
      }
    };
    const observer = new ResizeObserver(position);
    if (panel.current) observer.observe(panel.current);
    position();
    document.addEventListener('pointerdown', outside, true);
    document.addEventListener('focusin', outside);
    document.addEventListener('keydown', key);
    window.addEventListener('resize', position);
    window.addEventListener('scroll', position, true);
    return () => {
      observer.disconnect();
      document.removeEventListener('pointerdown', outside, true);
      document.removeEventListener('focusin', outside);
      document.removeEventListener('keydown', key);
      window.removeEventListener('resize', position);
      window.removeEventListener('scroll', position, true);
    };
  }, [anchor, placement, id]);
  return createPortal(<PopoverOwners value={[...ancestors, id]}>
    <div ref={panel} style={style} data-popover-owners={ancestors.join(' ')}
      className={`ui-popover ${className}`}>{children}</div>
  </PopoverOwners>, document.body);
}
export function Segmented<T extends string>({ options, value, onInput, onChange, label = 'Options' }:
  ValueProps<T> & { options: Option<T>[]; label?: string }) {
  return <div className="ui-control ui-segmented" role="radiogroup" aria-label={label}>
    {options.map((option, index) => <button key={option.value} type="button" role="radio"
      aria-checked={option.value === value} title={option.title} className="ui-segment"
      tabIndex={option.value === value ? 0 : -1}
      onClick={() => { onInput?.(option.value); onChange?.(option.value); }}
      onKeyDown={(event) => {
        if (!['ArrowRight', 'ArrowDown', 'ArrowLeft', 'ArrowUp', 'Home', 'End'].includes(event.key)) return;
        event.preventDefault();
        const next = event.key === 'Home' ? 0 : event.key === 'End' ? options.length - 1
          : (index + (['ArrowRight', 'ArrowDown'].includes(event.key) ? 1 : -1) + options.length) % options.length;
        onInput?.(options[next].value); onChange?.(options[next].value);
        (event.currentTarget.parentElement?.children[next] as HTMLElement)?.focus();
      }}>{option.icon}{option.label}</button>)}
  </div>;
}
