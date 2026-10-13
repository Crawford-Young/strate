import { useState } from 'react'
import { useLoad } from './hooks'

/** How many keys the cache holds; the one read longest ago goes first. */
export const CACHE_SIZE = 8

interface Cached<T> {
  /** The `version` it was read at; older is stale. */
  version: number
  value: T
}

/**
 * The result of `read` for `key`, cached per key (the last `CACHE_SIZE` read). The caller's
 * `version` is the only thing that rereads: moving it marks every cached key stale and rereads
 * the shown one at once, the stale value showing until the reread lands. A cached key is fresh
 * until then. A key not read yet shows the last value that landed meanwhile, with `pending` set.
 * Nothing is read while `active` is false: a key that went stale meanwhile is reread as soon as
 * it is active again.
 */
export function useCachedRead<T>(
  key: string,
  active: boolean,
  version: number,
  read: () => Promise<T>,
): { value: T | null; error: string | null; pending: boolean } {
  const [cache, setCache] = useState<{ byKey: ReadonlyMap<string, Cached<T>>; last?: T }>({
    byKey: new Map(),
  })
  const cached = cache.byKey.get(key)
  const pending = cached?.version !== version
  const loaded = useLoad(active && pending ? key : null, version, async () => {
    const value = await read()
    setCache((c) => {
      // A slow read can land after a newer one of the same key; the newer one stays.
      if ((c.byKey.get(key)?.version ?? -1) > version) return c
      const byKey = new Map(c.byKey)
      byKey.delete(key)
      byKey.set(key, { version, value })
      for (const old of byKey.keys()) if (byKey.size > CACHE_SIZE) byKey.delete(old)
      return { byKey, last: value }
    })
    return value
  })
  const error = loaded?.target === key && 'error' in loaded ? loaded.error : null
  return { value: cached?.value ?? cache.last ?? null, error, pending }
}
