import { describe, expect, it } from 'vitest'
import { agentRow, bottleneckDay, week } from '../tests/performance-fixtures'
import {
  bottleneckChart,
  byRepo,
  change,
  hours,
  rangeMean,
  share,
  successPct,
  totals,
  trendRows,
  weekLabel,
} from './performance'

const HOUR = 3_600_000

describe('weekLabel', () => {
  it('names a week by its Monday, with its days in range when fewer than 7', () => {
    expect(weekLabel(week({ week: '2026-10-05' }))).toBe('w/c Oct 5')
    expect(weekLabel(week({ week: '2026-10-12', days: 2 }))).toBe('w/c Oct 12 (2 days)')
    expect(weekLabel(week({ week: '2026-10-12', days: 1 }))).toBe('w/c Oct 12 (1 day)')
  })
})

describe('change', () => {
  it('is the signed fraction from the earlier value', () => {
    expect(change(7.5, 10)).toBeCloseTo(-0.25)
    expect(change(12, 8)).toBeCloseTo(0.5)
  })

  it('is undefined when either side is missing or the earlier is 0', () => {
    expect(change(null, 10)).toBeUndefined()
    expect(change(3, null)).toBeUndefined()
    expect(change(3, undefined)).toBeUndefined()
    expect(change(3, 0)).toBeUndefined()
  })
})

describe('rangeMean', () => {
  const weeks = [
    week({ issuesClosed: 3, usdPerClosedIssue: 10, prsMerged: 1, usdPerMergedPr: 4 }),
    week({ issuesClosed: 1, usdPerClosedIssue: 2 }),
    week({ spendUsd: 50 }),
  ]

  it('weights each week by its count, over the weeks that count something', () => {
    expect(
      rangeMean(
        weeks,
        (w) => w.usdPerClosedIssue,
        (w) => w.issuesClosed,
      ),
    ).toEqual({
      mean: 8,
      count: 4,
    })
    expect(
      rangeMean(
        weeks,
        (w) => w.usdPerMergedPr,
        (w) => w.prsMerged,
      ),
    ).toEqual({
      mean: 4,
      count: 1,
    })
  })

  it('is null when no week counts anything', () => {
    expect(
      rangeMean(
        [week()],
        (w) => w.usdPerClosedIssue,
        (w) => w.issuesClosed,
      ),
    ).toBeNull()
    expect(
      rangeMean(
        [],
        (w) => w.usdPerClosedIssue,
        (w) => w.issuesClosed,
      ),
    ).toBeNull()
  })
})

describe('hours', () => {
  it('shows ms as hours, one decimal unless asked for more', () => {
    expect(hours(1.5 * HOUR)).toBe('1.5 h')
    expect(hours(0.85 * HOUR, 2)).toBe('0.85 h')
    expect(hours(0)).toBe('0.0 h')
  })
})

describe('share', () => {
  it('is a whole percent of the whole, null over nothing', () => {
    expect(share(1, 4)).toBe(25)
    expect(share(1, 3)).toBe(33)
    expect(share(1, 0)).toBeNull()
  })
})

describe('trendRows', () => {
  it('has a row per week: $ per closed issue and merged PR, gaps kept, and the spend', () => {
    const rows = trendRows([
      week({ week: '2026-09-28', spendUsd: 40 }),
      week({ week: '2026-10-05', usdPerClosedIssue: 8.6, usdPerMergedPr: 5.5, spendUsd: 220 }),
    ])
    expect(rows).toEqual([
      { week: 'w/c Sep 28', perIssue: null, perPr: null, spend: 40 },
      { week: 'w/c Oct 5', perIssue: 8.6, perPr: 5.5, spend: 220 },
    ])
  })
})

describe('successPct', () => {
  it('is completed over the dispatches with an outcome, running ones left out', () => {
    expect(successPct(agentRow({ dispatches: 10, completed: 6, running: 2 }))).toBe(75)
  })

  it('is null when no dispatch has an outcome yet', () => {
    expect(successPct(agentRow({ dispatches: 2, running: 2 }))).toBeNull()
    expect(successPct(agentRow())).toBeNull()
  })
})

const days = [
  bottleneckDay({ day: '2026-10-12', repo: 'acme/web', askMs: HOUR, activeMs: 4 * HOUR }),
  bottleneckDay({
    day: '2026-10-12',
    repo: null,
    promptWaitMs: 3 * HOUR,
    activeMs: HOUR,
    blockedReports: 1,
  }),
  bottleneckDay({
    day: '2026-10-13',
    repo: 'acme/web',
    askMs: HOUR,
    blockedMs: HOUR / 2,
    activeMs: 2 * HOUR,
    blockedReports: 2,
  }),
  bottleneckDay({ day: '2026-10-13', repo: 'acme/api', promptWaitMs: HOUR / 4, activeMs: HOUR }),
]

describe('totals', () => {
  it('sums every day and repo', () => {
    expect(totals(days)).toEqual({
      askMs: 2 * HOUR,
      promptWaitMs: 3.25 * HOUR,
      blockedMs: HOUR / 2,
      activeMs: 8 * HOUR,
      blockedReports: 3,
    })
  })
})

describe('bottleneckChart', () => {
  it('stacks questions, prompt wait and blocked in hours, a row per day with time', () => {
    const { data, series } = bottleneckChart(days)
    expect(series.map((s) => s.label)).toEqual([
      'Answering questions',
      'Waiting for prompt',
      'Blocked (proxy)',
    ])
    expect(data).toEqual([
      { day: '2026-10-12', ask: 1, promptWait: 3, blocked: 0 },
      { day: '2026-10-13', ask: 1, promptWait: 0.25, blocked: 0.5 },
    ])
  })
})

describe('byRepo', () => {
  it('sums each repo over the days, most waiting first and Unlinked last', () => {
    expect(byRepo(days)).toEqual([
      {
        repo: 'acme/web',
        askMs: 2 * HOUR,
        promptWaitMs: 0,
        blockedMs: HOUR / 2,
        activeMs: 6 * HOUR,
        blockedReports: 2,
      },
      {
        repo: 'acme/api',
        askMs: 0,
        promptWaitMs: HOUR / 4,
        blockedMs: 0,
        activeMs: HOUR,
        blockedReports: 0,
      },
      {
        repo: null,
        askMs: 0,
        promptWaitMs: 3 * HOUR,
        blockedMs: 0,
        activeMs: HOUR,
        blockedReports: 1,
      },
    ])
  })
})
