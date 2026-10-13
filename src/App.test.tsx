import { act, cleanup, fireEvent, render, screen, within } from '@testing-library/react'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { defaultLayoutOf, dragTo, resetGroups } from '../tests/resizable-mock'
import {
  liveGraph,
  onStoreChanged,
  status,
  workstreams,
  type GraphAgent,
  type LiveGraph,
  type Workstream,
} from './api'
import { App } from './App'
import { PREFS_KEY } from './layout'

vi.mock('./api', () => ({
  status: vi.fn(),
  workstreams: vi.fn(),
  liveGraph: vi.fn(),
  onStoreChanged: vi.fn(),
}))

vi.mock('./elk', async () => {
  const { default: ELK } = await import('elkjs/lib/elk.bundled.js')
  const elk = new ELK()
  return { elk: () => elk }
})

vi.mock('@xyflow/react', async (importOriginal) => ({
  ...(await importOriginal<typeof import('@xyflow/react')>()),
  ...(await import('../tests/xyflow-mock')),
}))

vi.mock('@crawfordyoung/ui', async (importOriginal) => ({
  ...(await importOriginal<typeof import('@crawfordyoung/ui')>()),
  ...(await import('../tests/resizable-mock')),
}))

const statusMock = vi.mocked(status)
const workstreamsMock = vi.mocked(workstreams)
const liveGraphMock = vi.mocked(liveGraph)
const onStoreChangedMock = vi.mocked(onStoreChanged)

function ws(over: Partial<Workstream> & Pick<Workstream, 'id' | 'name'>): Workstream {
  return {
    fallback: false,
    costUsd: null,
    unpriced: 0,
    state: 'idle',
    needsYou: 0,
    sessionId: `s-${over.id}`,
    live: false,
    startedAt: null,
    ...over,
  }
}

const finished = ws({ id: 1, name: 'docs-cleanup', costUsd: 1.5, state: 'done' })
const shell = ws({
  id: 2,
  name: 'strate-17',
  costUsd: 3.1,
  state: 'needs_you',
  needsYou: 2,
  live: true,
  startedAt: '2026-10-12T10:00:00Z',
})
const unnamed = ws({ id: 3, name: '/work/app@main', fallback: true, state: 'working', live: true })

function agent(over: Partial<GraphAgent> & Pick<GraphAgent, 'id'>): GraphAgent {
  return {
    agentId: `a-${over.id}`,
    kind: 'subagent',
    name: null,
    agentType: null,
    description: null,
    model: null,
    state: 'working',
    costUsd: null,
    contextTokens: null,
    contextPct: null,
    nudgePct: 80,
    hardStopPct: 95,
    parent: null,
    ...over,
  }
}

const shellGraph: LiveGraph = {
  sessionId: 's-2',
  live: true,
  agents: [
    agent({
      id: 10,
      agentId: null,
      kind: 'orchestrator',
      name: 'strate-17',
      model: 'claude-opus-5-5',
      costUsd: 0.3,
      contextPct: 41.6,
    }),
    agent({
      id: 11,
      agentType: 'implementer',
      description: 'Build the shell',
      state: 'needs_you',
      costUsd: 0.12,
      parent: 10,
    }),
    agent({ id: 12, kind: 'teammate' }),
  ],
  edges: [{ kind: 'dispatch', parent: 10, child: 11 }],
}

const unnamedGraph: LiveGraph = {
  sessionId: 's-3',
  live: true,
  agents: [agent({ id: 20, agentId: null, kind: 'orchestrator', name: 'app-main' })],
  edges: [],
}

let storeChanged: () => void
const unlisten = vi.fn()

function deferred<T>() {
  let resolve!: (value: T) => void
  const promise = new Promise<T>((r) => (resolve = r))
  return { promise, resolve }
}

beforeEach(() => {
  localStorage.clear()
  resetGroups()
  unlisten.mockReset()
  statusMock.mockReset().mockResolvedValue('live')
  workstreamsMock.mockReset().mockResolvedValue([finished, shell, unnamed])
  liveGraphMock
    .mockReset()
    .mockImplementation((id) =>
      Promise.resolve(id === 2 ? shellGraph : id === 3 ? unnamedGraph : null),
    )
  onStoreChangedMock.mockReset().mockImplementation((callback) => {
    storeChanged = callback
    return Promise.resolve(unlisten)
  })
})

afterEach(() => {
  vi.useRealTimers()
})

const tab = (name: RegExp) => screen.getByRole('tab', { name })
const selectTab = (name: RegExp) => fireEvent.mouseDown(tab(name), { button: 0 })

describe('loading states', () => {
  it('shows Indexing… while the initial scan runs', async () => {
    statusMock.mockResolvedValue('indexing')
    render(<App />)
    expect(await screen.findByText('Indexing…')).toBeInTheDocument()
    expect(screen.queryByRole('img', { name: /indexing/i })).not.toBeInTheDocument()
    expect(screen.queryByRole('status', { name: /indexing/i })).not.toBeInTheDocument()
  })

  it('titles the window strate', async () => {
    render(<App />)
    expect(screen.getByRole('heading', { level: 1, name: 'strate' })).toBeInTheDocument()
    await act(async () => {})
  })

  it('shows an empty state with no workstreams', async () => {
    workstreamsMock.mockResolvedValue([])
    render(<App />)
    expect(await screen.findByText('No live sessions.')).toBeInTheDocument()
    expect(screen.queryByRole('tab')).not.toBeInTheDocument()
  })

  it('shows the same empty state when every session has stopped', async () => {
    workstreamsMock.mockResolvedValue([finished])
    render(<App />)
    expect(await screen.findByText('No live sessions.')).toBeInTheDocument()
    expect(screen.queryByRole('tab')).not.toBeInTheDocument()
    expect(liveGraphMock).not.toHaveBeenCalled()
  })

  it('reports a store error', async () => {
    workstreamsMock.mockRejectedValue('no such table: workstreams')
    render(<App />)
    expect(
      await screen.findByText("Couldn't read the store: no such table: workstreams"),
    ).toBeInTheDocument()
  })
})

describe('orchestrator tabs', () => {
  it('lists one tab per live session with its total $ and a needs-you badge', async () => {
    render(<App />)
    await screen.findByRole('tab', { name: /strate-17/ })
    expect(screen.getAllByRole('tab')).toHaveLength(2)
    expect(within(tab(/strate-17/)).getByText('$3.10')).toBeInTheDocument()
    expect(within(tab(/strate-17/)).getByText('needs you')).toBeInTheDocument()
    expect(within(tab(/app@main/)).queryByText('needs you')).not.toBeInTheDocument()
  })

  it('leaves sessions that are no longer live out of the strip', async () => {
    render(<App />)
    await screen.findByRole('tab', { name: /strate-17/ })
    expect(screen.queryByRole('tab', { name: /docs-cleanup/ })).not.toBeInTheDocument()
  })

  it('labels each tab with its state', async () => {
    render(<App />)
    await screen.findByRole('tab', { name: /strate-17/ })
    expect(within(tab(/strate-17/)).getByRole('img', { name: 'needs you' })).toBeInTheDocument()
    expect(within(tab(/app@main/)).getByRole('img', { name: 'working' })).toBeInTheDocument()
  })

  it('says needs you once in the tab name', async () => {
    render(<App />)
    await screen.findByRole('tab', { name: /strate-17/ })
    expect(tab(/strate-17/)).toHaveAccessibleName('needs you strate-17 $3.10')
  })

  it('flags names that fell back to cwd and branch', async () => {
    render(<App />)
    await screen.findByRole('tab', { name: /app@main/ })
    expect(within(tab(/app@main/)).getByTitle(/no \/rename yet/)).toHaveTextContent(
      '/work/app@main',
    )
    expect(tab(/app@main/)).toHaveAccessibleName(/\/work\/app@main \(unnamed\)/)
    expect(within(tab(/strate-17/)).queryByTitle(/no \/rename yet/)).not.toBeInTheDocument()
    expect(tab(/strate-17/)).not.toHaveAccessibleName(/unnamed/)
  })

  it('opens on the first live session', async () => {
    render(<App />)
    await screen.findByRole('tab', { name: /strate-17/ })
    expect(screen.getByRole('tab', { selected: true })).toHaveAccessibleName(/strate-17/)
    expect(liveGraphMock).toHaveBeenCalledWith(2)
  })

  it('opens on the first tab', async () => {
    workstreamsMock.mockResolvedValue([unnamed, shell])
    render(<App />)
    await screen.findByRole('tab', { name: /app@main/ })
    expect(screen.getByRole('tab', { selected: true })).toHaveAccessibleName(/app@main/)
    expect(liveGraphMock).toHaveBeenCalledWith(3)
  })

  it('switches the canvas to the picked tab', async () => {
    render(<App />)
    await screen.findByRole('button', { name: /implementer/ })
    selectTab(/app@main/)
    expect(await screen.findByRole('button', { name: /app-main/ })).toBeInTheDocument()
    expect(screen.queryByRole('button', { name: /implementer/ })).not.toBeInTheDocument()
    expect(screen.getByRole('heading', { name: '/work/app@main' })).toBeInTheDocument()
  })

  it('drops a graph that arrives after its tab was left', async () => {
    const late = deferred<LiveGraph | null>()
    liveGraphMock.mockImplementation((id) =>
      id === 2 ? late.promise : Promise.resolve(unnamedGraph),
    )
    render(<App />)
    await screen.findByRole('tab', { name: /strate-17/ })
    selectTab(/app@main/)
    await screen.findByRole('button', { name: /app-main/ })
    await act(async () => late.resolve(shellGraph))
    expect(screen.queryByRole('button', { name: /implementer/ })).not.toBeInTheDocument()
  })

  it('reports a live-graph error in the canvas', async () => {
    liveGraphMock.mockRejectedValue('database is locked')
    render(<App />)
    expect(
      await screen.findByText("Couldn't read the store: database is locked"),
    ).toBeInTheDocument()
  })
})

describe('tab strip', () => {
  it('scrolls horizontally on a vertical wheel and leaves horizontal deltas alone', async () => {
    render(<App />)
    await screen.findByRole('tab', { name: /strate-17/ })
    const strip = screen.getByRole('tablist', { name: 'Orchestrators' })
    strip.scrollLeft = 10
    fireEvent.wheel(strip, { deltaY: 40 })
    expect(strip.scrollLeft).toBe(50)
    fireEvent.wheel(strip, { deltaX: 30, deltaY: 5 })
    expect(strip.scrollLeft).toBe(50)
  })

  it('scrolls the selected tab into view when it changes', async () => {
    const scrollIntoView = vi.fn(function (this: Element) {})
    Element.prototype.scrollIntoView = scrollIntoView
    render(<App />)
    await screen.findByRole('tab', { name: /strate-17/ })
    expect(scrollIntoView.mock.contexts.at(-1)).toBe(tab(/strate-17/))
    expect(scrollIntoView).toHaveBeenLastCalledWith({ block: 'nearest', inline: 'nearest' })
    selectTab(/app@main/)
    await screen.findByRole('button', { name: /app-main/ })
    expect(scrollIntoView.mock.contexts.at(-1)).toBe(tab(/app@main/))
  })
})

describe('refetching', () => {
  it('lands a slow read even when the store keeps changing, then reruns once', async () => {
    const first = deferred<Workstream[]>()
    const second = deferred<Workstream[]>()
    workstreamsMock.mockReturnValueOnce(first.promise).mockReturnValueOnce(second.promise)
    render(<App />)
    await act(async () => {})
    act(() => storeChanged())
    act(() => storeChanged())
    act(() => storeChanged())
    expect(workstreamsMock).toHaveBeenCalledOnce()
    await act(async () => first.resolve([shell]))
    expect(screen.getByRole('tab', { name: /strate-17/ })).toBeInTheDocument()
    expect(workstreamsMock).toHaveBeenCalledTimes(2)
    await act(async () => second.resolve([shell, unnamed]))
    expect(screen.getByRole('tab', { name: /app@main/ })).toBeInTheDocument()
    expect(workstreamsMock).toHaveBeenCalledTimes(2)
  })

  it('refetches on store-changed and keeps the picked tab', async () => {
    render(<App />)
    await screen.findByRole('tab', { name: /strate-17/ })
    selectTab(/app@main/)
    await screen.findByRole('button', { name: /app-main/ })
    workstreamsMock.mockResolvedValue([finished, shell, { ...unnamed, name: '/work/app@next' }])
    act(() => storeChanged())
    expect(await screen.findByRole('tab', { name: /app@next/ })).toHaveAttribute(
      'aria-selected',
      'true',
    )
    expect(liveGraphMock).toHaveBeenLastCalledWith(3)
    expect(statusMock).toHaveBeenCalledTimes(2)
  })

  it('drops the picked tab when its session stops being live', async () => {
    render(<App />)
    await screen.findByRole('tab', { name: /strate-17/ })
    selectTab(/app@main/)
    await screen.findByRole('button', { name: /app-main/ })
    workstreamsMock.mockResolvedValue([finished, shell, { ...unnamed, live: false, state: 'done' }])
    act(() => storeChanged())
    await screen.findByRole('button', { name: /implementer/ })
    expect(screen.queryByRole('tab', { name: /app@main/ })).not.toBeInTheDocument()
    expect(screen.getByRole('tab', { selected: true })).toHaveAccessibleName(/strate-17/)
  })

  it('falls back to the first tab when the picked one goes away, and forgets the pick', async () => {
    render(<App />)
    await screen.findByRole('tab', { name: /strate-17/ })
    selectTab(/app@main/)
    fireEvent.click(await screen.findByRole('button', { name: /app-main/ }))
    workstreamsMock.mockResolvedValue([finished, shell])
    act(() => storeChanged())
    await screen.findByRole('button', { name: /implementer/ })
    expect(screen.getByRole('tab', { selected: true })).toHaveAccessibleName(/strate-17/)

    workstreamsMock.mockResolvedValue([finished, shell, unnamed])
    act(() => storeChanged())
    await screen.findByRole('tab', { name: /app@main/ })
    expect(screen.getByRole('tab', { selected: true })).toHaveAccessibleName(/strate-17/)
    selectTab(/app@main/)
    await screen.findByRole('button', { name: /app-main/ })
    const console = screen.getByRole('complementary', { name: 'Console' })
    expect(within(console).getByRole('heading', { name: 'Console' })).toBeInTheDocument()
  })

  it('stops listening on unmount', async () => {
    const { unmount } = render(<App />)
    await screen.findByRole('tab', { name: /strate-17/ })
    unmount()
    expect(unlisten).toHaveBeenCalledOnce()
  })

  it('stops listening when unmounted before the listener is ready', async () => {
    const ready = deferred<() => void>()
    onStoreChangedMock.mockReturnValue(ready.promise)
    const { unmount } = render(<App />)
    unmount()
    await act(async () => ready.resolve(unlisten))
    expect(unlisten).toHaveBeenCalledOnce()
  })
})

describe('session line', () => {
  it('shows running time, session $, total $ and needs-you, ticking on the minute', async () => {
    vi.useFakeTimers({ toFake: ['Date', 'setTimeout', 'clearTimeout'] })
    vi.setSystemTime(new Date('2026-10-12T11:02:30Z'))
    render(<App />)
    await act(() => vi.advanceTimersByTimeAsync(0))
    const line = (time: string) =>
      screen.queryByText(`running ${time} · $0.42 this session · $3.10 total · 2 needs you`)
    expect(line('1h 2m')).toBeInTheDocument()
    await act(() => vi.advanceTimersByTimeAsync(29_999))
    expect(line('1h 2m')).toBeInTheDocument()
    await act(() => vi.advanceTimersByTimeAsync(1))
    expect(line('1h 3m')).toBeInTheDocument()
    await act(() => vi.advanceTimersByTimeAsync(60_000))
    expect(line('1h 4m')).toBeInTheDocument()
    expect(workstreamsMock).toHaveBeenCalledOnce()
    expect(liveGraphMock).toHaveBeenCalledOnce()
  })
})

describe('canvas', () => {
  it('draws the live graph in tiers, orchestrator on top', async () => {
    render(<App />)
    const orchestrator = await screen.findByRole('button', { name: /strate-17/ })
    const y = (id: number) => Number(screen.getByTestId(`node-${id}`).dataset.y)
    expect(y(10)).toBeLessThan(y(11))
    expect(screen.getByTestId('edge-10-11')).toBeInTheDocument()
    expect(orchestrator).toHaveTextContent('claude-opus-5-5')
    expect(orchestrator).toHaveTextContent('42%')
    const worker = screen.getByRole('button', { name: /implementer/ })
    expect(worker).toHaveTextContent('Build the shell')
    expect(worker).toHaveTextContent('needs you')
    expect(screen.getByRole('button', { name: /teammate/ })).toBeInTheDocument()
  })
})

describe('console', () => {
  it('opens on node select and closes on ×', async () => {
    render(<App />)
    fireEvent.click(await screen.findByRole('button', { name: /implementer/ }))
    const console = screen.getByRole('complementary', { name: 'Console' })
    expect(within(console).getByRole('heading', { name: 'implementer' })).toBeInTheDocument()
    expect(within(console).getByText('The console arrives in #19.')).toBeInTheDocument()
    expect(screen.getByRole('button', { name: /implementer/ })).toHaveAttribute(
      'aria-pressed',
      'true',
    )
    fireEvent.click(within(console).getByRole('button', { name: 'Close console' }))
    expect(screen.queryByRole('complementary', { name: 'Console' })).not.toBeInTheDocument()
    const card = screen.getByRole('button', { name: /implementer/ })
    expect(card).toHaveAttribute('aria-pressed', 'false')
    expect(card).toHaveFocus()
  })

  it('returns focus to the canvas on × with no node selected', async () => {
    render(<App />)
    fireEvent.click(await screen.findByRole('button', { name: /implementer/ }))
    selectTab(/app@main/)
    await screen.findByRole('button', { name: /app-main/ })
    fireEvent.click(screen.getByRole('button', { name: 'Close console' }))
    expect(screen.getByRole('main')).toHaveFocus()
  })

  it('drops the node selection when dragged shut', async () => {
    render(<App />)
    fireEvent.click(await screen.findByRole('button', { name: /implementer/ }))
    act(() => dragTo('strate-columns', { harness: 0, canvas: 100, console: 0 }))
    expect(screen.queryByRole('complementary', { name: 'Console' })).not.toBeInTheDocument()
    expect(screen.getByRole('button', { name: /implementer/ })).toHaveAttribute(
      'aria-pressed',
      'false',
    )
    act(() => dragTo('strate-columns', { harness: 0, canvas: 70, console: 30 }))
    const console = screen.getByRole('complementary', { name: 'Console' })
    expect(within(console).getByRole('heading', { name: 'Console' })).toBeInTheDocument()
  })

  it('switches nodes without closing', async () => {
    render(<App />)
    fireEvent.click(await screen.findByRole('button', { name: /implementer/ }))
    fireEvent.click(screen.getByRole('button', { name: /teammate/ }))
    const console = screen.getByRole('complementary', { name: 'Console' })
    expect(within(console).getByRole('heading', { name: 'teammate' })).toBeInTheDocument()
  })

  it('has no top-bar button', async () => {
    render(<App />)
    await screen.findByRole('tab', { name: /strate-17/ })
    expect(screen.queryByRole('button', { name: /console/i })).not.toBeInTheDocument()
  })

  it('drops the node selection when the tab changes', async () => {
    render(<App />)
    fireEvent.click(await screen.findByRole('button', { name: /implementer/ }))
    selectTab(/app@main/)
    await screen.findByRole('button', { name: /app-main/ })
    const console = screen.getByRole('complementary', { name: 'Console' })
    expect(within(console).getByRole('heading', { name: 'Console' })).toBeInTheDocument()
  })
})

describe('panels', () => {
  const pressed = (name: string) =>
    screen.getByRole('button', { name }).getAttribute('aria-pressed')
  const saved = () => JSON.parse(localStorage.getItem(PREFS_KEY) ?? 'null')

  it('start closed on first launch', async () => {
    render(<App />)
    await screen.findByRole('tab', { name: /strate-17/ })
    expect(pressed('Harness')).toBe('false')
    expect(pressed('Terminal')).toBe('false')
    expect(screen.queryByRole('complementary', { name: 'Harness' })).not.toBeInTheDocument()
    expect(screen.queryByRole('region', { name: 'Terminal' })).not.toBeInTheDocument()
  })

  it('lay out Harness left, canvas centre, console right, terminal below', async () => {
    render(<App />)
    await screen.findByRole('tab', { name: /strate-17/ })
    const rows = screen.getByTestId('group-strate-rows')
    expect(rows).toHaveAttribute('data-orientation', 'vertical')
    const order = within(rows)
      .getAllByTestId(/^panel-/)
      .map((p) => p.dataset.testid)
    expect(order).toEqual([
      'panel-main',
      'panel-harness',
      'panel-canvas',
      'panel-console',
      'panel-terminal',
    ])
    expect(screen.getByTestId('group-strate-columns')).toHaveAttribute(
      'data-orientation',
      'horizontal',
    )
  })

  it('open from the toggle buttons with a one-line placeholder', async () => {
    render(<App />)
    await screen.findByRole('tab', { name: /strate-17/ })
    fireEvent.click(screen.getByRole('button', { name: 'Harness' }))
    fireEvent.click(screen.getByRole('button', { name: 'Terminal' }))
    expect(pressed('Harness')).toBe('true')
    expect(pressed('Terminal')).toBe('true')
    expect(
      within(screen.getByRole('complementary', { name: 'Harness' })).getByText(
        'Harness arrives in #23.',
      ),
    ).toBeInTheDocument()
    expect(
      within(screen.getByRole('region', { name: 'Terminal' })).getByText(
        'Terminal arrives in Epic E.',
      ),
    ).toBeInTheDocument()
    fireEvent.click(screen.getByRole('button', { name: 'Harness' }))
    expect(pressed('Harness')).toBe('false')
    expect(screen.queryByRole('complementary', { name: 'Harness' })).not.toBeInTheDocument()
  })

  it('save sizes and open state and restore them on the next launch', async () => {
    const first = render(<App />)
    await screen.findByRole('tab', { name: /strate-17/ })
    fireEvent.click(screen.getByRole('button', { name: 'Harness' }))
    act(() => dragTo('strate-columns', { harness: 22, canvas: 78, console: 0 }))
    fireEvent.click(screen.getByRole('button', { name: 'Terminal' }))
    expect(saved()).toEqual({
      harness: { open: true, size: 22 },
      console: { open: false, size: 32 },
      terminal: { open: true, size: 35 },
    })
    first.unmount()
    cleanup()
    resetGroups()

    render(<App />)
    await screen.findByRole('tab', { name: /strate-17/ })
    expect(defaultLayoutOf('strate-columns')).toEqual({ harness: 22, canvas: 78, console: 0 })
    expect(defaultLayoutOf('strate-rows')).toEqual({ main: 65, terminal: 35 })
    expect(pressed('Harness')).toBe('true')
    expect(pressed('Terminal')).toBe('true')
    expect(screen.getByRole('complementary', { name: 'Harness' })).toBeInTheDocument()
  })

  it('fall back to the defaults when the saved sizes cannot fit', async () => {
    localStorage.setItem(
      PREFS_KEY,
      JSON.stringify({
        harness: { open: true, size: 50 },
        console: { open: true, size: 45 },
        terminal: { open: true, size: 35 },
      }),
    )
    render(<App />)
    await screen.findByRole('tab', { name: /strate-17/ })
    expect(defaultLayoutOf('strate-columns')).toEqual({ harness: 0, canvas: 100, console: 0 })
    expect(pressed('Harness')).toBe('false')
  })

  it('reopen at the size they had before closing', async () => {
    render(<App />)
    await screen.findByRole('tab', { name: /strate-17/ })
    fireEvent.click(screen.getByRole('button', { name: 'Terminal' }))
    act(() => dragTo('strate-rows', { main: 75, terminal: 25 }))
    fireEvent.click(screen.getByRole('button', { name: 'Terminal' }))
    fireEvent.click(screen.getByRole('button', { name: 'Terminal' }))
    expect(saved().terminal).toEqual({ open: true, size: 25 })
  })

  it('close when dragged shut', async () => {
    render(<App />)
    await screen.findByRole('tab', { name: /strate-17/ })
    fireEvent.click(screen.getByRole('button', { name: 'Harness' }))
    act(() => dragTo('strate-columns', { harness: 0, canvas: 100, console: 0 }))
    expect(pressed('Harness')).toBe('false')
    expect(saved().harness).toEqual({ open: false, size: 30 })
  })
})

describe('audit view', () => {
  const openAudit = () => fireEvent.click(screen.getByRole('button', { name: 'Audit' }))
  const back = () => fireEvent.click(screen.getByRole('button', { name: 'Live' }))

  it('starts on the live view with the Audit view hidden', async () => {
    render(<App />)
    await screen.findByRole('tab', { name: /strate-17/ })
    expect(screen.queryByRole('heading', { name: 'Audit' })).not.toBeInTheDocument()
  })

  it('opens full-screen from the header, hiding the live view, and takes focus', async () => {
    render(<App />)
    await screen.findByRole('tab', { name: /strate-17/ })
    openAudit()
    const heading = screen.getByRole('heading', { name: 'Audit' })
    expect(heading).toHaveFocus()
    expect(screen.queryByRole('tablist')).not.toBeInTheDocument()
    expect(screen.queryByRole('button', { name: 'Audit' })).not.toBeInTheDocument()
  })

  it('goes back with ‹ Live, returning focus to the Audit button', async () => {
    render(<App />)
    await screen.findByRole('tab', { name: /strate-17/ })
    openAudit()
    back()
    expect(screen.queryByRole('heading', { name: 'Audit' })).not.toBeInTheDocument()
    expect(screen.getByRole('tablist')).toBeInTheDocument()
    expect(screen.getByRole('button', { name: 'Audit' })).toHaveFocus()
  })

  it('keeps the picked tab and panel prefs across a visit', async () => {
    render(<App />)
    await screen.findByRole('tab', { name: /strate-17/ })
    selectTab(/app@main/)
    fireEvent.click(screen.getByRole('button', { name: 'Harness' }))
    act(() => dragTo('strate-columns', { harness: 22, canvas: 78, console: 0 }))
    const before = localStorage.getItem(PREFS_KEY)
    openAudit()
    back()
    expect(tab(/app@main/)).toHaveAttribute('aria-selected', 'true')
    expect(screen.getByRole('button', { name: 'Harness' })).toHaveAttribute('aria-pressed', 'true')
    expect(localStorage.getItem(PREFS_KEY)).toBe(before)
  })
})
