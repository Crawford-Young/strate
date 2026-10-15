import { act, fireEvent, render, screen, within } from '@testing-library/react'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { audit, headline, node } from '../tests/audit-fixtures'
import { agentRow, performance } from '../tests/performance-fixtures'
import {
  auditPerformance,
  auditRefresh,
  auditRollup,
  auditTimeline,
  onStoreChanged,
  writeExport,
  type Audit,
  type AuditFilter,
  type TimelineDay,
} from './api'
import { compact, dailyChart } from './audit'
import { AuditView } from './AuditView'
import { totals } from './performance'

vi.mock('./api', () => ({
  auditPerformance: vi.fn(),
  auditRollup: vi.fn(),
  auditRefresh: vi.fn(),
  auditTimeline: vi.fn(),
  writeExport: vi.fn(),
  onStoreChanged: vi.fn(),
}))
// Spied, to see what a rerender recomputes.
vi.mock('./audit', async (importOriginal) => {
  const real = await importOriginal<typeof import('./audit')>()
  return { ...real, dailyChart: vi.fn(real.dailyChart), compact: vi.fn(real.compact) }
})
vi.mock('./performance', async (importOriginal) => {
  const real = await importOriginal<typeof import('./performance')>()
  return { ...real, totals: vi.fn(real.totals) }
})
// happy-dom has no layout, so the panels are the stand-in group; the panel also shows its
// default size, which the stand-in ignores.
vi.mock('@crawfordyoung/ui', async (importOriginal) => ({
  ...(await importOriginal<typeof import('@crawfordyoung/ui')>()),
  ...(await import('../tests/resizable-mock')),
  ResizablePanel: ({ id, defaultSize, children }: Record<string, never>) => (
    <section data-testid={`panel-${id}`} data-default-size={defaultSize}>
      {children}
    </section>
  ),
}))

const rollupMock = vi.mocked(auditRollup)
const performanceMock = vi.mocked(auditPerformance)
const refreshMock = vi.mocked(auditRefresh)
const timelineMock = vi.mocked(auditTimeline)
const writeMock = vi.mocked(writeExport)
// Every store-changed listener; the event reaches them all. Audit should add none.
let listeners: (() => void)[] = []
const storeChanged = () => act(() => listeners.forEach((listener) => listener()))
/** Lets settled reads land. */
const flush = () => act(() => new Promise<void>((r) => setTimeout(r, 0)))

const HOUR = 3_600_000
const DAY = 24 * HOUR

// Hierarchy: acme/web (an epic with a GitHub issue, plus an issue known only from a window
// name), acme/api, then Unlinked, which costs the most so a sort would move it if it could.
const tree: Audit = audit(
  [
    node('repo', 'acme/web', { costUsd: 6.5, unpriced: 2, agentMs: 3 * HOUR, activeMs: 2 * HOUR }, [
      node(
        'epic',
        '10',
        { costUsd: 4, progress: { completed: 1, total: 2 }, elapsedMs: 2 * DAY, open: true },
        [node('issue', '11', { costUsd: 4 }, [], { label: '#11 Fix', github: true })],
        { label: '#10 Epic', github: true },
      ),
      node('issue', '13', { costUsd: 2.5 }, [], { label: '#13 Guess' }),
    ]),
    node('repo', 'acme/api', {
      costUsd: 1,
      tokens: { input: 1000, output: 200, cacheRead: 3000, cacheWrite: 300 },
      sessions: 3,
      agents: 5,
      rotations: 1,
      waitMs: 30 * 60_000,
      elapsedMs: 90 * 60_000,
    }),
    node('unlinked', 'unlinked', { costUsd: 9 }, [], { label: 'Unlinked' }),
  ],
  {
    headline: headline({
      issuesClosed: 3,
      medianElapsedMs: DAY + 4 * HOUR,
      usdPerClosedIssue: 4.2,
      usdPerMergedPr: 3.1,
    }),
  },
)

const byDay: Audit = audit([
  node('group', '2026-10-12', {}, [
    node('repo', 'acme/api', { costUsd: 1, agentMs: HOUR / 2, activeMs: 9 * HOUR }),
    node('repo', 'acme/web', { costUsd: 2.5, agentMs: 3 * HOUR, activeMs: 9 * HOUR }),
  ]),
  node('group', '2026-10-13', {}, [
    node('repo', 'acme/web', { costUsd: 4, agentMs: HOUR, activeMs: 9 * HOUR }),
  ]),
])

const timelineDays: TimelineDay[] = [
  {
    day: '2026-10-12',
    spans: [
      { startMs: 9 * HOUR, endMs: 11 * HOUR, kind: 'active' },
      { startMs: 10 * HOUR, endMs: 12 * HOUR, kind: 'agent' },
    ],
    marks: [{ atMs: 11 * HOUR, label: '#11 closed' }],
  },
]

const perf = performance({ agents: [agentRow({ dispatches: 4, completed: 4, costUsd: 2 })] })

const calls = (groupBy: (g: string) => boolean) =>
  rollupMock.mock.calls.map(([f]) => f).filter((f) => groupBy(f.groupBy))
const treeCalls = () => calls((g) => g !== 'day')
const lastTreeFilter = () => treeCalls().at(-1) as AuditFilter

beforeEach(() => {
  vi.useFakeTimers({ toFake: ['Date'] })
  vi.setSystemTime(new Date(2026, 9, 13, 15, 0))
  rollupMock
    .mockReset()
    .mockImplementation((f) => Promise.resolve(f.groupBy === 'day' ? byDay : tree))
  performanceMock.mockReset().mockResolvedValue(perf)
  refreshMock.mockReset().mockResolvedValue()
  timelineMock.mockReset().mockResolvedValue(timelineDays)
  writeMock.mockReset().mockResolvedValue(true)
  listeners = []
  vi.mocked(onStoreChanged).mockImplementation((listener) => {
    listeners.push(listener)
    return Promise.resolve(() => {})
  })
})

afterEach(() => {
  vi.useRealTimers()
})

async function show(props: Partial<Parameters<typeof AuditView>[0]> = {}) {
  const view = render(<AuditView hidden={false} onBack={() => {}} {...props} />)
  await screen.findByRole('region', { name: 'Breakdown' })
  return view
}

const region = (name: string) => screen.getByRole('region', { name })
/** Clicks the sort button in the table's column header `name`. */
const sortBy = (name: string) =>
  fireEvent.click(
    within(within(region('Breakdown')).getByRole('columnheader', { name })).getByRole('button'),
  )
const tableRow = (name: RegExp) => within(region('Breakdown')).getByRole('row', { name })
const pick = (name: string) => fireEvent.click(screen.getByRole('radio', { name }))

/** Opens the Select named `label` and picks `option`. */
async function choose(label: string, option: string) {
  const trigger = screen.getByRole('combobox', { name: label })
  fireEvent.keyDown(trigger, { key: 'Enter' })
  fireEvent.click(await screen.findByRole('option', { name: option }))
}

/** The chart's data table: one row per day, the day then each series. */
const chartRows = () =>
  within(region('Per day'))
    .getAllByRole('row')
    .map((r) =>
      within(r)
        .queryAllByRole('cell')
        .map((c) => c.textContent),
    )
    .filter((cells) => cells.length > 0)

const tableRows = () =>
  within(region('Breakdown'))
    .getAllByRole('row')
    .slice(1)
    // The name each row's select button carries, markers aside.
    .map((r) => r.querySelector('[aria-pressed]')?.textContent)

describe('loading', () => {
  it('reads each rollup once on show, and never again on a store change', async () => {
    await show()
    await flush()
    expect(calls(() => true)).toHaveLength(2)
    for (let i = 0; i < 3; i++) storeChanged()
    await flush()
    expect(onStoreChanged).not.toHaveBeenCalled()
    expect(calls(() => true)).toHaveLength(2)
  })

  it('rereads each rollup, and the selected row, each time it is shown again', async () => {
    const { rerender } = await show()
    fireEvent.click(screen.getByRole('button', { name: 'acme/api' }))
    await flush()
    expect([calls(() => true).length, timelineMock.mock.calls.length]).toEqual([2, 1])
    rerender(<AuditView hidden onBack={() => {}} />)
    await flush()
    expect([calls(() => true).length, timelineMock.mock.calls.length]).toEqual([2, 1])
    rerender(<AuditView hidden={false} onBack={() => {}} />)
    await flush()
    expect(calls((g) => g === 'day')).toHaveLength(2)
    expect(treeCalls()).toHaveLength(2)
    expect(timelineMock).toHaveBeenCalledTimes(2)
  })

  it('reads nothing while hidden', async () => {
    render(<AuditView hidden onBack={() => {}} />)
    await act(() => Promise.resolve())
    expect(rollupMock).not.toHaveBeenCalled()
  })

  it('says it is loading until the first rollup lands', async () => {
    render(<AuditView hidden={false} onBack={() => {}} />)
    expect(screen.getByText('Loading…')).toBeInTheDocument()
    await screen.findByRole('region', { name: 'Breakdown' })
    expect(screen.queryByText('Loading…')).not.toBeInTheDocument()
  })

  it('reports a failed rollup', async () => {
    rollupMock.mockImplementation((f) =>
      f.groupBy === 'day' ? Promise.resolve(byDay) : Promise.reject('bad day: 2026-13-01'),
    )
    render(<AuditView hidden={false} onBack={() => {}} />)
    expect(await screen.findByRole('alert')).toHaveTextContent('bad day: 2026-13-01')
  })
})

describe('controls', () => {
  it('start on the last 7 local days, every repo, by Repo › Epic › Issue › PR', async () => {
    await show()
    expect(lastTreeFilter()).toEqual({
      from: '2026-10-07',
      to: '2026-10-13',
      repos: [],
      groupBy: 'hierarchy',
    })
    expect(screen.getByRole('radio', { name: '7d' })).toHaveAttribute('aria-checked', 'true')
  })

  it('switch the range to 14 or 30 days', async () => {
    await show()
    pick('14d')
    expect(lastTreeFilter()).toMatchObject({ from: '2026-09-30', to: '2026-10-13' })
    pick('30d')
    expect(lastTreeFilter()).toMatchObject({ from: '2026-09-14', to: '2026-10-13' })
  })

  it('take a custom range from the date picker, starting from the current one', async () => {
    await show()
    pick('Custom')
    expect(lastTreeFilter()).toMatchObject({ from: '2026-10-07', to: '2026-10-13' })
    fireEvent.click(screen.getByRole('button', { name: /October 7, 2026 – October 13, 2026/ }))
    fireEvent.click(await screen.findByRole('button', { name: /October 1st, 2026/ }))
    expect(lastTreeFilter()).toMatchObject({ from: '2026-10-01' })
    fireEvent.click(screen.getByRole('button', { name: /October 3rd, 2026/ }))
    expect(lastTreeFilter()).toMatchObject({ from: '2026-10-01', to: '2026-10-03' })
  })

  it('keep the custom range when the picker is cleared', async () => {
    await show()
    pick('Custom')
    fireEvent.click(screen.getByRole('button', { name: /October 7, 2026 – October 13, 2026/ }))
    const oct7 = () => screen.getByRole('button', { name: /October 7th, 2026/ })
    await screen.findByRole('grid', { name: /October 2026/ })
    fireEvent.click(oct7())
    expect(lastTreeFilter()).toMatchObject({ from: '2026-10-07', to: '2026-10-07' })
    // A one-day range picked again clears the picker; the filter keeps its last range.
    fireEvent.click(oct7())
    expect(lastTreeFilter()).toMatchObject({ from: '2026-10-07', to: '2026-10-07' })
    expect(
      screen.getByRole('button', { name: /October 7, 2026 – October 7, 2026/ }),
    ).toBeInTheDocument()
  })

  it('filter to one repo, picked from the repos with work in range', async () => {
    await show()
    await choose('Repo', 'acme/web')
    expect(lastTreeFilter()).toMatchObject({ repos: ['acme/web'] })
    const reads = treeCalls().length
    await choose('Repo', 'All repos')
    // Every repo was read at open, so it comes from the cache.
    expect(treeCalls()).toHaveLength(reads)
    expect(screen.getByRole('combobox', { name: 'Repo' })).toHaveTextContent('All repos')
  })

  it('keep the range and measure when their pressed item is clicked again', async () => {
    await show()
    pick('7d')
    expect(screen.getByRole('radio', { name: '7d' })).toHaveAttribute('aria-checked', 'true')
    pick('$')
    expect(screen.getByRole('radio', { name: '$' })).toHaveAttribute('aria-checked', 'true')
  })

  it('keep the last rollup on screen, marked busy, while a new filter is read', async () => {
    await show()
    const lands: (() => void)[] = []
    rollupMock.mockImplementation(
      (f) => new Promise((r) => lands.push(() => r(f.groupBy === 'day' ? byDay : tree))),
    )
    pick('14d')
    expect(screen.getByRole('main')).toHaveAttribute('aria-busy', 'true')
    expect(tableRows()).toEqual(['acme/web', 'acme/api', 'Unlinked'])
    await act(async () => lands.forEach((land) => land()))
    expect(screen.getByRole('main')).toHaveAttribute('aria-busy', 'false')
  })

  it('group by any of the spec groupings', async () => {
    await show()
    await choose('Group by', 'Model')
    expect(lastTreeFilter()).toMatchObject({ groupBy: 'model' })
    await choose('Group by', 'Agent type')
    expect(lastTreeFilter()).toMatchObject({ groupBy: 'agent_type' })
  })
})

describe('performance', () => {
  /** A tile's value, then its delta and hint, as text. */
  const tile = (label: string) =>
    [...(screen.getByText(label, { selector: 'dt' }).closest('dl') as HTMLElement).children]
      .slice(1)
      .map((d) => d.textContent)
  const perfCalls = () => performanceMock.mock.calls.map(([f]) => f)

  it('sits above the spend chart and tree: outcome, agents, bottleneck, waste, then spend', async () => {
    await show()
    await screen.findByRole('region', { name: 'Outcome' })
    const sections = within(screen.getByRole('main'))
      .getAllByRole('heading', { level: 2 })
      .map((h) => h.textContent)
    expect(sections).toEqual([
      'Outcome',
      'Agents & models',
      'Bottleneck',
      'Waste & rework',
      'Spend',
    ])
    expect(region('Spend')).toContainElement(region('Breakdown'))
  })

  it('takes only the median elapsed from the rollup, the other range figures from the days', async () => {
    await show()
    await screen.findByRole('region', { name: 'Outcome' })
    expect(tile('Median elapsed')).toEqual(['1d 4h', 'range: first window to close'])
    expect(tile('$ per closed issue')).toEqual(['$—'])
    expect(tile('Issues closed')).toEqual(['0', 'completed only'])
  })

  it('has no headline of its own any more: the outcome tiles carry it', async () => {
    await show()
    expect(screen.queryByRole('region', { name: 'Headline' })).toBeNull()
  })

  it('reads once on show under the range and repo, and never on a store change', async () => {
    await show()
    await flush()
    expect(perfCalls()).toEqual([
      expect.objectContaining({ from: '2026-10-07', to: '2026-10-13', repos: [] }),
    ])
    storeChanged()
    await flush()
    expect(performanceMock).toHaveBeenCalledOnce()
  })

  it('reads nothing while hidden, and rereads each time it is shown again', async () => {
    const { rerender } = render(<AuditView hidden onBack={() => {}} />)
    await flush()
    expect(performanceMock).not.toHaveBeenCalled()
    rerender(<AuditView hidden={false} onBack={() => {}} />)
    await flush()
    rerender(<AuditView hidden onBack={() => {}} />)
    rerender(<AuditView hidden={false} onBack={() => {}} />)
    await flush()
    expect(performanceMock).toHaveBeenCalledTimes(2)
  })

  it('follows the repo filter and range, but not the grouping', async () => {
    await show()
    await flush()
    await choose('Group by', 'Model')
    await flush()
    expect(performanceMock).toHaveBeenCalledOnce()
    await choose('Repo', 'acme/web')
    await flush()
    pick('14d')
    await flush()
    expect(perfCalls().slice(1)).toEqual([
      expect.objectContaining({ from: '2026-10-07', repos: ['acme/web'] }),
      expect.objectContaining({ from: '2026-09-30', repos: ['acme/web'] }),
    ])
  })

  it('rereads after a GitHub refresh', async () => {
    await show()
    await flush()
    fireEvent.click(screen.getByRole('button', { name: 'Refresh GitHub' }))
    await flush()
    expect(performanceMock).toHaveBeenCalledTimes(2)
  })

  it('reports a failed read and still shows the spend tree', async () => {
    performanceMock.mockRejectedValue('bad day: 2026-13-01')
    await show()
    await flush()
    expect(screen.getByRole('alert')).toHaveTextContent('bad day: 2026-13-01')
    expect(screen.queryByRole('region', { name: 'Outcome' })).toBeNull()
    expect(region('Breakdown')).toBeInTheDocument()
  })

  it('notes that cloud spend is not counted, only when it applies', async () => {
    await show()
    await screen.findByRole('region', { name: 'Outcome' })
    expect(screen.queryByText(/Cloud sessions/)).not.toBeInTheDocument()
    rollupMock.mockImplementation((f) =>
      Promise.resolve(
        f.groupBy === 'day' ? byDay : { ...tree, headline: headline({ cloudUncounted: true }) },
      ),
    )
    pick('14d')
    expect(await screen.findByText(/Cloud sessions are not counted/)).toBeInTheDocument()
  })

  it('warns about repos with no GitHub data', async () => {
    rollupMock.mockImplementation((f) =>
      Promise.resolve(
        f.groupBy === 'day'
          ? byDay
          : { ...tree, githubErrors: [{ repo: 'acme/web', error: 'gh: not logged in' }] },
      ),
    )
    render(<AuditView hidden={false} onBack={() => {}} />)
    expect(await screen.findByRole('alert')).toHaveTextContent('acme/web: gh: not logged in')
  })
})

describe('GitHub refresh', () => {
  const refreshButton = () => screen.getByRole('button', { name: /^Refresh(ing…| GitHub)$/ })

  it('never asks on its own: not on show, window focus or showing again', async () => {
    const { rerender } = await show()
    act(() => void window.dispatchEvent(new Event('focus')))
    rerender(<AuditView hidden onBack={() => {}} />)
    rerender(<AuditView hidden={false} onBack={() => {}} />)
    await flush()
    expect(refreshMock).not.toHaveBeenCalled()
  })

  it('is busy until the refresh is stored, then rereads each shown rollup and the row once', async () => {
    await show()
    fireEvent.click(screen.getByRole('button', { name: 'acme/api' }))
    await flush()
    let stored!: () => void
    refreshMock.mockReturnValue(new Promise((r) => (stored = r)))
    fireEvent.click(refreshButton())
    expect(refreshMock).toHaveBeenCalledOnce()
    expect(refreshButton()).toHaveTextContent('Refreshing…')
    expect(refreshButton()).toBeDisabled()
    await flush()
    expect([calls(() => true).length, timelineMock.mock.calls.length]).toEqual([2, 1])
    await act(async () => stored())
    await flush()
    expect(refreshButton()).toHaveTextContent('Refresh GitHub')
    expect(refreshButton()).toBeEnabled()
    expect(calls((g) => g === 'day')).toHaveLength(2)
    expect(treeCalls()).toHaveLength(2)
    expect(timelineMock).toHaveBeenCalledTimes(2)
  })

  it('rereads the one shared rollup once when the table is by day over every repo', async () => {
    await show()
    await choose('Group by', 'Day')
    await flush()
    const before = calls(() => true).length
    fireEvent.click(refreshButton())
    await flush()
    expect(calls(() => true)).toHaveLength(before + 1)
  })

  it('reports a failure, rereads nothing, and clears the report on the next try', async () => {
    await show()
    await flush()
    let fail!: (e: string) => void
    refreshMock.mockReturnValueOnce(new Promise((_, reject) => (fail = reject)))
    fireEvent.click(refreshButton())
    await act(async () => fail('no engine'))
    await flush()
    expect(screen.getByRole('alert')).toHaveTextContent('GitHub refresh failed: no engine')
    expect(refreshButton()).toBeEnabled()
    expect(calls(() => true)).toHaveLength(2)
    fireEvent.click(refreshButton())
    await flush()
    expect(screen.queryByRole('alert')).toBeNull()
  })

  it('recomputes no chart, table or performance view while it is busy', async () => {
    await show()
    await flush()
    refreshMock.mockReturnValue(new Promise(() => {}))
    const charts = vi.mocked(dailyChart).mock.calls.length
    const cells = vi.mocked(compact).mock.calls.length
    const sums = vi.mocked(totals).mock.calls.length
    expect(sums).toBeGreaterThan(0)
    fireEvent.click(refreshButton())
    expect(refreshButton()).toBeDisabled()
    expect(vi.mocked(dailyChart).mock.calls.length).toBe(charts)
    expect(vi.mocked(compact).mock.calls.length).toBe(cells)
    expect(vi.mocked(totals).mock.calls.length).toBe(sums)
  })
})

describe('daily chart', () => {
  it('stacks $ by repo, a row per day of the range, from an every-repo by-day rollup', async () => {
    await show()
    expect(within(region('Per day')).getByRole('columnheader', { name: 'Day' })).toBeInTheDocument()
    expect(calls((g) => g === 'day').at(-1)).toEqual({
      from: '2026-10-07',
      to: '2026-10-13',
      repos: [],
      groupBy: 'day',
    })
    const rows = chartRows()
    expect(rows).toHaveLength(7)
    expect(rows.at(-2)?.slice(1)).toEqual(['$1.00', '$2.50'])
    expect(rows.at(-1)?.slice(1)).toEqual(['$0.00', '$4.00'])
  })

  it('follows the $ / hours toggle, hours being agent-hours', async () => {
    await show()
    expect(within(region('Per day')).getByRole('heading')).toHaveTextContent(
      '$ per day, stacked by repo',
    )
    pick('Hours')
    expect(within(region('Per day')).getByRole('heading')).toHaveTextContent(
      'Agent-hours per day, stacked by repo',
    )
    expect(chartRows().at(-2)?.slice(1)).toEqual(['0.5h', '3h'])
  })

  it('shares one read with the table when it too is by day over every repo', async () => {
    await show()
    await choose('Group by', 'Day')
    expect(calls((g) => g === 'day')).toHaveLength(1)
    expect(tableRows()).toEqual(['2026-10-12', '2026-10-13'])
  })

  it('keeps only the picked repo, without a new by-day read', async () => {
    await show()
    const daily = calls((g) => g === 'day').length
    await choose('Repo', 'acme/web')
    expect(chartRows().at(-1)?.slice(1)).toEqual(['$4.00'])
    expect(calls((g) => g === 'day')).toHaveLength(daily)
  })
})

describe('tree table', () => {
  it('has a column for every spec metric, agent-hours marked as wall time', async () => {
    await show()
    const headers = within(region('Breakdown'))
      .getAllByRole('columnheader')
      .map((h) => h.textContent)
    expect(headers).toEqual([
      'Name',
      'Elapsed',
      'Active',
      'Agent-hours (wall)',
      'Waiting on you',
      '$',
      'Tokens',
      'Sessions',
      'Agents',
      'Rotations',
      'Progress',
    ])
  })

  it('shows each metric on its row', async () => {
    await show()
    const api = within(tableRow(/acme\/api/))
      .getAllByRole('cell')
      .map((c) => c.textContent)
    expect(api).toEqual([
      'acme/api',
      '1h 30m',
      '0m',
      '0m',
      '30m',
      '$1.00',
      '4.5k',
      '3',
      '5',
      '1',
      '—',
    ])
  })

  it('puts Unlinked last, whichever way a column sorts', async () => {
    await show()
    expect(tableRows()).toEqual(['acme/web', 'acme/api', 'Unlinked'])
    sortBy('$')
    expect(tableRows().at(-1)).toBe('Unlinked')
    sortBy('$')
    expect(tableRows().at(-1)).toBe('Unlinked')
    sortBy('Name')
    expect(tableRows()).toEqual(['acme/api', 'acme/web', 'Unlinked'])
  })

  it('sorts by elapsed, agent-hours and progress, a missing value lowest', async () => {
    await show()
    sortBy('Elapsed')
    expect(tableRows()).toEqual(['acme/api', 'acme/web', 'Unlinked'])
    sortBy('Agent-hours (wall)')
    expect(tableRows()).toEqual(['acme/web', 'acme/api', 'Unlinked'])
    fireEvent.click(screen.getByRole('button', { name: 'Expand repo acme/web' }))
    sortBy('Progress')
    expect(tableRows().slice(1, 3)).toEqual(['#10 Epic', '#13 Guess'])
    sortBy('Progress')
    expect(tableRows().slice(1, 3)).toEqual(['#13 Guess', '#10 Epic'])
  })

  it('expands a row into its children', async () => {
    await show()
    fireEvent.click(screen.getByRole('button', { name: 'Expand repo acme/web' }))
    fireEvent.click(screen.getByRole('button', { name: 'Expand epic #10 Epic' }))
    expect(tableRows()).toEqual([
      'acme/web',
      '#10 Epic',
      '#11 Fix',
      '#13 Guess',
      'acme/api',
      'Unlinked',
    ])
  })

  it('marks issues with no GitHub data, open elapsed and unpriced $, and agent-hours as measured', async () => {
    await show()
    fireEvent.click(screen.getByRole('button', { name: 'Expand repo acme/web' }))
    const guess = within(tableRow(/#13 Guess/))
    expect(guess.getByText('no GitHub')).toBeInTheDocument()
    expect(within(tableRow(/#10 Epic/)).queryByText('no GitHub')).toBeNull()
    const web = within(tableRow(/^acme\/web/))
      .getAllByRole('cell')
      .map((c) => c.textContent)
    // Background agent-hours are measured like foreground ones: no estimate marker (#60).
    expect(web[3]).toBe('3h 0m')
    expect(screen.queryByText(/≈/)).toBeNull()
    expect(web[5]).toBe('$6.50 +2 unpriced')
    const epic = within(tableRow(/#10 Epic/))
      .getAllByRole('cell')
      .map((c) => c.textContent)
    expect(epic[1]).toBe('2d 0h · open')
    expect(epic[10]).toBe('1/2')
  })

  it('selects a row by its path, and forgets it when the grouping changes', async () => {
    await show()
    fireEvent.click(screen.getByRole('button', { name: 'Expand repo acme/web' }))
    fireEvent.click(screen.getByRole('button', { name: 'Expand epic #10 Epic' }))
    const fix = () => screen.getByRole('button', { name: '#11 Fix' })
    expect(fix()).toHaveAttribute('aria-pressed', 'false')
    fireEvent.click(fix())
    expect(fix()).toHaveAttribute('aria-pressed', 'true')
    expect(screen.getByRole('button', { name: 'acme/web' })).toHaveAttribute(
      'aria-pressed',
      'false',
    )
    await choose('Group by', 'Day')
    await choose('Group by', 'Repo › Epic › Issue › PR')
    expect(fix()).toHaveAttribute('aria-pressed', 'false')
  })

  it('lets no row be picked while a new filter is read over the old tree', async () => {
    await show()
    rollupMock.mockImplementation(() => new Promise(() => {}))
    pick('14d')
    expect(screen.getByRole('button', { name: 'acme/web' })).toBeDisabled()
  })

  it('drops the selection once its row is gone from the new rollup', async () => {
    rollupMock.mockImplementation((f) =>
      Promise.resolve(
        f.groupBy === 'day'
          ? byDay
          : f.repos.length
            ? audit(tree.root.children.filter((n) => n.key === f.repos[0]))
            : tree,
      ),
    )
    await show()
    fireEvent.click(screen.getByRole('button', { name: 'acme/web' }))
    expect(screen.getByRole('complementary', { name: 'Row detail' })).toBeInTheDocument()
    await choose('Repo', 'acme/api')
    await screen.findByRole('button', { name: 'acme/api' })
    await vi.waitFor(() => expect(tableRows()).toEqual(['acme/api']))
    expect(screen.queryByRole('complementary', { name: 'Row detail' })).toBeNull()
  })

  it('keeps rows expanded by node, not position, across a change of grouping', async () => {
    await show()
    fireEvent.click(screen.getByRole('button', { name: 'Expand repo acme/web' }))
    await choose('Group by', 'Day')
    await choose('Group by', 'Repo › Epic › Issue › PR')
    expect(tableRows().slice(0, 3)).toEqual(['acme/web', '#10 Epic', '#13 Guess'])
  })
})

describe('row detail', () => {
  const detail = () => screen.getByRole('complementary', { name: 'Row detail' })
  const selectFix = () => {
    fireEvent.click(screen.getByRole('button', { name: 'Expand repo acme/web' }))
    fireEvent.click(screen.getByRole('button', { name: 'Expand epic #10 Epic' }))
    fireEvent.click(screen.getByRole('button', { name: '#11 Fix' }))
  }

  it('is closed, and reads nothing, until a row is selected', async () => {
    await show()
    expect(screen.queryByRole('complementary', { name: 'Row detail' })).toBeNull()
    expect(timelineMock).not.toHaveBeenCalled()
  })

  it('docks at 38% under the table and shows the timeline of the selected row', async () => {
    await show()
    selectFix()
    expect(await within(detail()).findByText('#11 closed')).toBeInTheDocument()
    expect(timelineMock).toHaveBeenLastCalledWith(
      { from: '2026-10-07', to: '2026-10-13', repos: [], groupBy: 'hierarchy' },
      [
        ['repo', 'acme/web'],
        ['epic', '10'],
        ['issue', '11'],
      ],
    )
    expect(within(detail()).getByRole('heading', { name: '#11 Fix' })).toBeInTheDocument()
    expect(within(detail()).getAllByText('Oct 12').length).toBeGreaterThan(0)
    expect(screen.getByTestId('panel-audit-detail')).toHaveAttribute('data-default-size', '38%')
    expect(screen.getByTestId('panel-audit-detail')).toContainElement(detail())
  })

  it('says its agent time is non-idle, unlike the wall time of the table column', async () => {
    await show()
    selectFix()
    expect((await within(detail()).findAllByText('Agent (non-idle)')).length).toBeGreaterThan(0)
  })

  it('names each span kind in its legend: active, wait, then agent', async () => {
    await show()
    selectFix()
    await within(detail()).findAllByText('Agent (non-idle)')
    for (const label of ['Active', 'Waiting on you']) {
      expect(within(detail()).getAllByText(label).length).toBeGreaterThan(0)
    }
  })

  it('follows the selection to another row', async () => {
    await show()
    selectFix()
    fireEvent.click(screen.getByRole('button', { name: '#13 Guess' }))
    expect(within(detail()).getByRole('heading', { name: '#13 Guess' })).toBeInTheDocument()
    await act(() => Promise.resolve())
    expect(timelineMock.mock.lastCall?.[1]).toEqual([
      ['repo', 'acme/web'],
      ['issue', '13'],
    ])
  })

  it('closes with its close button, or by selecting the row again', async () => {
    await show()
    selectFix()
    fireEvent.click(within(detail()).getByRole('button', { name: 'Close row detail' }))
    expect(screen.queryByRole('complementary', { name: 'Row detail' })).toBeNull()
    expect(screen.getByRole('button', { name: '#11 Fix' })).toHaveAttribute('aria-pressed', 'false')
    fireEvent.click(screen.getByRole('button', { name: '#11 Fix' }))
    expect(detail()).toBeInTheDocument()
    fireEvent.click(screen.getByRole('button', { name: '#11 Fix' }))
    expect(screen.queryByRole('complementary', { name: 'Row detail' })).toBeNull()
  })

  it('hands focus back to the selected row on close, or to the table when it is hidden', async () => {
    await show()
    selectFix()
    fireEvent.click(within(detail()).getByRole('button', { name: 'Close row detail' }))
    expect(screen.getByRole('button', { name: '#11 Fix' })).toHaveFocus()
    fireEvent.click(screen.getByRole('button', { name: '#11 Fix' }))
    fireEvent.click(screen.getByRole('button', { name: 'Collapse repo acme/web' }))
    fireEvent.click(within(detail()).getByRole('button', { name: 'Close row detail' }))
    expect(region('Breakdown')).toHaveFocus()
  })

  it('closes when the grouping changes, since its path means nothing there', async () => {
    await show()
    selectFix()
    await choose('Group by', 'Day')
    expect(screen.queryByRole('complementary', { name: 'Row detail' })).toBeNull()
  })

  it('says so when the row has no activity in range', async () => {
    timelineMock.mockResolvedValue([])
    await show()
    selectFix()
    expect(await within(detail()).findByText('No activity in this range.')).toBeInTheDocument()
  })

  it('shows a failed read', async () => {
    timelineMock.mockRejectedValue('bad day: 2026-13-01')
    await show()
    selectFix()
    expect(await within(detail()).findByRole('alert')).toHaveTextContent('bad day: 2026-13-01')
  })

  it('warns that marks name no repo only for a row that spans repos', async () => {
    await show()
    await choose('Group by', 'Day')
    const note = /spans repos/
    fireEvent.click(await screen.findByRole('button', { name: '2026-10-12' }))
    expect(within(detail()).getByText(note)).toBeInTheDocument()
    fireEvent.click(screen.getByRole('button', { name: 'Expand group 2026-10-12' }))
    fireEvent.click(screen.getByRole('button', { name: 'acme/api' }))
    expect(within(detail()).queryByText(note)).toBeNull()
  })

  it('does not warn for a PR group, which is one repo', async () => {
    rollupMock.mockImplementation((f) =>
      Promise.resolve(
        f.groupBy === 'day'
          ? byDay
          : f.groupBy === 'pr'
            ? audit([node('group', 'acme/web#12', {}, [], { label: 'PR #12' })])
            : tree,
      ),
    )
    await show()
    await choose('Group by', 'PR')
    fireEvent.click(await screen.findByRole('button', { name: 'PR #12' }))
    expect(within(detail()).queryByText(/spans repos/)).toBeNull()
  })
})

describe('export', () => {
  const exportButton = () => screen.getByRole('button', { name: 'Export CSV' })

  it('is off until a rollup is shown, and while a new filter is read', async () => {
    render(<AuditView hidden={false} onBack={() => {}} />)
    expect(exportButton()).toBeDisabled()
    await screen.findByRole('region', { name: 'Breakdown' })
    expect(exportButton()).toBeEnabled()
    rollupMock.mockImplementation(() => new Promise(() => {}))
    pick('14d')
    expect(exportButton()).toBeDisabled()
  })

  it('hands the whole tree under the filter to the app, which asks where to save it', async () => {
    await show()
    fireEvent.click(exportButton())
    await vi.waitFor(() => expect(writeMock).toHaveBeenCalledOnce())
    const [text, name] = writeMock.mock.calls[0] as [string, string]
    expect(name).toBe('strate-audit-2026-10-07_2026-10-13.csv')
    // Collapsed rows are in it too.
    const rows = text.split('\r\n')
    expect(rows[0]).toMatch(/^\uFEFFKind,Level 1,Level 2,Level 3,Path,GitHub,/)
    expect(rows.filter((r) => r.startsWith('issue,acme/web,#10 Epic,#11 Fix,'))).toHaveLength(1)
    expect(rows.filter((r) => r.startsWith('unlinked,Unlinked,'))).toHaveLength(1)
  })

  it('shows nothing when the save dialog is cancelled', async () => {
    writeMock.mockResolvedValue(false)
    await show()
    fireEvent.click(exportButton())
    await vi.waitFor(() => expect(writeMock).toHaveBeenCalledOnce())
    await act(() => Promise.resolve())
    expect(screen.queryByRole('alert')).toBeNull()
  })

  it('reports a failed write, and clears the report on the next export', async () => {
    writeMock.mockRejectedValueOnce('Access is denied. (os error 5)')
    await show()
    fireEvent.click(exportButton())
    expect(await screen.findByRole('alert')).toHaveTextContent(
      'Export failed: Access is denied. (os error 5)',
    )
    fireEvent.click(exportButton())
    await vi.waitFor(() => expect(screen.queryByRole('alert')).toBeNull())
  })
})

describe('control bar', () => {
  it('is the only bar: the Audit heading at its start, ‹ Live at its end, going back', async () => {
    const onBack = vi.fn()
    await show({ onBack })
    expect(screen.queryByRole('banner')).toBeNull()
    const heading = screen.getByRole('heading', { level: 1, name: 'Audit' })
    const bar = heading.parentElement as HTMLElement
    expect(bar.firstElementChild).toBe(heading)
    expect(within(bar).getByRole('radio', { name: '7d' })).toBeInTheDocument()
    const buttons = within(bar)
      .getAllByRole('button')
      .map((b) => b.textContent)
    expect(buttons.slice(-3)).toEqual(['Refresh GitHub', 'Export CSV', 'Live'])
    fireEvent.click(screen.getByRole('button', { name: 'Live' }))
    expect(onBack).toHaveBeenCalledOnce()
  })
})
