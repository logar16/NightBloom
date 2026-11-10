import { useEffect, useState } from 'react';
import { Compass, RotateCcw, RotateCw, Undo2 } from 'lucide-react';
import { useMutation } from '@tanstack/react-query';
import { api, queryClient } from '../../api/client';
import { toast } from '../../api/hooks';
import type { Job } from '../../api/types';
import { IconButton } from '../../ui';
import './rotation.css';

const wrap = (angle: number) => ((angle + 180) % 360 + 360) % 360 - 180;

export function RotationControl({ session, saved, angle, onAngle, busy }: {
  session: string; saved: number; angle: number; onAngle: (angle: number) => void; busy: boolean;
}) {
  const [draft, setDraft] = useState(String(angle));
  useEffect(() => { setDraft(String(angle)); }, [angle]);
  const mutation = useMutation({
    mutationKey: ['session-write', 'rotation', session],
    mutationFn: async (rotation: number) => {
      let job = await api<Job>('/session/rotation', 'PUT', { rotation_deg: rotation }, session);
      void queryClient.invalidateQueries({ queryKey: ['status'] });
      while (job.state === 'running' || job.state === 'queued') {
        await new Promise(resolve => setTimeout(resolve, 400));
        job = await api<Job>('/job', 'GET', undefined, session);
      }
      if (job.state !== 'done') throw Error(job.error || 'Rotation update cancelled');
    },
    onSuccess: async () => {
      await Promise.all(['session', 'sessions', 'plan'].map(key =>
        queryClient.invalidateQueries({ queryKey: [key] })));
    },
    onError: (error: Error) => { onAngle(saved); toast(error.message, session); },
  });
  const { mutate, isPending } = mutation;
  useEffect(() => {
    if (busy || isPending || angle === saved) return;
    const timer = setTimeout(() => mutate(angle), 450);
    return () => clearTimeout(timer);
  }, [angle, saved, busy, isPending, mutate]);
  const disabled = busy || isPending;
  const change = (next: number) => onAngle(Math.round(next * 10) / 10);
  return <div className="capture-rotation" aria-busy={isPending}>
    <label htmlFor="capture-angle" title="North clockwise from up · +90° puts north right.">
      <Compass size={14} />Rotation
    </label>
    <div className="capture-angle-controls">
      <IconButton icon={RotateCcw} title="Rotate view left 90°" className="small" disabled={disabled}
        onClick={() => change(wrap(angle - 90))} />
      <input id="capture-angle" aria-label="Capture rotation" type="number" min="-180" max="180" step="0.1"
        title="Degrees east of north · Arrow keys nudge 0.1°" value={draft} disabled={disabled}
        onChange={event => {
          setDraft(event.target.value);
          const value = event.target.valueAsNumber;
          if (Number.isFinite(value) && value >= -180 && value <= 180) onAngle(value);
        }}
        onBlur={() => {
          const value = Math.min(180, Math.max(-180, Number(draft) || 0));
          change(value); setDraft(String(value));
        }}
        onKeyDown={event => {
          if (['ArrowLeft', 'ArrowDown', 'ArrowRight', 'ArrowUp'].includes(event.key)) {
            event.preventDefault();
            change(Math.min(180, Math.max(-180, angle + (['ArrowLeft', 'ArrowDown'].includes(event.key) ? -.1 : .1))));
          }
        }} />
      <span className="capture-angle-unit" aria-hidden="true">°</span>
      <IconButton icon={RotateCw} title="Rotate view right 90°" className="small" disabled={disabled}
        onClick={() => change(wrap(angle + 90))} />
      <IconButton icon={Undo2} title="Reset capture rotation" className="small" disabled={disabled || angle === 0}
        onClick={() => change(0)} />
    </div>
  </div>;
}

export function CaptureCompass({ angle }: { angle: number }) {
  const radians = angle * Math.PI / 180;
  const n = [Math.sin(radians), -Math.cos(radians)];
  const e = [-Math.cos(radians), -Math.sin(radians)];
  return <svg className="capture-compass" viewBox="0 0 90 90" role="img"
    aria-label={`NightBloom compass, ${angle}°`} data-angle={angle}>
    <title>NightBloom · North clockwise from up</title>
    <circle cx="45" cy="45" r="2" />
    {[[n, 'N'], [e, 'E']].map(([vector, label]) => {
      const [x, y] = vector as number[];
      return <g key={label as string} data-direction={label}>
        <path d={`M45 45 L${45 + x * 25} ${45 + y * 25}`} />
        <text x={45 + x * 36} y={45 + y * 36}>{label as string}</text>
      </g>;
    })}
    <text className="capture-compass-label" x="45" y="86">NightBloom</text>
  </svg>;
}
