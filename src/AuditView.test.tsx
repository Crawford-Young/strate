import { act, fireEvent, render, screen, within } from '@testing-library/react'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { audit, headline, node, slice } from '../tests/audit-fixtures'
import { agentRow, performance } from '../tests/performance-fixtures'
import {
  auditPerformance,
  auditRefresh,
  auditRollup,
  auditTimeline,
  onStoreChanged,
  writeExport,
  type Audit,
  type TimelineDay,
} from './api'
import { compact, dailyChart, shortDay } from './audit'
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
// The top rows carry their $ and agent-hours per day, for the chart and the By day column.
const tree: Audit = audit(
  [
    node(
      'repo',
      'acme/web',
      {
        costUsd: 6.5,
        unpriced: 2,
        agentMs: 4 * HOUR,
        activeMs: 2 * HOUR,
        byDay: [slice('2026-10-12', 2.5, 3 * HOUR), slice('2026-10-13', 4, HOUR)],
        byAgentType: [slice('implementer', 4, 4 * HOUR), slice('orchestrator', 2.5)],
        byModel: [slice('model-a', 6.5, 4 * HOUR)],
      },
      [
        node(
          'epic',
          '10',
          { costUsd: 4, progress: { completed: 1, total: 2 }, elapsedMs: 2 * DAY, open: true },
          [node('issue', '11', { costUsd: 4 }, [], { label: '#11 Fix', github: true })],
          { label: '#10 Epic', github: true },
        ),
        node('issue', '13', { costUsd: 2.5 }, [], { label: '#13 Guess' }),
      ],
    ),
    node('repo', 'acme/api', {
      costUsd: 1,
      agentMs: HOUR / 2,
      byDay: [slice('2026-10-12', 1, HOUR / 2)],
      tokens: { input: 1000, output: 200, cacheRead: 3000, cacheWrite: 300 },
      sessions: 3,
      agents: 5,
      waitMs: 30 * 60_000,
      elapsedMs: 90 * 60_000,
    }),
    node('unlinked', '', { costUsd: 9, byDay: [slice('2026-10-11', 9)] }, [], {
      label: 'Unlinked',
    }),
  ],
  {
    repos: ['acme/api', 'acme/web'],
    unlinked: true,
    headline: headline({
      issuesClosed: 3,
      medianElapsedMs: DAY + 4 * HOUR,
      usdPerClosedIssue: 4.2,
      usdPerMergedPr: 3.1,
    }),
  },
)

// The root holds every split key its rows do: the breakdown colours are picked over it.
tree.root.metrics.byAgentType = [slice('implementer', 4, 4 * HOUR), slice('orchestrator', 2.5)]
tree.root.metrics.byModel = [slice('model-a', 6.5, 4 * HOUR)]

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

const reads = () => rollupMock.mock.calls.length
const lastFilter = () => rollupMock.mock.lastCall?.[0]

beforeEach(() => {
  vi.useFakeTimers({ toFake: ['Date'] })
  vi.setSystemTime(new Date(2026, 9, 13, 15, 0))
  rollupMock.mockReset().mockResolvedValue(tree)
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
/** The name button of the tree row `name`, which a repo chip may share. */
const rowButton = (name: string) => within(region('Breakdown')).getByRole('button', { name })
const pick = (name: string) => fireEvent.click(screen.getByRole('radio', { name }))

/** The repo chip `name`. */
const chip = (name: string) =>
  within(screen.getByRole('group', { name: 'Repos' })).getByRole('button', { name })

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
  it('reads the rollup once on show, and never again on a store change', async () => {
    await show()
    await flush()
    expect(reads()).toBe(1)
    for (let i = 0; i < 3; i++) storeChanged()
    await flush()
    expect(onStoreChanged).not.toHaveBeenCalled()
    expect(reads()).toBe(1)
  })

  it('rereads the rollup, and the selected row, each time it is shown again', async () => {
    const { rerender } = await show()
    fireEvent.click(rowButton('acme/api'))
    await flush()
    expect([reads(), timelineMock.mock.calls.length]).toEqual([1, 1])
    rerender(<AuditView hidden onBack={() => {}} />)
    await flush()
    expect([reads(), timelineMock.mock.calls.length]).toEqual([1, 1])
    rerender(<AuditView hidden={false} onBack={() => {}} />)
    await flush()
    expect([reads(), timelineMock.mock.calls.length]).toEqual([2, 2])
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
    rollupMock.mockRejectedValue('bad day: 2026-13-01')
    render(<AuditView hidden={false} onBack={() => {}} />)
    expect(await screen.findByRole('alert')).toHaveTextContent('bad day: 2026-13-01')
  })
})

describe('controls', () => {
  it('start on the last 7 local days and every repo', async () => {
    await show()
    expect(lastFilter()).toEqual({
      from: '2026-10-07',
      to: '2026-10-13',
      repos: [],
    })
    expect(screen.getByRole('radio', { name: '7d' })).toHaveAttribute('aria-checked', 'true')
    expect(chip('All repos')).toHaveAttribute('aria-pressed', 'true')
  })

  it('switch the range to 14 or 30 days', async () => {
    await show()
    pick('14d')
    expect(lastFilter()).toMatchObject({ from: '2026-09-30', to: '2026-10-13' })
    pick('30d')
    expect(lastFilter()).toMatchObject({ from: '2026-09-14', to: '2026-10-13' })
  })

  it('take a custom range from the date picker, starting from the current one', async () => {
    await show()
    pick('Custom')
    expect(lastFilter()).toMatchObject({ from: '2026-10-07', to: '2026-10-13' })
    fireEvent.click(screen.getByRole('button', { name: /October 7, 2026 – October 13, 2026/ }))
    fireEvent.click(await screen.findByRole('button', { name: /October 1st, 2026/ }))
    expect(lastFilter()).toMatchObject({ from: '2026-10-01' })
    fireEvent.click(screen.getByRole('button', { name: /October 3rd, 2026/ }))
    expect(lastFilter()).toMatchObject({ from: '2026-10-01', to: '2026-10-03' })
  })

  it('keep the custom range when the picker is cleared', async () => {
    await show()
    pick('Custom')
    fireEvent.click(screen.getByRole('button', { name: /October 7, 2026 – October 13, 2026/ }))
    const oct7 = () => screen.getByRole('button', { name: /October 7th, 2026/ })
    await screen.findByRole('grid', { name: /October 2026/ })
    fireEvent.click(oct7())
    expect(lastFilter()).toMatchObject({ from: '2026-10-07', to: '2026-10-07' })
    // A one-day range picked again clears the picker; the filter keeps its last range.
    fireEvent.click(oct7())
    expect(lastFilter()).toMatchObject({ from: '2026-10-07', to: '2026-10-07' })
    expect(
      screen.getByRole('button', { name: /October 7, 2026 – October 7, 2026/ }),
    ).toBeInTheDocument()
  })

  it("list the read's repos as chips, then Unlinked, with All repos first", async () => {
    await show()
    const chips = within(screen.getByRole('group', { name: 'Repos' }))
      .getAllByRole('button')
      .map((b) => b.textContent)
    expect(chips).toEqual(['All repos', 'acme/api', 'acme/web', 'Unlinked'])
  })

  it('filter to any repos picked, Unlinked as the empty slug, All repos for every one', async () => {
    await show()
    fireEvent.click(chip('acme/web'))
    expect(lastFilter()).toMatchObject({ repos: ['acme/web'] })
    expect(chip('All repos')).toHaveAttribute('aria-pressed', 'false')
    expect(chip('acme/web')).toHaveAttribute('aria-pressed', 'true')
    fireEvent.click(chip('Unlinked'))
    expect(lastFilter()).toMatchObject({ repos: ['acme/web', ''] })
    fireEvent.click(chip('acme/web'))
    expect(lastFilter()).toMatchObject({ repos: [''] })
    const before = reads()
    fireEvent.click(chip('All repos'))
    // Every repo was read at open, so it comes from the cache.
    expect(reads()).toBe(before)
    expect(chip('All repos')).toHaveAttribute('aria-pressed', 'true')
    expect(chip('Unlinked')).toHaveAttribute('aria-pressed', 'false')
  })

  it('fall back to every repo when the last picked one is unpicked, or All repos is clicked again', async () => {
    await show()
    fireEvent.click(chip('acme/api'))
    fireEvent.click(chip('acme/api'))
    expect(chip('All repos')).toHaveAttribute('aria-pressed', 'true')
    expect(chip('acme/api')).toHaveAttribute('aria-pressed', 'false')
    fireEvent.click(chip('All repos'))
    expect(chip('All repos')).toHaveAttribute('aria-pressed', 'true')
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
    rollupMock.mockImplementation(() => new Promise((r) => lands.push(() => r(tree))))
    pick('14d')
    expect(screen.getByRole('main')).toHaveAttribute('aria-busy', 'true')
    expect(tableRows()).toEqual(['acme/web', 'acme/api', 'Unlinked'])
    await act(async () => lands.forEach((land) => land()))
    expect(screen.getByRole('main')).toHaveAttribute('aria-busy', 'false')
  })

  it('have no Group by picker: every dimension is a column of the one tree', async () => {
    await show()
    expect(screen.queryByRole('combobox')).toBeNull()
    expect(screen.queryByText(/Group by/)).toBeNull()
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

  it('follows the repo chips and range, but not the measure', async () => {
    await show()
    await flush()
    pick('Hours')
    await flush()
    expect(performanceMock).toHaveBeenCalledOnce()
    fireEvent.click(chip('acme/web'))
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
    rollupMock.mockResolvedValue({ ...tree, headline: headline({ cloudUncounted: true }) })
    pick('14d')
    expect(await screen.findByText(/Cloud sessions are not counted/)).toBeInTheDocument()
  })

  it('warns about repos with no GitHub data', async () => {
    rollupMock.mockResolvedValue({
      ...tree,
      githubErrors: [{ repo: 'acme/web', error: 'gh: not logged in' }],
    })
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

  it('is busy until the refresh is stored, then rereads the rollup and the row once', async () => {
    await show()
    fireEvent.click(rowButton('acme/api'))
    await flush()
    let stored!: () => void
    refreshMock.mockReturnValue(new Promise((r) => (stored = r)))
    fireEvent.click(refreshButton())
    expect(refreshMock).toHaveBeenCalledOnce()
    expect(refreshButton()).toHaveTextContent('Refreshing…')
    expect(refreshButton()).toBeDisabled()
    await flush()
    expect([reads(), timelineMock.mock.calls.length]).toEqual([1, 1])
    await act(async () => stored())
    await flush()
    expect(refreshButton()).toHaveTextContent('Refresh GitHub')
    expect(refreshButton()).toBeEnabled()
    expect([reads(), timelineMock.mock.calls.length]).toEqual([2, 2])
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
    expect(reads()).toBe(1)
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
  it("stacks $ by the tree's top rows, a row per day of the range, from the tree's own read", async () => {
    await show()
    await flush()
    expect(within(region('Per day')).getByRole('columnheader', { name: 'Day' })).toBeInTheDocument()
    expect(reads()).toBe(1)
    const rows = chartRows()
    expect(rows).toHaveLength(7)
    expect(rows.at(-3)?.slice(1)).toEqual(['$0.00', '$0.00', '$9.00'])
    expect(rows.at(-2)?.slice(1)).toEqual(['$2.50', '$1.00', '$0.00'])
    expect(rows.at(-1)?.slice(1)).toEqual(['$4.00', '$0.00', '$0.00'])
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
    expect(chartRows().at(-2)?.slice(1)).toEqual(['3h', '0.5h', '0h'])
  })

  it('charts the rows of the filtered tree, so a repo picked charts what its tree shows', async () => {
    // A unit of acme/web whose director sits under acme/ops: the tree shows its $ there.
    const ops = node('repo', 'acme/ops', { costUsd: 3, byDay: [slice('2026-10-13', 3)] })
    rollupMock.mockImplementation((f) =>
      Promise.resolve(f.repos.length ? audit([ops], { repos: tree.repos }) : tree),
    )
    await show()
    fireEvent.click(chip('acme/web'))
    await flush()
    expect(chartRows().at(-1)?.slice(1)).toEqual(['$3.00'])
    expect(within(region('Per day')).getByRole('list', { name: 'Legend' })).toHaveTextContent(
      /^acme\/ops$/,
    )
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
      'By day',
      'By agent type',
      'By model',
      'Tokens',
      'Sessions',
      'Agents',
      'Progress',
    ])
  })

  it('shows each metric on its row', async () => {
    await show()
    const api = within(tableRow(/acme\/api/))
      .getAllByRole('cell')
      .map((c) => c.textContent)
    // The name carries its level badge; the breakdown cells draw bars, with no text.
    expect(api).toEqual([
      'acme/apirepo',
      '1h 30m',
      '0m',
      '30m',
      '30m',
      '$1.00',
      '',
      '',
      '',
      '4.5k',
      '3',
      '5',
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
    expect(web[3]).toBe('4h 0m')
    expect(screen.queryByText(/≈/)).toBeNull()
    expect(web[5]).toBe('$6.50 +2 unpriced')
    const epic = within(tableRow(/#10 Epic/))
      .getAllByRole('cell')
      .map((c) => c.textContent)
    expect(epic[1]).toBe('2d 0h · open')
    expect(epic[12]).toBe('1/2')
  })

  it('selects a row by its path', async () => {
    await show()
    fireEvent.click(screen.getByRole('button', { name: 'Expand repo acme/web' }))
    fireEvent.click(screen.getByRole('button', { name: 'Expand epic #10 Epic' }))
    const fix = () => screen.getByRole('button', { name: '#11 Fix' })
    expect(fix()).toHaveAttribute('aria-pressed', 'false')
    fireEvent.click(fix())
    expect(fix()).toHaveAttribute('aria-pressed', 'true')
    expect(rowButton('acme/web')).toHaveAttribute('aria-pressed', 'false')
  })

  it('draws By day over the range and the splits on the measure the chart uses', async () => {
    await show()
    const web = within(tableRow(/^acme\/web/))
    const days = ['2026-10-07', '2026-10-08', '2026-10-09', '2026-10-10', '2026-10-11']
    const quiet = days.map((d) => `${shortDay(d)} $0.00`).join(', ')
    expect(web.getByRole('img', { name: /^By day/ })).toHaveAccessibleName(
      `By day: ${quiet}, ${shortDay('2026-10-12')} $2.50, ${shortDay('2026-10-13')} $4.00`,
    )
    expect(web.getByText('implementer 62%')).toBeInTheDocument()
    pick('Hours')
    expect(web.getByRole('img', { name: /^By agent type/ })).toHaveAccessibleName(
      'By agent type: implementer 4h (100%)',
    )
    expect(web.getByRole('img', { name: /^By model/ })).toHaveAccessibleName(
      'By model: model-a 4h (100%)',
    )
  })

  it('lets no row be picked while a new filter is read over the old tree', async () => {
    await show()
    rollupMock.mockImplementation(() => new Promise(() => {}))
    pick('14d')
    expect(rowButton('acme/web')).toBeDisabled()
  })

  it('drops the selection once its row is gone from the new rollup', async () => {
    rollupMock.mockImplementation((f) =>
      Promise.resolve(
        f.repos.length ? audit(tree.root.children.filter((n) => n.key === f.repos[0])) : tree,
      ),
    )
    await show()
    fireEvent.click(rowButton('acme/web'))
    expect(screen.getByRole('complementary', { name: 'Row detail' })).toBeInTheDocument()
    fireEvent.click(chip('acme/api'))
    await within(region('Breakdown')).findByRole('button', { name: 'acme/api' })
    await vi.waitFor(() => expect(tableRows()).toEqual(['acme/api']))
    expect(screen.queryByRole('complementary', { name: 'Row detail' })).toBeNull()
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
      { from: '2026-10-07', to: '2026-10-13', repos: [] },
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

describe('scrolling', () => {
  it('scrolls once: the page clips, and the main area leaves scrolling to its panel', async () => {
    const { container } = await show()
    expect((container.firstElementChild as HTMLElement).className).toMatch(/\boverflow-hidden\b/)
    const main = screen.getByRole('main')
    expect(main.className).not.toMatch(/overflow|h-full/)
    expect(screen.getByTestId('panel-audit-main')).toContainElement(main)
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
    expect(within(bar).getByRole('group', { name: 'Repos' })).toBeInTheDocument()
    expect(within(bar).getByRole('radio', { name: 'Hours' })).toBeInTheDocument()
    const buttons = within(bar)
      .getAllByRole('button')
      .map((b) => b.textContent)
    expect(buttons.slice(-3)).toEqual(['Refresh GitHub', 'Export CSV', 'Live'])
    fireEvent.click(screen.getByRole('button', { name: 'Live' }))
    expect(onBack).toHaveBeenCalledOnce()
  })
})
