// Pure helpers for the Audit view: local-day ranges, number labels, and the daily chart's rows.
import type { StackedBarDatum, StackedBarSeries } from '@crawfordyoung/ui'
import type { Audit, AuditNode } from './api'

const HOUR = 3_600_000

/** What the daily chart and the $ / hours toggle measure: $, or agent-hours (subagent wall time, as the table's column). */
export type Measure = 'usd' | 'hours'

const pad = (n: number) => String(n).padStart(2, '0')

/** A date's local day, `YYYY-MM-DD`. */
export function localDay(date: Date): string {
  return `${date.getFullYear()}-${pad(date.getMonth() + 1)}-${pad(date.getDate())}`
}

/** Local midnight of a `YYYY-MM-DD` day. */
export function parseDay(day: string): Date {
  const [y, m, d] = day.split('-').map(Number) as [number, number, number]
  return new Date(y, m - 1, d)
}

// Stepping by calendar day, not 24 h, so a DST change never skips or repeats a day.
const addDays = (date: Date, days: number) =>
  new Date(date.getFullYear(), date.getMonth(), date.getDate() + days)

/** The last `n` local days, today included. */
export function lastDays(n: number, now: Date): { from: string; to: string } {
  return { from: localDay(addDays(now, 1 - n)), to: localDay(now) }
}

/** Every local day `from..=to`. */
export function dayRange(from: string, to: string): string[] {
  const days: string[] = []
  for (let d = parseDay(from); localDay(d) <= to; d = addDays(d, 1)) days.push(localDay(d))
  return days
}

/** A count shortened to `k` or `M`, one decimal. */
export function compact(n: number): string {
  if (n >= 1e6) return `${(n / 1e6).toFixed(1)}M`
  if (n >= 1e3) return `${(n / 1e3).toFixed(1)}k`
  return String(n)
}

/** Hours to at most one decimal: `1.3h`, `12h`. */
export const hoursLabel = (h: number) => `${Number(h.toFixed(1))}h`

const measured = (node: AuditNode, measure: Measure) =>
  measure === 'usd' ? node.metrics.costUsd : node.metrics.agentMs / HOUR

/** Repos (and Unlinked) under each day of a by-day rollup. */
const dayChildren = (byDay: Audit) => byDay.root.children.flatMap((day) => day.children)

/** The repos with work in a by-day rollup, sorted. */
export function repoOptions(byDay: Audit): string[] {
  const repos = dayChildren(byDay).filter((n) => n.kind === 'repo')
  return [...new Set(repos.map((n) => n.key))].sort()
}

/**
 * The daily chart from a by-day rollup: one row per day `from..=to` (days with no work are
 * empty) and one series per repo, sorted, then Unlinked. `repo` keeps that repo only. Series
 * keys are `s0`, `s1`…, since a chart reads a dotted key as a path and slugs can hold dots.
 */
export function dailyChart(
  byDay: Audit,
  measure: Measure,
  repo: string | null,
  from: string,
  to: string,
): { data: StackedBarDatum[]; series: StackedBarSeries[] } {
  const id = (n: AuditNode) => `${n.kind}:${n.key}`
  const nodes = dayChildren(byDay).filter((n) => repo === null || n.key === repo)
  const order = (n: AuditNode) => `${n.kind === 'unlinked' ? 1 : 0}${n.key}`
  const unique = new Map(nodes.map((n) => [id(n), n]))
  const sorted = [...unique.values()].sort((a, b) => order(a).localeCompare(order(b)))
  const keys = new Map(sorted.map((n, i) => [id(n), `s${i}`]))
  const series = sorted.map((n) => ({ key: keys.get(id(n)) as string, label: n.label }))

  const days = new Map(byDay.root.children.map((day) => [day.key, day]))
  const data = dayRange(from, to).map((day) => {
    const row: Record<string, unknown> = { day }
    for (const n of days.get(day)?.children ?? []) {
      const key = keys.get(id(n))
      if (key) row[key] = measured(n, measure)
    }
    return row
  })
  return { data, series }
}
