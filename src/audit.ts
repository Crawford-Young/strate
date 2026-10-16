// Pure helpers for the Audit view: local-day ranges, number labels, the repo chips, the daily
// chart's rows and the breakdown columns' colours and segments.
import type {
  ChartToken,
  InlineStackedBarSegment,
  StackedBarDatum,
  StackedBarSeries,
} from '@crawfordyoung/ui'
import type { Audit, AuditNode, AuditPath, Slice } from './api'

const HOUR = 3_600_000

/** What the chart and breakdown columns measure: $, or agent-hours (subagent wall time, as the table's column). */
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

const shortDayFormat = new Intl.DateTimeFormat(undefined, { month: 'short', day: 'numeric' })

/** A `YYYY-MM-DD` day as a short local date: `Oct 5`. */
export const shortDay = (day: string) => shortDayFormat.format(parseDay(day))

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

/** Whether `root` holds a node at `path` (its `[kind, key]` steps down from `root`). */
export function hasPath(root: AuditNode, path: AuditPath): boolean {
  let at: AuditNode | undefined = root
  for (const [kind, key] of path) at = at?.children.find((n) => n.kind === kind && n.key === key)
  return at !== undefined
}

/** A node's or slice's measure: $, or agent-hours in hours. */
export const measured = (m: { costUsd: number; agentMs: number }, measure: Measure) =>
  measure === 'usd' ? m.costUsd : m.agentMs / HOUR

/** The repo filter's slug for the windows in no repo. */
export const UNLINKED = ''

/**
 * The repo chips: the repos the read lists (every one in range, whatever the filter), sorted,
 * then Unlinked when the range has any. A picked one the range no longer holds stays, so it
 * can be unpicked.
 */
export function repoChips(read: Audit | null, picked: string[]): string[] {
  const repos = new Set([...(read?.repos ?? []), ...picked.filter((r) => r !== UNLINKED)])
  const unlinked = read?.unlinked || picked.includes(UNLINKED)
  return [...[...repos].sort(), ...(unlinked ? [UNLINKED] : [])]
}

/**
 * The daily chart: one row per day `from..=to` (days with no work are empty) and one series
 * per top row of the tree (its repos, then Unlinked), so it charts what the tree shows under
 * any filter. Series keys are `s0`, `s1`…, since a chart reads a dotted key as a path and
 * slugs can hold dots.
 */
export function dailyChart(
  read: Audit,
  measure: Measure,
  from: string,
  to: string,
): { data: StackedBarDatum[]; series: StackedBarSeries[] } {
  const rows = read.root.children
  const series = rows.map((n, i) => ({ key: `s${i}`, label: n.label }))
  const byDay = rows.map((n) => new Map(n.metrics.byDay.map((s) => [s.key, s])))
  const data = dayRange(from, to).map((day) => {
    const row: Record<string, string | number> = { day }
    byDay.forEach((days, i) => {
      const s = days.get(day)
      if (s) row[`s${i}`] = measured(s, measure)
    })
    return row
  })
  return { data, series }
}

/**
 * A split's colours, picked once over the whole tree so a key keeps its colour on every row:
 * `named` in legend order, and `other`, the colour of every key folded into one Other
 * segment (null when none folds).
 */
export interface Palette {
  named: Map<string, ChartToken>
  other: ChartToken | null
}

/** Other's colour in both splits; no named series takes it, so none reads as Other. */
const OTHER: ChartToken = 5

/**
 * `pinned` keys keep their token; the rest, by $ over `slices` (a tie by key), take `tokens`
 * in turn, and any past them fold into Other.
 */
function palette(
  slices: Slice[],
  tokens: readonly ChartToken[],
  pinned: Readonly<Record<string, ChartToken>> = {},
): Palette {
  const named = new Map<string, ChartToken>()
  for (const s of slices) if (s.key in pinned) named.set(s.key, pinned[s.key] as ChartToken)
  const rest = slices
    .filter((s) => !(s.key in pinned))
    .sort((a, b) => b.costUsd - a.costUsd || a.key.localeCompare(b.key))
  rest.slice(0, tokens.length).forEach((s, i) => named.set(s.key, tokens[i] as ChartToken))
  return { named, other: rest.length > tokens.length ? OTHER : null }
}

/** The agent types' colours: the orchestrator always --chart-6, the others 1..4. */
export const agentTypePalette = (slices: Slice[]) =>
  palette(slices, [1, 2, 3, 4], { orchestrator: 6 })

/** The models' colours, starting off the agent types' first ones. */
export const modelPalette = (slices: Slice[]) => palette(slices, [4, 3, 2, 1, 6])

export const agentTypeLabel = (key: string) => key || 'Unknown'

export const modelLabel = (key: string) => key || 'No requests'

/**
 * A row's slices as bar segments on `measure`, in legend order, then one Other for the keys
 * `palette` folds. A segment key is `slice:<key>`, or `other`, so none can clash.
 */
export function segments(
  slices: Slice[],
  palette: Palette,
  measure: Measure,
  label: (key: string) => string,
): InlineStackedBarSegment[] {
  const value = new Map(slices.map((s) => [s.key, measured(s, measure)]))
  const named = [...palette.named].flatMap(([key, color]) => {
    const v = value.get(key)
    return v === undefined ? [] : [{ key: `slice:${key}`, label: label(key), value: v, color }]
  })
  if (palette.other === null) return named
  const other = slices.filter((s) => !palette.named.has(s.key))
  const total = other.reduce((sum, s) => sum + measured(s, measure), 0)
  return [...named, { key: 'other', label: 'Other', value: total, color: palette.other }]
}
