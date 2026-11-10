import type { ReactNode } from 'react';
import { CloudOff } from 'lucide-react';
import { useStatus } from './api/hooks';

const SERVICE_IMPACT = {
  cds: 'Survey search, previews and downloads are unavailable; bands already downloaded still build.',
  mast: 'HST/JWST search and downloads are unavailable; files already downloaded still build.',
};

export function StatusBar({ children }: { children?: ReactNode }) {
  const { data: status } = useStatus();
  return <footer className="studio-status">
    {children && <div className="workspace-status">{children}</div>}
    <div className="connection-status" aria-label="Connections">
      {Object.entries(status?.services ?? {}).filter(([, service]) => service.ok === false).map(([name, service]) =>
        <span key={name} id={`service-${name}`} className="connection down" role="status"
          aria-label={`${name.toUpperCase()} unreachable`}
          title={`${name.toUpperCase()} unreachable. ${SERVICE_IMPACT[name as keyof typeof SERVICE_IMPACT]}${
            service.checked ? ` Last checked ${new Date(service.checked).toLocaleTimeString()}.` : ''}`}>
          <CloudOff size={14} aria-hidden />
        </span>)}
      <span id="aladin" className={`connection ${status?.aladin ? 'connected' : ''}`}
        title={status?.aladin ? 'Connected to Aladin Desktop' : 'Open Aladin Desktop with its SAMP hub enabled'}>
        <i />{status?.aladin ? 'Aladin' : 'Aladin offline'}
      </span>
    </div>
  </footer>;
}
