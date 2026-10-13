import { act, fireEvent, render, screen, within } from '@testing-library/react'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { audit, headline, node } from '../tests/audit-fixtures'
import {
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

vi.mock('./api', () => ({
  auditRollup: vi.fn(),
  auditRefresh: vi.fn(),
  auditTimeline: vi.fn(),
  writeExport: vi.fn(),
  onStoreChanged: vi.fn(),
}))
// Spied, to see what a store change while hidden recomputes.
vi.mock('./audit', async (importOriginal) => {
  const real = await importOriginal<typeof import('./audit')>()
  return { ...real, dailyChart: vi.fn(real.dailyChart), compact: vi.fn(real.compact) }
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
const refreshMock = vi.mocked(auditRefresh)
const timelineMock = vi.mocked(auditTimeline)
const writeMock = vi.mocked(writeExport)
// Every store-changed listener; the event reaches them all.
let listeners: (() => void)[] = []
const storeChanged = () => act(() => listeners.forEach((listener) => listener()))

const HOUR = 3_600_000
const DAY = 24 * HOUR

// Hierarchy: acme/web (an epic with a GitHub issue, plus an issue known only from a window
// name), acme/api, then Unlinked, which costs the most so a sort would move it if it could.
const tree: Audit = audit(
  [
    node(
      'repo',
      'acme/web',
      { costUsd: 6.5, unpriced: 2, agentMs: 3 * HOUR, approximate: true, activeMs: 2 * HOUR },
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
  it('recomputes no chart or table on store changes while hidden', async () => {
    const { rerender } = await show()
    rerender(<AuditView hidden onBack={() => {}} />)
    const charts = vi.mocked(dailyChart).mock.calls.length
    const cells = vi.mocked(compact).mock.calls.length
    for (let i = 0; i < 3; i++) storeChanged()
    expect(vi.mocked(dailyChart).mock.calls.length).toBe(charts)
    expect(vi.mocked(compact).mock.calls.length).toBe(cells)
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

describe('headline', () => {
  it('shows issues closed, median elapsed, $ per closed issue and $ per merged PR', async () => {
    await show()
    const kpis = region('Headline')
    expect(within(kpis).getByText('Issues closed').nextSibling).toHaveTextContent('3')
    expect(within(kpis).getByText('Median elapsed').nextSibling).toHaveTextContent('1d 4h')
    expect(within(kpis).getByText('$ per closed issue').nextSibling).toHaveTextContent('$4.20')
    expect(within(kpis).getByText('$ per merged PR').nextSibling).toHaveTextContent('$3.10')
  })

  it('shows dashes when nothing closed or merged', async () => {
    rollupMock.mockImplementation((f) => Promise.resolve(f.groupBy === 'day' ? byDay : audit([])))
    render(<AuditView hidden={false} onBack={() => {}} />)
    const kpis = await screen.findByRole('region', { name: 'Headline' })
    expect(within(kpis).getByText('Median elapsed').nextSibling).toHaveTextContent('—')
    expect(within(kpis).getByText('$ per closed issue').nextSibling).toHaveTextContent('$—')
  })

  it('notes that cloud spend is not counted, only when it applies', async () => {
    await show()
    expect(screen.queryByText(/Cloud sessions/)).not.toBeInTheDocument()
    rollupMock.mockImplementation((f) =>
      Promise.resolve(
        f.groupBy === 'day' ? byDay : { ...tree, headline: headline({ cloudUncounted: true }) },
      ),
    )
    pick('14d')
    expect(await screen.findByText(/Cloud sessions are not counted/)).toBeInTheDocument()
  })

  it('warns about repos with no GitHub data and refreshes it on request', async () => {
    rollupMock.mockImplementation((f) =>
      Promise.resolve(
        f.groupBy === 'day'
          ? byDay
          : { ...tree, githubErrors: [{ repo: 'acme/web', error: 'gh: not logged in' }] },
      ),
    )
    render(<AuditView hidden={false} onBack={() => {}} />)
    const banner = await screen.findByRole('alert')
    expect(banner).toHaveTextContent('acme/web: gh: not logged in')
    fireEvent.click(within(banner).getByRole('button', { name: 'Refresh' }))
    expect(refreshMock).toHaveBeenCalledOnce()
  })

  it('turns Refresh off while it is asked for, and reports a failure', async () => {
    rollupMock.mockImplementation((f) =>
      Promise.resolve(
        f.groupBy === 'day'
          ? byDay
          : { ...tree, githubErrors: [{ repo: 'acme/web', error: 'gh: not logged in' }] },
      ),
    )
    let fail!: (e: string) => void
    refreshMock.mockReturnValue(new Promise((_, reject) => (fail = reject)))
    render(<AuditView hidden={false} onBack={() => {}} />)
    const refresh = await screen.findByRole('button', { name: 'Refresh' })
    fireEvent.click(refresh)
    expect(screen.getByRole('button', { name: 'Refreshing…' })).toBeDisabled()
    await act(async () => fail('no engine'))
    expect(screen.getByText('Refresh failed: no engine')).toBeInTheDocument()
    expect(screen.getByRole('button', { name: 'Refresh' })).toBeEnabled()
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

  it('marks issues with no GitHub data, approximate agent-hours, open elapsed and unpriced $', async () => {
    await show()
    fireEvent.click(screen.getByRole('button', { name: 'Expand repo acme/web' }))
    const guess = within(tableRow(/#13 Guess/))
    expect(guess.getByText('no GitHub')).toBeInTheDocument()
    expect(within(tableRow(/#10 Epic/)).queryByText('no GitHub')).toBeNull()
    const web = within(tableRow(/^acme\/web/))
      .getAllByRole('cell')
      .map((c) => c.textContent)
    expect(web[3]).toBe('≈3h 0m')
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

describe('header', () => {
  it('goes back to the live view with ‹ Live', async () => {
    const onBack = vi.fn()
    await show({ onBack })
    fireEvent.click(screen.getByRole('button', { name: 'Live' }))
    expect(onBack).toHaveBeenCalledOnce()
  })
})
