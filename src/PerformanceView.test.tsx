import { fireEvent, render, screen, within } from '@testing-library/react'
import { describe, expect, it } from 'vitest'
import { headline } from '../tests/audit-fixtures'
import {
  agentRow,
  bottleneckDay,
  outcome,
  performance,
  waste,
  day,
} from '../tests/performance-fixtures'
import type { Headline, Performance } from './api'
import { PerformanceView } from './PerformanceView'

const HOUR = 3_600_000

const range: Headline = headline({
  issuesClosed: 28,
  medianElapsedMs: 28 * HOUR,
  usdPerClosedIssue: 8.1,
  usdPerMergedPr: 6.2,
})

const twoDays = outcome({
  days: [
    day({
      day: '2026-10-11',
      issuesClosed: 17,
      usdPerClosedIssue: 8.63,
      activeMsPerClosedIssue: 0.63 * HOUR,
      prsMerged: 16,
      usdPerMergedPr: 5.58,
      spendUsd: 223.8,
    }),
    day({
      day: '2026-10-12',
      issuesClosed: 11,
      usdPerClosedIssue: 7.58,
      activeMsPerClosedIssue: 0.85 * HOUR,
      prsMerged: 11,
      usdPerMergedPr: 7.58,
      spendUsd: 218.1,
    }),
  ],
  unlinkedUsd: 220,
  unlinkedWorkstreams: 10,
  openUsd: 134,
  openIssues: 9,
})

function show(perf: Partial<Parameters<typeof performance>[0]> = {}, h: Headline = range) {
  const p: Performance = performance({ outcome: twoDays, ...perf })
  return render(<PerformanceView headline={h} performance={p} />)
}

const region = (name: string) => screen.getByRole('region', { name })
const squash = (s: string | null) => (s ?? '').replace(/\s+/g, ' ').trim()

/** A tile's value, then its delta and hint (whichever it has), as text. */
function tile(label: string, within_: HTMLElement = document.body) {
  const dl = within(within_).getByText(label, { selector: 'dt' }).closest('dl') as HTMLElement
  return [...dl.querySelectorAll('dd')].map((d) => squash(d.textContent))
}
const tileValue = (label: string) =>
  (screen.getByText(label, { selector: 'dt' }).closest('dl') as HTMLElement).querySelector('dd')

/** A table's body rows as cell text, under the region `name`. */
const rows = (name: string) =>
  within(region(name))
    .getAllByRole('row')
    .map((r) =>
      within(r)
        .queryAllByRole('cell')
        .map((c) => squash(c.textContent)),
    )
    .filter((cells) => cells.length > 0)

/** Clicks the sort button of the column `header` in the region `name`. */
const sortBy = (name: string, header: string) =>
  fireEvent.click(
    within(within(region(name)).getByRole('columnheader', { name: header })).getByRole('button'),
  )

const headers = (name: string) =>
  within(region(name))
    .getAllByRole('columnheader')
    .map((h) => squash(h.textContent))

describe('outcome', () => {
  it('has the section heading and its question', () => {
    show()
    expect(within(region('Outcome')).getByRole('heading', { name: 'Outcome' })).toBeVisible()
    expect(region('Outcome')).toHaveTextContent('Is the harness getting better?')
  })

  it('shows each tile over the whole range, with no delta', () => {
    show()
    expect(tile('$ per closed issue')).toEqual(['$8.22'])
    expect(tile('Hours per closed issue')).toEqual(['0.72 h', 'active orchestrator time'])
    expect(tile('$ per merged PR')).toEqual(['$6.39'])
    expect(tile('Issues closed')).toEqual(['28', 'completed only'])
    expect(region('Outcome')).not.toHaveTextContent(/vs|Tiles:/)
  })

  it('shows the spend with no outcome, and the median elapsed over the range', () => {
    show()
    expect(tile('No linked outcome')).toEqual([
      '$220.00',
      '10 workstreams',
      'no <repo>-<issue> name',
    ])
    expect(tile('Spent on open issues')).toEqual(['$134.00', '9 issues', 'not yet an outcome'])
    expect(tile('Median elapsed')).toEqual(['1d 4h', 'range: first window to close'])
  })

  it('shows dashes when nothing closed or merged', () => {
    show({ outcome: outcome() }, headline())
    expect(tile('$ per closed issue')).toEqual(['$—'])
    expect(tile('Hours per closed issue')).toEqual(['—', 'active orchestrator time'])
    expect(tile('$ per merged PR')).toEqual(['$—'])
    expect(tile('Issues closed')).toEqual(['0', 'completed only'])
    expect(tile('Median elapsed')[0]).toBe('—')
  })

  it('notes that cloud spend is not counted, only when it applies', () => {
    const { unmount } = show()
    expect(screen.queryByText(/Cloud sessions/)).not.toBeInTheDocument()
    unmount()
    show({}, { ...range, cloudUncounted: true })
    expect(screen.getByText(/Cloud sessions are not counted/)).toBeInTheDocument()
  })

  it('charts $ per closed issue and merged PR by day over the day’s spend, gaps as dashes', () => {
    show({
      outcome: outcome({ days: [day({ day: '2026-10-10', spendUsd: 40 }), ...twoDays.days] }),
    })
    expect(headers('Per outcome, by day')).toEqual([
      'Day',
      '$ per closed issue',
      '$ per merged PR',
      'total spend that day',
    ])
    expect(rows('Per outcome, by day')).toEqual([
      ['Oct 10', '—', '—', '$40.00'],
      ['Oct 11', '$8.63', '$5.58', '$223.80'],
      ['Oct 12', '$7.58', '$7.58', '$218.10'],
    ])
  })
})

describe('agents & models', () => {
  const agents = [
    agentRow({
      agentType: 'orchestrator',
      model: 'opus-5-5',
      orchestrator: true,
      costUsd: 289.16,
      tokens: { input: 500_000, output: 120_000, cacheRead: 8_000_000, cacheWrite: 1_500_000 },
      cacheHitPct: 98.3,
    }),
    agentRow({
      agentType: 'implementer',
      model: 'opus-5-5',
      dispatches: 41,
      completed: 35,
      blocked: 3,
      noReport: 2,
      running: 1,
      followups: 28,
      costUsd: 93.31,
      usdPerDispatch: 2.28,
      tokens: { input: 1_000_000, output: 801_400, cacheRead: 2_000_000, cacheWrite: 100_000 },
      cacheHitPct: 96.84,
    }),
    agentRow({ agentType: '', model: '', dispatches: 2, running: 2 }),
  ]

  it('has a column per measure', () => {
    show({ agents })
    expect(headers('Agents & models')).toEqual([
      'Agent',
      'Model',
      'Dispatches',
      'Success',
      'Blocked',
      'No report',
      'Follow-ups',
      '$ total',
      '$ / dispatch',
      'Input Mtok',
      'Output ktok',
      'Cache hit',
    ])
    expect(region('Agents & models')).toHaveTextContent('Sorted by $.')
  })

  it('shows each lane, success over the dispatches with an outcome', () => {
    show({ agents })
    expect(rows('Agents & models')[1]).toEqual([
      'implementer',
      'opus-5-5',
      '41',
      '88%',
      '3',
      '2',
      '28',
      '$93.31',
      '$2.28',
      '3.1',
      '801',
      '96.8%',
    ])
    const meters = within(region('Agents & models')).getAllByRole('progressbar', {
      name: 'Success',
    })
    expect(meters[0]).toHaveAttribute('aria-valuenow', '88')
  })

  it('flags non-zero blocked and no-report counts', () => {
    show({ agents })
    const row = within(region('Agents & models')).getAllByRole('row')[2] as HTMLElement
    const cells = within(row).getAllByRole('cell') as [HTMLElement, ...HTMLElement[]]
    const [, , , , blocked, noReport, followups] = cells
    expect(within(blocked as HTMLElement).getByText('3')).toHaveClass('bg-warning')
    expect(within(noReport as HTMLElement).getByText('2')).toHaveClass('bg-warning')
    expect(within(followups as HTMLElement).getByText('28')).not.toHaveClass('bg-warning')
  })

  it('mutes orchestrator rows, with dashes where dispatches would go', () => {
    show({ agents })
    expect(rows('Agents & models')[0]).toEqual([
      'orchestrator',
      'opus-5-5',
      '—',
      '—',
      '—',
      '—',
      '—',
      '$289.16',
      '—',
      '10.0',
      '120',
      '98.3%',
    ])
    const row = within(region('Agents & models')).getAllByRole('row')[1] as HTMLElement
    expect(within(row).getByText('$289.16')).toHaveClass('text-muted-foreground')
  })

  it('names an unknown lane, and dashes a success or cache hit with no data', () => {
    show({ agents })
    expect(rows('Agents & models')[2]).toEqual([
      'unknown',
      'unknown',
      '2',
      '—',
      '0',
      '0',
      '0',
      '$0.00',
      '$—',
      '0.0',
      '0',
      '—',
    ])
  })

  it('keeps the order it is given: orchestrators, then by $', () => {
    show({ agents })
    expect(rows('Agents & models').map((r) => r[0])).toEqual([
      'orchestrator',
      'implementer',
      'unknown',
    ])
  })

  it('sorts by a column, most first, then least; a missing value last either way', () => {
    show({ agents })
    const lanes = () => rows('Agents & models').map((r) => r[0])
    sortBy('Agents & models', '$ total')
    expect(lanes()).toEqual(['orchestrator', 'implementer', 'unknown'])
    sortBy('Agents & models', '$ total')
    expect(lanes()).toEqual(['unknown', 'implementer', 'orchestrator'])
    for (let i = 0; i < 2; i++) {
      sortBy('Agents & models', 'Success')
      expect(lanes()).toEqual(['implementer', 'orchestrator', 'unknown'])
    }
  })

  it('sorts the orchestrator after the lanes by every dispatch column, either way', () => {
    show({ agents })
    const dispatch = ['Dispatches', 'Success', 'Blocked', 'No report', 'Follow-ups', '$ / dispatch']
    for (const header of headers('Agents & models'))
      for (let i = 0; i < 2; i++) {
        sortBy('Agents & models', header)
        const lanes = rows('Agents & models').map((r) => r[0])
        expect(lanes).toHaveLength(3)
        if (dispatch.includes(header))
          expect(lanes.indexOf('orchestrator')).toBeGreaterThan(lanes.indexOf('implementer'))
      }
  })
})

describe('bottleneck', () => {
  const days = [
    bottleneckDay({
      day: '2026-10-12',
      repo: 'acme/web',
      askMs: 2 * HOUR,
      activeMs: 6 * HOUR,
      permissionMs: HOUR / 2,
      permissionPrompts: 2,
    }),
    bottleneckDay({
      day: '2026-10-12',
      repo: null,
      promptWaitMs: HOUR,
      activeMs: 2 * HOUR,
      blockedReports: 1,
    }),
    bottleneckDay({
      day: '2026-10-13',
      repo: 'acme/web',
      blockedMs: HOUR / 2,
      activeMs: 2 * HOUR,
      blockedReports: 2,
      permissionPrompts: 1,
    }),
  ]
  const recorded = { recordedSince: '2026-10-05', open: 1 }

  it('totals your time in the loop as a share of active time', () => {
    show({ days })
    const b = region('Bottleneck')
    expect(tile('Answering questions', b)).toEqual([
      '2.0 h',
      '20% of 10.0 h active',
      'AskUserQuestion, ExitPlanMode',
    ])
    expect(tile('Waiting for your prompt', b)).toEqual([
      '1.0 h',
      '10% of active',
      'last reply → your next message',
    ])
    expect(tile('Blocked (proxy)', b)).toEqual([
      '0.5 h',
      '3 blocked reports',
      'NEEDS_CONTEXT → next prompt or dispatch',
    ])
  })

  it('says permission prompts are not recorded until a hook has recorded one', () => {
    show({ days })
    expect(tile('Permission prompts', region('Bottleneck'))).toEqual([
      'Not recorded',
      'no PermissionRequest hook received yet',
    ])
  })

  it('totals the permission waits, counting the open prompts apart, since the first recorded', () => {
    show({ days, permissionWaits: recorded })
    expect(tile('Permission prompts', region('Bottleneck'))).toEqual([
      '0.5 h',
      '3 prompts, 1 still open',
      'recorded since Oct 5',
    ])
  })

  it('names a single prompt and leaves out the open count when none is open', () => {
    show({
      days: [bottleneckDay({ permissionMs: HOUR, permissionPrompts: 1 })],
      permissionWaits: { recordedSince: '2026-10-12', open: 0 },
    })
    expect(tile('Permission prompts', region('Bottleneck'))).toEqual([
      '1.0 h',
      '1 prompt',
      'recorded since Oct 12',
    ])
  })

  it('dashes the shares when there is no active time', () => {
    show()
    expect(tile('Answering questions', region('Bottleneck'))[1]).toBe('— of 0.0 h active')
  })

  it('stacks questions, prompt wait, blocked and permission prompts per day, in hours', () => {
    show({ days, permissionWaits: recorded })
    expect(headers('Per day, hours')).toEqual([
      'Day',
      'Answering questions',
      'Waiting for prompt',
      'Blocked (proxy)',
      'Permission prompts',
    ])
    expect(rows('Per day, hours')).toEqual([
      ['Oct 12', '2h', '1h', '0h', '0.5h'],
      ['Oct 13', '0h', '0h', '0.5h', '0h'],
    ])
  })

  it('splits it per repo, Unlinked last, with the wait share of active time', () => {
    show({ days, permissionWaits: recorded })
    expect(headers('Per repo')).toEqual([
      'Repo',
      'Questions',
      'Prompt wait',
      'Blocked',
      'Permission',
      'Active',
      'Wait share',
    ])
    expect(rows('Per repo')).toEqual([
      ['acme/web', '2.0 h', '0.0 h', '0.5 h', '0.5 h', '8.0 h', '38%'],
      ['Unlinked', '0.0 h', '1.0 h', '0.0 h', '0.0 h', '2.0 h', '50%'],
    ])
  })

  it('dashes the wait share of a repo with no active time', () => {
    show({ days: [bottleneckDay({ repo: 'acme/api', askMs: HOUR })], permissionWaits: recorded })
    expect(rows('Per repo')).toEqual([
      ['acme/api', '1.0 h', '0.0 h', '0.0 h', '0.0 h', '0.0 h', '—'],
    ])
  })

  it('dashes permission per repo and charts no permission series until a prompt is recorded', () => {
    show({ days })
    expect(rows('Per repo').map((r) => r[4])).toEqual(['—', '—'])
    expect(headers('Per day, hours')).not.toContain('Permission prompts')
  })

  it('notes that the wait share can pass 100%', () => {
    show({ days })
    expect(region('Per repo')).toHaveTextContent(
      "Wait share can pass 100%: blocked and subagent permission time fall outside the orchestrator's active time.",
    )
  })

  it('keeps Unlinked last under any sort', () => {
    show({ days: [...days, bottleneckDay({ repo: 'acme/api', askMs: 4 * HOUR })] })
    for (const header of headers('Per repo'))
      for (let i = 0; i < 2; i++) {
        sortBy('Per repo', header)
        expect(rows('Per repo').map((r) => r[0])).toEqual([
          expect.stringMatching(/^acme/),
          expect.stringMatching(/^acme/),
          'Unlinked',
        ])
      }
  })
})

describe('waste & rework', () => {
  it('counts each kind of rework against its whole', () => {
    show({
      waste: waste({
        followups: 62,
        sendMessages: 102,
        blocked: 5,
        dispatches: 198,
        noReport: 3,
        noReportTop: { agentType: 'implementer', model: 'opus-5-5', count: 3 },
        compactions: 1,
        clears: 50,
        contextPeaksOver80: 2,
        deepestPeak: { pct: 91.6, model: 'opus-5-5' },
        apiRetries: 1,
        apiErrors: 4,
        promptTooLong: 1,
        reopenedIssues: 2,
        revertedPrs: 1,
        repos: 5,
      }),
    })
    const w = region('Waste & rework')
    expect(w).toHaveTextContent('Zeros are real counts, not missing data.')
    expect(tile('Follow-ups to agents', w)).toEqual([
      '62',
      'of 102 SendMessage',
      'a dispatch that needed more',
    ])
    expect(tile('Blocked reports', w)).toEqual(['5', 'of 198 dispatches', 'NEEDS_CONTEXT'])
    expect(tile('No report back', w)).toEqual([
      '3',
      'implementer · opus-5-5',
      'no task notification',
    ])
    expect(tile('Compactions', w)).toEqual(['1', '50 /clear rotations'])
    expect(tile('Context peaks ≥ 80%', w)).toEqual(['2', 'deepest 92% (opus-5-5)'])
    expect(tile('API retries · errors', w)).toEqual(['1 · 4', '1 prompt-too-long'])
    expect(tile('Reopened issues', w)).toEqual(['2', '5 repos'])
    expect(tile('Reverted PRs', w)).toEqual(['1', '5 repos'])
    expect(tileValue('Compactions')).not.toHaveClass('text-muted-foreground')
  })

  it('mutes a real zero, and names no lane or peak when there is none', () => {
    show({ waste: waste({ noReportTop: { agentType: '', model: '', count: 0 } }) })
    const w = region('Waste & rework')
    for (const label of [
      'Follow-ups to agents',
      'Blocked reports',
      'No report back',
      'Compactions',
      'Context peaks ≥ 80%',
      'API retries · errors',
      'Reopened issues',
      'Reverted PRs',
    ])
      expect(tileValue(label)).toHaveClass('text-muted-foreground')
    expect(tile('No report back', w)).toEqual(['0', 'unknown · unknown', 'no task notification'])
    expect(tile('Context peaks ≥ 80%', w)).toEqual(['0'])
  })

  it('has no lane under no report when nothing went unreported', () => {
    show()
    expect(tile('No report back', region('Waste & rework'))).toEqual(['0', 'no task notification'])
  })
})
