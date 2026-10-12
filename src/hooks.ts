import { useEffect, useEffectEvent, useRef, useState } from 'react'
import { onStoreChanged } from './api'

/** Bumps on every `store-changed` event; the shell refetches on it, never on a timer. */
export function useStoreVersion(): number {
  const [version, setVersion] = useState(0)
  useEffect(() => {
    let mounted = true
    let unlisten: (() => void) | undefined
    void onStoreChanged(() => setVersion((v) => v + 1)).then((stop) => {
      if (mounted) unlisten = stop
      else stop()
    })
    return () => {
      mounted = false
      unlisten?.()
    }
  }, [])
  return version
}

type Loaded<T> = { target: string; value: T } | { target: string; error: string }

/**
 * The latest result of `load` for `target`.
 *
 * A new `target` starts a read at once and drops any result still due for the
 * old one. A `version` bump never drops a read in flight: the store changes up
 * to 4 times a second, and dropping would starve a read slower than that.
 * Instead bumps coalesce into one rerun after the read in flight lands, so at
 * most one read per target is in flight and results land in order. A null
 * `target` loads nothing.
 */
export function useLoad<T>(
  target: string | null,
  version: number,
  load: () => Promise<T>,
): Loaded<T> | null {
  const [loaded, setLoaded] = useState<Loaded<T> | null>(null)
  const run = useEffectEvent(load)
  const reads = useRef({ generation: 0, target: null as string | null, busy: false, rerun: false })
  useEffect(() => {
    if (target === null) return
    const r = reads.current
    if (r.busy && r.target === target) {
      r.rerun = true
      return
    }
    const generation = ++r.generation
    Object.assign(r, { target, busy: true, rerun: false })
    const read = () => run().then(settle, (e: unknown) => settle(undefined, String(e)))
    const settle = (value?: T, error?: string) => {
      if (r.generation !== generation) return
      setLoaded(error === undefined ? { target, value: value as T } : { target, error })
      r.busy = r.rerun
      r.rerun = false
      if (r.busy) void read()
    }
    void read()
  }, [target, version])
  return loaded
}

/**
 * The current time, updated each time a whole `periodMs` has passed since
 * `sinceMs`: a minute display that counts from a start ticks once a minute,
 * exactly when its number changes.
 */
export function useClock(sinceMs: number, periodMs = 60_000): number {
  const [now, setNow] = useState(Date.now)
  useEffect(() => {
    let timer: ReturnType<typeof setTimeout>
    const schedule = () => {
      const elapsed = Date.now() - sinceMs
      const wait = periodMs - (((elapsed % periodMs) + periodMs) % periodMs)
      timer = setTimeout(() => {
        setNow(Date.now())
        schedule()
      }, wait)
    }
    schedule()
    return () => clearTimeout(timer)
  }, [sinceMs, periodMs])
  return now
}
