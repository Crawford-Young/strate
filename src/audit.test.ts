import { describe, expect, it } from 'vitest'
import { audit, node } from '../tests/audit-fixtures'
import {
  compact,
  dailyChart,
  dayRange,
  hasPath,
  hoursLabel,
  lastDays,
  localDay,
  repoOptions,
  shortDay,
} from './audit'

const HOUR = 3_600_000

describe('local days', () => {
  it('formats a date as its local YYYY-MM-DD', () => {
    expect(localDay(new Date(2026, 9, 5, 23, 59))).toBe('2026-10-05')
    expect(localDay(new Date(2026, 0, 1, 0, 0))).toBe('2026-01-01')
  })

  it('spans the last n local days, today included', () => {
    expect(lastDays(7, new Date(2026, 9, 13, 10))).toEqual({ from: '2026-10-07', to: '2026-10-13' })
    expect(lastDays(14, new Date(2026, 2, 3, 1))).toEqual({ from: '2026-02-18', to: '2026-03-03' })
    expect(lastDays(1, new Date(2026, 9, 13))).toEqual({ from: '2026-10-13', to: '2026-10-13' })
  })

  it('labels a day by its local date', () => {
    const want = new Intl.DateTimeFormat(undefined, { month: 'short', day: 'numeric' }).format(
      new Date(2026, 9, 5),
    )
    expect(shortDay('2026-10-05')).toBe(want)
  })

  it('lists every day from..=to, across a month end', () => {
    expect(dayRange('2026-09-29', '2026-10-02')).toEqual([
      '2026-09-29',
      '2026-09-30',
      '2026-10-01',
      '2026-10-02',
    ])
    expect(dayRange('2026-10-02', '2026-10-02')).toEqual(['2026-10-02'])
  })
})

describe('node paths', () => {
  const tree = audit([
    node('repo', 'acme/web', {}, [node('issue', '11')]),
    node('repo', 'acme/api'),
  ])

  it('finds a path step by step from the root', () => {
    expect(hasPath(tree.root, [['repo', 'acme/web']])).toBe(true)
    expect(
      hasPath(tree.root, [
        ['repo', 'acme/web'],
        ['issue', '11'],
      ]),
    ).toBe(true)
  })

  it('misses a path when any step is gone, or matches on kind or key alone', () => {
    expect(
      hasPath(tree.root, [
        ['repo', 'acme/api'],
        ['issue', '11'],
      ]),
    ).toBe(false)
    expect(hasPath(tree.root, [['epic', 'acme/web']])).toBe(false)
    expect(hasPath(tree.root, [['repo', 'acme/x']])).toBe(false)
  })
})

describe('numbers', () => {
  it('shortens counts to k and M', () => {
    expect(compact(0)).toBe('0')
    expect(compact(950)).toBe('950')
    expect(compact(1_234)).toBe('1.2k')
    expect(compact(3_400_000)).toBe('3.4M')
  })

  it('shows hours to one decimal', () => {
    expect(hoursLabel(0)).toBe('0h')
    expect(hoursLabel(1.25)).toBe('1.3h')
    expect(hoursLabel(12)).toBe('12h')
  })
})

describe('the daily chart', () => {
  // A by-day rollup: each day's Group holds its repos, then Unlinked.
  const byDay = audit([
    node('group', '2026-10-05', {}, [
      node('repo', 'acme/web', { costUsd: 2, agentMs: 2 * HOUR, activeMs: 9 * HOUR }),
      node('unlinked', 'unlinked', { costUsd: 0.5, agentMs: HOUR / 2, activeMs: 9 * HOUR }, [], {
        label: 'Unlinked',
      }),
    ]),
    node('group', '2026-10-07', {}, [
      node('repo', 'acme/api', { costUsd: 1, agentMs: 3 * HOUR, activeMs: 9 * HOUR }),
      node('repo', 'acme/web', { costUsd: 4, agentMs: HOUR, activeMs: 9 * HOUR }),
    ]),
  ])

  it('stacks one series per repo, Unlinked last, with a row for every day in range', () => {
    const chart = dailyChart(byDay, 'usd', null, '2026-10-05', '2026-10-07')
    expect(chart.series.map((s) => s.label)).toEqual(['acme/api', 'acme/web', 'Unlinked'])
    const [api, web, unlinked] = chart.series.map((s) => s.key)
    expect(chart.data).toEqual([
      { day: '2026-10-05', [web as string]: 2, [unlinked as string]: 0.5 },
      { day: '2026-10-06' },
      { day: '2026-10-07', [api as string]: 1, [web as string]: 4 },
    ])
  })

  it('uses series keys a chart reads as plain fields, whatever the slug', () => {
    const chart = dailyChart(byDay, 'usd', null, '2026-10-05', '2026-10-05')
    for (const s of chart.series) expect(s.key).toMatch(/^[a-z0-9]+$/)
  })

  it('charts agent-hours (wall) on the hours measure, not active time', () => {
    const chart = dailyChart(byDay, 'hours', null, '2026-10-07', '2026-10-07')
    const [api, web] = chart.series.map((s) => s.key)
    expect(chart.data).toEqual([{ day: '2026-10-07', [api as string]: 3, [web as string]: 1 }])
  })

  it('keeps only the picked repo', () => {
    const chart = dailyChart(byDay, 'usd', 'acme/web', '2026-10-05', '2026-10-07')
    expect(chart.series.map((s) => s.label)).toEqual(['acme/web'])
    const [web] = chart.series.map((s) => s.key)
    expect(chart.data.map((d) => d[web as string])).toEqual([2, undefined, 4])
  })

  it('lists the repos with work, sorted, without Unlinked', () => {
    expect(repoOptions(byDay)).toEqual(['acme/api', 'acme/web'])
  })
})
