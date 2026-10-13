import { useState } from 'react'
import { auditRollup, type Audit, type AuditFilter } from './api'
import { useLoad } from './hooks'

/** How many filters the cache holds; the one read longest ago goes first. */
export const CACHE_SIZE = 8

interface Cached {
  /** The `version` it was read at; older is stale. */
  version: number
  audit: Audit
}

/**
 * The rollup for `filter`, cached per filter (the last `CACHE_SIZE` read). The caller's
 * `version` is the only thing that rereads: moving it marks every cached filter stale and rereads
 * the shown one at once, the stale rollup showing until the reread lands. A cached filter is
 * fresh until then. A filter not read yet shows the last rollup that landed meanwhile, with
 * `pending` set. Nothing is read while `active` is false: a filter that went stale meanwhile is
 * reread as soon as it is active again.
 */
export function useAudit(
  filter: AuditFilter,
  active: boolean,
  version: number,
): { audit: Audit | null; error: string | null; pending: boolean } {
  const [cache, setCache] = useState<{ byKey: ReadonlyMap<string, Cached>; last?: Audit }>({
    byKey: new Map(),
  })
  const key = JSON.stringify([filter.from, filter.to, filter.repos, filter.groupBy])
  const cached = cache.byKey.get(key)
  const pending = cached?.version !== version
  const loaded = useLoad(active && pending ? key : null, version, async () => {
    const audit = await auditRollup(filter)
    setCache((c) => {
      // A slow read can land after a newer one of the same filter; the newer one stays.
      if ((c.byKey.get(key)?.version ?? -1) > version) return c
      const byKey = new Map(c.byKey)
      byKey.delete(key)
      byKey.set(key, { version, audit })
      for (const old of byKey.keys()) if (byKey.size > CACHE_SIZE) byKey.delete(old)
      return { byKey, last: audit }
    })
    return audit
  })
  const error = loaded?.target === key && 'error' in loaded ? loaded.error : null
  return { audit: cached?.audit ?? cache.last ?? null, error, pending }
}
