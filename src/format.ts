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

/**
 * The line over the canvas: running time, session $, total $, needs-you. The session $ is only the
 * part of the drawn session in this workstream, so it never exceeds the total. The running time is
 * the session's run time as read, plus `sinceReadMs` of local time since.
 */
export function sessionLine(ws: Workstream, graph: LiveGraph | null, sinceReadMs: number): string {
  const runMs = graph?.runMs ?? null
  const running = runMs === null ? 'running' : `running ${duration(runMs + sinceReadMs)}`
  const parts = [
    ws.live ? running : 'finished',
    `${money(graph?.sessionCostUsd ?? null)} this session`,
    `${money(ws.costUsd)} total`,
  ]
  if (ws.needsYou > 0) parts.push(`${ws.needsYou} needs you`)
  return parts.join(' · ')
}
