// Display formatting for the shell, following the mockup's copy.
import type { AgentNodeState } from '@crawfordyoung/ui'
import type { AgentState, LiveGraph, Workstream } from './api'

/** Dollars to the cent; a dash when nothing is priced. */
export function money(usd: number | null): string {
  return usd === null ? '$—' : `$${usd.toFixed(2)}`
}

/** Whole minutes elapsed as `Xm`, `Xh Ym` or `Xd Yh`. */
export function duration(ms: number): string {
  const min = Math.max(0, Math.floor(ms / 60_000))
  const d = Math.floor(min / 1440)
  const h = Math.floor((min % 1440) / 60)
  const m = min % 60
  if (d) return `${d}d ${h}h`
  return h ? `${h}h ${m}m` : `${m}m`
}

/** The library spells `needs_you` as `needs-you`. */
export function nodeState(state: AgentState): AgentNodeState {
  return state === 'needs_you' ? 'needs-you' : state
}

/** Every directed unit's root and the agents beneath it: other sessions, drawn here. */
function directedAgents(graph: LiveGraph): Set<number> {
  const out = new Set<number>()
  const queue = graph.edges.filter((e) => e.kind === 'directs').map((e) => e.child)
  for (let id = queue.pop(); id !== undefined; id = queue.pop()) {
    if (out.has(id)) continue
    out.add(id)
    for (const e of graph.edges) if (e.parent === id) queue.push(e.child)
  }
  return out
}

/**
 * $ of the drawn session: each agent's `costUsd` sums its own requests only,
 * so the agents add up to the session without double counting. Directed units
 * and their agents are drawn on a director's canvas but are their own sessions.
 */
export function sessionCost(graph: LiveGraph | null): number | null {
  const units = graph ? directedAgents(graph) : new Set<number>()
  const priced = (graph?.agents ?? []).flatMap((a) =>
    a.costUsd === null || units.has(a.id) ? [] : [a.costUsd],
  )
  return priced.length ? priced.reduce((sum, usd) => sum + usd, 0) : null
}

/** The line over the canvas: running time, session $, total $, needs-you. */
export function sessionLine(ws: Workstream, graph: LiveGraph | null, now: number): string {
  const running = ws.startedAt ? `running ${duration(now - Date.parse(ws.startedAt))}` : 'running'
  const parts = [
    ws.live ? running : 'finished',
    `${money(sessionCost(graph))} this session`,
    `${money(ws.costUsd)} total`,
  ]
  if (ws.needsYou > 0) parts.push(`${ws.needsYou} needs you`)
  return parts.join(' · ')
}
