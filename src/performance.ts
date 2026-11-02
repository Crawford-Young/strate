// Pure helpers for the Audit performance view: range means, and the bottleneck's sums.
import type { StackedBarDatum, StackedBarSeries, TrendChartDatum } from '@crawfordyoung/ui'
import type { AgentRow, BottleneckDay, OutcomeDay } from './api'
import { shortDay } from './audit'

const HOUR = 3_600_000

/**
 * The mean over the whole range of a per-day value, each day weighted by its `count` (issues
 * closed, PRs merged): the days with a value and a count over 0 only. Null when no day has one.
 */
export function rangeMean(
  days: OutcomeDay[],
  value: (d: OutcomeDay) => number | null,
  count: (d: OutcomeDay) => number,
): number | null {
  let sum = 0
  let n = 0
  for (const d of days) {
    const v = value(d)
    if (v === null || count(d) === 0) continue
    sum += v * count(d)
    n += count(d)
  }
  return n === 0 ? null : sum / n
}

/** Ms as hours: `1.5 h`. */
export const hours = (ms: number, digits = 1) => `${(ms / HOUR).toFixed(digits)} h`

/** `part` as a whole percent of `whole`; null when `whole` is 0. */
export const share = (part: number, whole: number) =>
  whole === 0 ? null : Math.round((part / whole) * 100)

/** The Outcome trend: a row per day, null where nothing closed or merged (a gap, not 0). */
export function trendRows(days: OutcomeDay[]): TrendChartDatum[] {
  return days.map((d) => ({
    day: shortDay(d.day),
    perIssue: d.usdPerClosedIssue,
    perPr: d.usdPerMergedPr,
    spend: d.spendUsd,
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
  permissionMs: 0,
  permissionPrompts: 0,
}

const add = (a: BottleneckSums, b: BottleneckSums): BottleneckSums => ({
  askMs: a.askMs + b.askMs,
  promptWaitMs: a.promptWaitMs + b.promptWaitMs,
  blockedMs: a.blockedMs + b.blockedMs,
  activeMs: a.activeMs + b.activeMs,
  blockedReports: a.blockedReports + b.blockedReports,
  permissionMs: a.permissionMs + b.permissionMs,
  permissionPrompts: a.permissionPrompts + b.permissionPrompts,
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

/**
 * Your time per day over every repo, in hours, stacked; only the days with a row. Permission
 * prompts join once `recordedSince` (the first recorded prompt's day) is set, with no value on
 * the days before it, and the label names that day when the chart has such days.
 */
export function bottleneckChart(
  days: BottleneckDay[],
  recordedSince: string | null,
): { data: StackedBarDatum[]; series: StackedBarSeries[] } {
  const sums = [...sumBy(days, (d) => d.day)]
  const data = sums.map(([day, s]) => ({
    day,
    ask: s.askMs / HOUR,
    promptWait: s.promptWaitMs / HOUR,
    blocked: s.blockedMs / HOUR,
    ...(recordedSince !== null && {
      permission: day < recordedSince ? null : s.permissionMs / HOUR,
    }),
  }))
  if (recordedSince === null) return { data, series: SERIES }
  const label = sums.some(([day]) => day < recordedSince)
    ? `Permission prompts (since ${shortDay(recordedSince)})`
    : 'Permission prompts'
  return { data, series: [...SERIES, { key: 'permission', label }] }
}

/** One repo's time over the range; `repo` null is Unlinked. */
export type RepoSums = BottleneckSums & { repo: string | null }

/** Your time in the loop: questions, prompt wait, blocked and permission prompts. */
export const waited = (s: BottleneckSums) => s.askMs + s.promptWaitMs + s.blockedMs + s.permissionMs

/** Each repo summed over the days, the most time waiting first, Unlinked last. */
export function byRepo(days: BottleneckDay[]): RepoSums[] {
  const rows = [...sumBy(days, (d) => d.repo)].map(([repo, s]) => ({ repo, ...s }))
  const last = (r: RepoSums) => (r.repo === null ? 1 : 0)
  return rows.sort((a, b) => last(a) - last(b) || waited(b) - waited(a))
}
