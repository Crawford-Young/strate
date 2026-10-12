// Node positions for one tab's graph. ELK runs only when the structure changes
// (an agent or an edge added or removed); placed nodes stay pinned while their
// parents are unchanged. Mount it once per tab (keyed), so another tab lays
// out fresh.
import { useEffect, useEffectEvent, useState } from 'react'
import type { LiveGraph } from './api'
import { elk } from './elk'
import { layoutGraph, structureKey, type Layout, type Positions } from './graphLayout'

interface Laid {
  structure: string
  /** The latest layout that landed; kept through a failure, so a retry pins. */
  layout: Layout | undefined
  error: string | null
}

export interface GraphLayout {
  /** Null until the first layout lands; then the latest, even while the next runs. */
  positions: Positions | null
  /** Why the latest layout failed; the next structural change retries. */
  error: string | null
}

export function useGraphLayout(graph: LiveGraph): GraphLayout {
  const structure = structureKey(graph)
  const [laid, setLaid] = useState<Laid | null>(null)
  const relayout = useEffectEvent((previous?: Layout) => layoutGraph(elk(), graph, previous))
  useEffect(() => {
    if (laid?.structure === structure) return
    let current = true
    const land = (next: Omit<Laid, 'structure'>) => {
      if (current) setLaid({ structure, ...next })
    }
    relayout(laid?.layout).then(
      (layout) => land({ layout, error: null }),
      (reason: unknown) =>
        land({
          layout: laid?.layout,
          error: reason instanceof Error ? reason.message : String(reason),
        }),
    )
    return () => {
      current = false
    }
  }, [structure, laid])
  return { positions: laid?.layout?.positions ?? null, error: laid?.error ?? null }
}
