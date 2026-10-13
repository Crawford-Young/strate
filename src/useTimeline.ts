import { auditTimeline, type AuditFilter, type AuditPath, type TimelineDay } from './api'
import { useLoad } from './hooks'
import { REFETCH_DEBOUNCE_MS, useTrailing } from './useAudit'

/**
 * The timeline of the row at `path` under `filter`, or nulls with none selected. Nothing is
 * read while `active` is false (Audit hidden); a store change rereads after the same 1 s
 * trailing debounce as `useAudit`. Another row or filter drops the old row's days at once.
 *
 * `storeVersion` is `useStoreVersion()`, taken by the caller so the view listens once.
 */
export function useTimeline(
  filter: AuditFilter,
  path: AuditPath | null,
  active: boolean,
  storeVersion: number,
): { days: TimelineDay[] | null; error: string | null } {
  const version = useTrailing(storeVersion, REFETCH_DEBOUNCE_MS)
  const key = JSON.stringify([filter.from, filter.to, filter.repos, filter.groupBy, path])
  const loaded = useLoad(active && path !== null ? key : null, version, () =>
    auditTimeline(filter, path as AuditPath),
  )
  const mine = loaded?.target === key ? loaded : null
  return {
    days: mine !== null && 'value' in mine ? mine.value : null,
    error: mine !== null && 'error' in mine ? mine.error : null,
  }
}
