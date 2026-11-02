import { auditTimeline, type AuditFilter, type AuditPath, type TimelineDay } from './api'
import { useLoad } from './hooks'

/**
 * The timeline of the row at `path` under `filter`, or nulls with none selected. Nothing is
 * read while `active` is false (Audit hidden); moving the caller's `version` rereads at once,
 * as in `useAudit`. Another row or filter drops the old row's days at once.
 */
export function useTimeline(
  filter: AuditFilter,
  path: AuditPath | null,
  active: boolean,
  version: number,
): { days: TimelineDay[] | null; error: string | null } {
  const key = JSON.stringify([filter.from, filter.to, filter.repos, path])
  const loaded = useLoad(active && path !== null ? key : null, version, () =>
    auditTimeline(filter, path as AuditPath),
  )
  const mine = loaded?.target === key ? loaded : null
  return {
    days: mine !== null && 'value' in mine ? mine.value : null,
    error: mine !== null && 'error' in mine ? mine.error : null,
  }
}
