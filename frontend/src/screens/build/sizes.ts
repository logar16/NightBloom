import type { Job, PlanFile } from '../../api/types';
import { bytes } from '../sky/utils';

export const transferBytes = (file: PlanFile) => file.cached ? 0
  : file.transfer_bytes === undefined ? file.size_bytes : file.transfer_bytes;
export const transferredBytes = (file: PlanFile) => file.cached ? 0
  : file.transferred_bytes ?? file.done_bytes ?? 0;

export function transferTotal(files: PlanFile[]) {
  const amounts = files.map(transferBytes);
  return amounts.some(value => value == null) ? null : amounts.reduce<number>((sum, value) => sum + (value ?? 0), 0);
}

export function transferLabel(files: PlanFile[]) {
  const total = files.reduce((sum, file) => sum + (transferBytes(file) ?? 0), 0);
  const unknown = files.some(file => transferBytes(file) == null);
  if (unknown && total === 0) return bytes(null);
  if (total === 0) return '0 B';
  const approximate = files.some(file => !file.cached && file.transfer_estimated);
  return `${approximate ? '~' : ''}${bytes(total)}${unknown ? ' + unknown' : ''}`;
}

export function jobProgress(job: Job) {
  const files = job.files ?? [];
  const total = transferTotal(files);
  if (job.kind !== 'build' || (job.phase ?? job.stage) !== 'download' || !total) return job.progress;
  const done = files.reduce((sum, file) => sum + transferredBytes(file), 0);
  return Math.min(files.every(file => file.state === 'cached' || file.state === 'done') ? 1 : .99, done / total);
}

export function sourceFieldsLabel(file: PlanFile) {
  const fields = file.source_fields;
  if (!fields?.length) return '';
  const cached = fields.filter(field => field.cached).length;
  const min = bytes(Math.min(...fields.map(field => field.size_bytes)));
  const max = bytes(Math.max(...fields.map(field => field.size_bytes)));
  return [`${fields.length} field${fields.length === 1 ? '' : 's'}`, `~${min === max ? min : `${min}–${max}`}/field`,
    cached < fields.length ? `${fields.length - cached} ${file.state === 'done' ? 'fetched' : 'to fetch'}` : '',
    cached ? `${cached} cached` : ''].filter(Boolean).join(' · ');
}
