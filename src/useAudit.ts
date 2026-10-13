import { useEffect, useState } from 'react'
import { auditRollup, type Audit, type AuditFilter } from './api'
import { useLoad } from './hooks'

/** How long the store must stay quiet before a stale rollup is reread. */
export const REFETCH_DEBOUNCE_MS = 1_000

/** `value`, once it has held for `ms`: a trailing debounce. */
export function useTrailing(value: number, ms: number): number {
  const [settled, setSettled] = useState(value)
  useEffect(() => {
    const timer = setTimeout(() => setSettled(value), ms)
    return () => clearTimeout(timer)
  }, [value, ms])
  return settled
}

interface Cached {
  /** The (debounced) store version it was read at; older is stale. */
  version: number
  audit: Audit
}

/**
 * The rollup for `filter`, cached per filter. A store change marks every cached filter stale,
 * and the shown one is reread once the store has been quiet for 1 s; a stale one still shows
 * until its reread lands. A filter not read yet shows the last rollup that landed meanwhile,
 * with `pending` set. Nothing is read while `active` is false: a filter that went stale
 * meanwhile is reread as soon as it is active again.
 *
 * `storeVersion` is `useStoreVersion()`, taken by the caller so a view with several rollups
 * listens to the store once.
 */
export function useAudit(
  filter: AuditFilter,
  active: boolean,
  storeVersion: number,
): { audit: Audit | null; error: string | null; pending: boolean } {
  const version = useTrailing(storeVersion, REFETCH_DEBOUNCE_MS)
  const [cache, setCache] = useState<{ byKey: ReadonlyMap<string, Cached>; last?: Audit }>({
    byKey: new Map(),
  })
  const key = JSON.stringify([filter.from, filter.to, filter.repos, filter.groupBy])
  const cached = cache.byKey.get(key)
  const pending = cached?.version !== version
  const loaded = useLoad(active && pending ? key : null, version, async () => {
    const audit = await auditRollup(filter)
    setCache((c) => ({ byKey: new Map(c.byKey).set(key, { version, audit }), last: audit }))
    return audit
  })
  const error = loaded?.target === key && 'error' in loaded ? loaded.error : null
  return { audit: cached?.audit ?? cache.last ?? null, error, pending }
}
