import { useRef, useState } from 'react';
import { Activity, X } from 'lucide-react';
import { useCancelJob, useOpenSession, useShell, useStatus } from './api/hooks';
import { activeJob, projectColor, projectHref } from './api/shell';
import { IconButton, Popover } from './ui';
import { eta } from './screens/sky/utils';
import { jobProgress } from './screens/build/sizes';
import './jobs.css';

export function JobsTray() {
  const { data: status } = useStatus();
  const [visible, setVisible] = useState(false);
  const anchor = useRef<HTMLButtonElement>(null);
  const cancel = useCancelJob(), open = useOpenSession();
  const jobs = status?.jobs ?? [];
  const active = jobs.filter(activeJob), count = active.length;
  // The ring follows this project's job when it has one, otherwise the mean of all active jobs.
  const current = status?.job && activeJob(status.job) ? status.job : null;
  const progress = current ? jobProgress(current) : count ? active.reduce((sum, job) => sum + jobProgress(job), 0) / count : 0;
  const summary = current ? `${current.stage || current.kind.replaceAll('-', ' ')} ${Math.round(progress * 100)}%` : '';
  return <>
    <button ref={anchor} className={`icon jobs-toggle ${count ? 'busy' : ''}`}
      title={summary ? `Project jobs · ${summary}` : 'Project jobs'}
      aria-label={`Project jobs${count ? ` (${count})` : ''}${summary ? `, ${summary}` : ''}`}
      aria-expanded={visible} onClick={() => setVisible(!visible)}>
      {count > 0 && <svg className="jobs-ring" viewBox="0 0 36 36" aria-hidden>
        <circle cx="18" cy="18" r="16" pathLength="100" />
        <circle cx="18" cy="18" r="16" pathLength="100" style={{ strokeDasharray: `${progress * 100} 100` }} />
      </svg>}
      <Activity />{count > 0 && <span className="jobs-count">{count}</span>}
    </button>
    {visible && <Popover anchor={anchor} onClose={() => setVisible(false)} className="jobs-tray">
      <div className="popover-title">Project jobs<span className="mono">{count} active</span></div>
      {!jobs.length && <p className="muted">No recent jobs</p>}
      {[...jobs.filter(activeJob), ...jobs.filter(job => !activeJob(job)).reverse()].map(job => {
        const screen = ['capture', 'view-rotation', 'search-mast', 'search-surveys'].includes(job.kind) ? 'sky'
          : job.kind.startsWith('export') ? 'edit' : 'build';
        return <div key={job.id} className={`project-job ${job.state}`} style={{ '--project-color': projectColor(job.session) } as React.CSSProperties}>
          <a href={projectHref(job.session, screen)} onClick={event => {
            if (event.button !== 0 || event.ctrlKey || event.metaKey || event.shiftKey || event.altKey) return;
            event.preventDefault(); setVisible(false);
            open.mutate(job.session, { onSuccess: () => useShell.getState().setScreen(screen) });
          }}>
            <div className="project-job-title"><i className="project-tag" /><strong>{job.name}</strong>
              <span className="mono">{Math.round(jobProgress(job) * 100)}%</span></div>
            <div className="project-job-meta"><span>{job.kind.replaceAll('-', ' ')}</span><span>
              {activeJob(job) && job.cancel_requested ? 'Cancelling…'
                : job.state === 'queued' ? 'Queued' : activeJob(job) ? job.stage || 'Starting' : job.state}
              {activeJob(job) && job.eta_s != null ? ` · ${eta(job.eta_s)}`
                : activeJob(job) && job.kind === 'build' && ['align', 'publish'].includes(job.phase ?? job.stage) ? ' · Estimating…' : ''}
            </span></div>
            {job.state === 'queued' && <small>{job.message}{job.waiting_for
              ? ` (${job.waiting_for.stage} ${Math.round(job.waiting_for.progress * 100)}%)` : ''}</small>}
            {job.state === 'error' && <small>{job.error}</small>}
            <div className="project-job-progress"><i style={{ width: `${jobProgress(job) * 100}%` }} /></div>
          </a>
          {activeJob(job) && <IconButton icon={X} title={`Cancel ${job.name}`} disabled={job.cancel_requested}
            onClick={() => cancel.mutate(job.id)} />}
        </div>;
      })}
    </Popover>}
  </>;
}
