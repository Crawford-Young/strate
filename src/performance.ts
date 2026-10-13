// Pure helpers for the Audit performance view: week labels, deltas, and the bottleneck's sums.
import type { StackedBarDatum, StackedBarSeries, TrendChartDatum } from '@crawfordyoung/ui'
import type { AgentRow, BottleneckDay, OutcomeWeek } from './api'
import { shortDay } from './audit'

const HOUR = 3_600_000

/** An ISO week by its Monday, `w/c Oct 12`, with its days in range when fewer than 7. */
export function weekLabel({ week, days }: OutcomeWeek): string {
  const label = `w/c ${shortDay(week)}`
  return days < 7 ? `${label} (${days} ${days === 1 ? 'day' : 'days'})` : label
}

/** The signed fraction from `before` to `now`; undefined when either is missing or `before` is 0. */
export function change(now: number | null, before: number | null | undefined): number | undefined {
  if (now === null || before === null || before === undefined || before === 0) return undefined
  return (now - before) / before
}

/**
 * A per-week mean over the whole range, each week weighted by its `count` (issues closed, PRs
 * merged): the weeks with a value and a count over 0 only. Null when no week has one.
 */
export function rangeMean(
  weeks: OutcomeWeek[],
  value: (w: OutcomeWeek) => number | null,
  count: (w: OutcomeWeek) => number,
): { mean: number; count: number } | null {
  let sum = 0
  let n = 0
  for (const w of weeks) {
    const v = value(w)
    if (v === null || count(w) === 0) continue
    sum += v * count(w)
    n += count(w)
  }
  return n === 0 ? null : { mean: sum / n, count: n }
}

/** Ms as hours: `1.5 h`. */
export const hours = (ms: number, digits = 1) => `${(ms / HOUR).toFixed(digits)} h`

/** `part` as a whole percent of `whole`; null when `whole` is 0. */
export const share = (part: number, whole: number) =>
  whole === 0 ? null : Math.round((part / whole) * 100)

/** The Outcome trend: a row per week, null where nothing closed or merged (a gap, not 0). */
export function trendRows(weeks: OutcomeWeek[]): TrendChartDatum[] {
  return weeks.map((w) => ({
    week: weekLabel(w),
    perIssue: w.usdPerClosedIssue,
    perPr: w.usdPerMergedPr,
    spend: w.spendUsd,
  }))
}

/** Completed, as a whole percent of the dispatches with an outcome; running ones have none yet. */
export const successPct = (row: AgentRow) => share(row.completed, row.dispatches - row.running)

export type BottleneckSums = Omit<BottleneckDay, 'day' | 'repo'>

const ZERO: BottleneckSums = {
  askMs: 0,
  promptWaitMs: 0,
  blockedMs: 0,
  activeMs: 0,
  blockedReports: 0,
}

const add = (a: BottleneckSums, b: BottleneckSums): BottleneckSums => ({
  askMs: a.askMs + b.askMs,
  promptWaitMs: a.promptWaitMs + b.promptWaitMs,
  blockedMs: a.blockedMs + b.blockedMs,
  activeMs: a.activeMs + b.activeMs,
  blockedReports: a.blockedReports + b.blockedReports,
})

/** Every day and repo summed. */
export const totals = (days: BottleneckDay[]): BottleneckSums => days.reduce(add, ZERO)

/** `days` summed per `by` value, in first-seen order. */
function sumBy<K>(days: BottleneckDay[], by: (d: BottleneckDay) => K): Map<K, BottleneckSums> {
  const sums = new Map<K, BottleneckSums>()
  for (const d of days) sums.set(by(d), add(sums.get(by(d)) ?? ZERO, d))
  return sums
}

const SERIES: StackedBarSeries[] = [
  { key: 'ask', label: 'Answering questions' },
  { key: 'promptWait', label: 'Waiting for prompt' },
  { key: 'blocked', label: 'Blocked (proxy)' },
]

/** Your time per day over every repo, in hours, stacked; only the days with a row. */
export function bottleneckChart(days: BottleneckDay[]): {
  data: StackedBarDatum[]
  series: StackedBarSeries[]
} {
  const data = [...sumBy(days, (d) => d.day)].map(([day, s]) => ({
    day,
    ask: s.askMs / HOUR,
    promptWait: s.promptWaitMs / HOUR,
    blocked: s.blockedMs / HOUR,
  }))
  return { data, series: SERIES }
}

/** One repo's time over the range; `repo` null is Unlinked. */
export type RepoSums = BottleneckSums & { repo: string | null }

const waited = (s: BottleneckSums) => s.askMs + s.promptWaitMs + s.blockedMs

/** Each repo summed over the days, the most time waiting first, Unlinked last. */
export function byRepo(days: BottleneckDay[]): RepoSums[] {
  const rows = [...sumBy(days, (d) => d.repo)].map(([repo, s]) => ({ repo, ...s }))
  const last = (r: RepoSums) => (r.repo === null ? 1 : 0)
  return rows.sort((a, b) => last(a) - last(b) || waited(b) - waited(a))
}
