import { auditRollup, type Audit, type AuditFilter } from './api'
import { useCachedRead } from './useCachedRead'

/**
 * The rollup for `filter`, cached per filter (`useCachedRead`): only the caller's `version`
 * rereads, a filter not read yet shows the last rollup that landed with `pending` set, and
 * nothing is read while `active` is false.
 */
export function useAudit(
  filter: AuditFilter,
  active: boolean,
  version: number,
): { audit: Audit | null; error: string | null; pending: boolean } {
  const key = JSON.stringify([filter.from, filter.to, filter.repos])
  const { value, error, pending } = useCachedRead(key, active, version, () => auditRollup(filter))
  return { audit: value, error, pending }
}
