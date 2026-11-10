import { useEffect, useRef, useState, type DragEvent } from 'react';
import { useIsMutating } from '@tanstack/react-query';
import { ImageUp } from 'lucide-react';
import { toast, useRestoreExportLook } from '../../api/hooks';
import { readExportLink } from '../../api/png-links';

export function useExportImageOpen() {
  const restore = useRestoreExportLook();
  const [reading, setReading] = useState(false);
  const pending = useRef(false);
  const changing = useIsMutating({ mutationKey: ['session-open'] }) > 0;
  const saving = useIsMutating({ mutationKey: ['session-write'] }) > 0;
  const busy = reading || changing || saving;
  const open = async (file: File) => {
    if (pending.current || changing || saving) { toast('Wait for the current project change to finish.'); return; }
    pending.current = true; setReading(true);
    const finished = () => { pending.current = false; setReading(false); };
    try {
      const link = await readExportLink(file);
      restore.mutate(link, { onSettled: finished });
    }
    catch (error) {
      toast(error instanceof Error ? error.message : String(error));
      finished();
    }
  };
  useEffect(() => {
    const paste = (event: ClipboardEvent) => {
      const file = event.clipboardData?.files[0];
      if (file) { event.preventDefault(); void open(file); }
    };
    document.addEventListener('paste', paste);
    return () => document.removeEventListener('paste', paste);
  });
  return {
    open, busy,
    handlers: {
      onDragOver: (event: DragEvent<HTMLElement>) => {
        if (event.dataTransfer.types.includes('Files')) event.preventDefault();
      },
      onDrop: (event: DragEvent<HTMLElement>) => {
        const file = event.dataTransfer.files[0];
        if (file) { event.preventDefault(); void open(file); }
      },
    },
  };
}

export function ExportImagePicker({ opener }: { opener: ReturnType<typeof useExportImageOpen> }) {
  const input = useRef<HTMLInputElement>(null);
  return <>
    <button className="home-rail-action home-upload" title="Open exported PNG (or paste/drop on Home)"
      aria-label="Open exported PNG" disabled={opener.busy} onClick={() => input.current?.click()}>
      <span className="home-action-icon"><ImageUp /></span><span className="home-rail-label">Open image</span>
    </button>
    <input ref={input} hidden type="file" accept=".png,image/png" aria-label="Exported PNG file"
      onChange={event => {
        const file = event.target.files?.[0];
        event.target.value = '';
        if (file) void opener.open(file);
      }} />
  </>;
}
