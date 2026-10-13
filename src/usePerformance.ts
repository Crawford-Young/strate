import { auditPerformance, type AuditFilter, type Performance } from './api'
import { useCachedRead } from './useCachedRead'

/**
 * The performance read for `filter`'s days and repos, cached and reread as `useAudit` does.
 * The grouping is not part of the read, so changing it neither rereads nor marks it pending.
 */
export function usePerformance(
  filter: AuditFilter,
  active: boolean,
  version: number,
): { performance: Performance | null; error: string | null; pending: boolean } {
  const key = JSON.stringify([filter.from, filter.to, filter.repos])
  const { value, error, pending } = useCachedRead(key, active, version, () =>
    auditPerformance(filter),
  )
  return { performance: value, error, pending }
}
