import { describe, expect, it } from 'vitest'
import { agentRow, bottleneckDay, day } from '../tests/performance-fixtures'
import {
  bottleneckChart,
  byRepo,
  hours,
  rangeMean,
  share,
  successPct,
  totals,
  trendRows,
} from './performance'

const HOUR = 3_600_000

describe('rangeMean', () => {
  const days = [
    day({ issuesClosed: 3, usdPerClosedIssue: 10, prsMerged: 1, usdPerMergedPr: 4 }),
    day({ issuesClosed: 1, usdPerClosedIssue: 2 }),
    day({ spendUsd: 50 }),
  ]

  it('weights each day by its count, over the days that count something', () => {
    expect(
      rangeMean(
        days,
        (d) => d.usdPerClosedIssue,
        (d) => d.issuesClosed,
      ),
    ).toBe(8)
    expect(
      rangeMean(
        days,
        (d) => d.usdPerMergedPr,
        (d) => d.prsMerged,
      ),
    ).toBe(4)
  })

  it('is null when no day counts anything', () => {
    expect(
      rangeMean(
        [day()],
        (d) => d.usdPerClosedIssue,
        (d) => d.issuesClosed,
      ),
    ).toBeNull()
    expect(
      rangeMean(
        [],
        (d) => d.usdPerClosedIssue,
        (d) => d.issuesClosed,
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
  it('has a row per day: $ per closed issue and merged PR, gaps kept, and the spend', () => {
    const rows = trendRows([
      day({ day: '2026-10-04', spendUsd: 40 }),
      day({ day: '2026-10-05', usdPerClosedIssue: 8.6, usdPerMergedPr: 5.5, spendUsd: 220 }),
    ])
    expect(rows).toEqual([
      { day: 'Oct 4', perIssue: null, perPr: null, spend: 40 },
      { day: 'Oct 5', perIssue: 8.6, perPr: 5.5, spend: 220 },
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
    permissionMs: HOUR / 2,
    permissionPrompts: 3,
  }),
  bottleneckDay({
    day: '2026-10-13',
    repo: 'acme/api',
    promptWaitMs: HOUR / 4,
    activeMs: HOUR,
    permissionMs: 3 * HOUR,
    permissionPrompts: 1,
  }),
]

describe('totals', () => {
  it('sums every day and repo', () => {
    expect(totals(days)).toEqual({
      askMs: 2 * HOUR,
      promptWaitMs: 3.25 * HOUR,
      blockedMs: HOUR / 2,
      activeMs: 8 * HOUR,
      blockedReports: 3,
      permissionMs: 3.5 * HOUR,
      permissionPrompts: 4,
    })
  })
})

describe('bottleneckChart', () => {
  it('stacks questions, prompt wait, blocked and permission prompts in hours, a row per day with time', () => {
    const { data, series } = bottleneckChart(days, '2026-10-01')
    expect(series.map((s) => s.label)).toEqual([
      'Answering questions',
      'Waiting for prompt',
      'Blocked (proxy)',
      'Permission prompts',
    ])
    expect(data).toEqual([
      { day: '2026-10-12', ask: 1, promptWait: 3, blocked: 0, permission: 0 },
      { day: '2026-10-13', ask: 1, promptWait: 0.25, blocked: 0.5, permission: 3.5 },
    ])
  })

  it('leaves permission prompts out until a prompt is recorded', () => {
    const { data, series } = bottleneckChart(days, null)
    expect(series.map((s) => s.key)).not.toContain('permission')
    expect(data.every((d) => !('permission' in d))).toBe(true)
  })

  it('has no permission value on days before the first recorded prompt, and says since when', () => {
    const { data, series } = bottleneckChart(days, '2026-10-13')
    expect(series.at(-1)).toEqual({ key: 'permission', label: 'Permission prompts (since Oct 13)' })
    expect(data.map((d) => d.permission)).toEqual([null, 3.5])
  })
})

describe('byRepo', () => {
  it('sums each repo over the days, most waiting (permission prompts too) first and Unlinked last', () => {
    expect(byRepo(days)).toEqual([
      {
        repo: 'acme/api',
        askMs: 0,
        promptWaitMs: HOUR / 4,
        blockedMs: 0,
        activeMs: HOUR,
        blockedReports: 0,
        permissionMs: 3 * HOUR,
        permissionPrompts: 1,
      },
      {
        repo: 'acme/web',
        askMs: 2 * HOUR,
        promptWaitMs: 0,
        blockedMs: HOUR / 2,
        activeMs: 6 * HOUR,
        blockedReports: 2,
        permissionMs: HOUR / 2,
        permissionPrompts: 3,
      },
      {
        repo: null,
        askMs: 0,
        promptWaitMs: 3 * HOUR,
        blockedMs: 0,
        activeMs: HOUR,
        blockedReports: 1,
        permissionMs: 0,
        permissionPrompts: 0,
      },
    ])
  })
})
