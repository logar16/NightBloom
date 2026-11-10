import { useId, useRef, useState } from 'react';
import { ChevronDown } from 'lucide-react';
import { Popover } from './common';
import type { Option, ValueProps } from './common';

export function Select<T extends string>({ label = 'Select', options, value, onInput, onChange }:
  ValueProps<T> & { label?: string; options: Option<T>[] }) {
  const anchor = useRef<HTMLButtonElement>(null);
  const [open, setOpen] = useState(false);
  const [active, setActive] = useState(0);
  const search = useRef({ text: '', at: 0 });
  const id = useId();
  const show = () => {
    setOpen(true); setActive(Math.max(0, options.findIndex((option) => option.value === value)));
  };
  const choose = (index: number) => {
    const option = options[index];
    if (!option) return;
    onInput?.(option.value); onChange?.(option.value); setOpen(false); anchor.current?.focus();
  };
  const content = (option?: Option<T>) => <span className="ui-option-main">
    {option?.icon && <span className="ui-option-icon">{option.icon}</span>}
    {option?.swatch && <span className="ui-option-swatch" style={{ background: option.swatch }} />}
    <span className="ui-option-label">{option?.label ?? label}</span>
  </span>;
  return <div className="ui-control ui-select">
    <button type="button" ref={anchor} role="combobox" aria-label={label} aria-haspopup="listbox"
      aria-expanded={open} aria-controls={open ? id : undefined}
      aria-activedescendant={open ? `${id}-${active}` : undefined} className="ui-select-trigger"
      onClick={() => open ? setOpen(false) : show()} onKeyDown={(event) => {
        if (event.key === 'Escape' || event.key === 'Tab') { setOpen(false); return; }
        if (['ArrowDown', 'ArrowUp', 'Home', 'End', 'Enter', ' '].includes(event.key)) {
          event.preventDefault();
          if (!open) show();
          else if (event.key === 'Enter' || event.key === ' ') choose(active);
          else setActive(event.key === 'Home' ? 0 : event.key === 'End' ? options.length - 1
            : (active + (event.key === 'ArrowUp' ? -1 : 1) + options.length) % options.length);
        } else if (event.key.length === 1 && !event.ctrlKey && !event.metaKey && !event.altKey) {
          event.preventDefault();
          const now = performance.now();
          const text = now - search.current.at > 650 ? event.key : search.current.text + event.key;
          search.current = { text, at: now };
          if (!open) show();
          const index = options.findIndex((option) => option.label.toLowerCase().startsWith(text.toLowerCase()));
          if (index >= 0) setActive(index);
        }
      }}>{content(options.find((option) => option.value === value))}<ChevronDown size={12} /></button>
    {open && <Popover anchor={anchor} onClose={() => setOpen(false)}>
      <div id={id} className="ui-select-list" role="listbox" aria-label={label}
        style={{ minWidth: anchor.current?.getBoundingClientRect().width }}>
        {options.map((option, index) => <div key={option.value} id={`${id}-${index}`}
          role="option" aria-selected={option.value === value}
          ref={(element) => { if (index === active) element?.scrollIntoView({ block: 'nearest' }); }}
          className={`ui-select-option ${index === active ? 'ui-active' : ''}`}
          onPointerMove={() => setActive(index)} onClick={() => choose(index)}>
          {content(option)}{option.description && <span className="ui-option-description">{option.description}</span>}
        </div>)}
      </div>
    </Popover>}
  </div>;
}
