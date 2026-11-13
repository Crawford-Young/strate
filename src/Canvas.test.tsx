import { Tabs } from '@crawfordyoung/ui'
import { act, fireEvent, render, screen, within } from '@testing-library/react'
import type { ElkNode } from 'elkjs/lib/elk-api'
import ELK from 'elkjs/lib/elk.bundled.js'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { centerCalls, fitView, flowProps, resetFlow, zoomCalls } from '../tests/xyflow-mock'
import type { GraphAgent, GraphEdge, LiveGraph, Workstream } from './api'
import { Canvas, REFIT_MS } from './Canvas'
import { NODE_HEIGHT, NODE_WIDTH } from './graphLayout'

const layout = vi.hoisted(() => vi.fn<(graph: ElkNode) => Promise<ElkNode>>())

vi.mock('./elk', () => ({ elk: () => ({ layout }) }))

vi.mock('@xyflow/react', async (importOriginal) => ({
  ...(await importOriginal<typeof import('@xyflow/react')>()),
  ...(await import('../tests/xyflow-mock')),
}))

const real = new ELK()

function deferred<T>() {
  let resolve!: (value: T) => void
  const promise = new Promise<T>((r) => (resolve = r))
  return { promise, resolve }
}

const ws = (id: number): Workstream => ({
  id,
  name: `ws-${id}`,
  fallback: false,
  costUsd: null,
  unpriced: 0,
  state: 'working',
  needsYou: 0,
  sessionId: `s-${id}`,
  live: true,
  startedAt: null,
  director: null,
})

function agent(over: Partial<GraphAgent> & Pick<GraphAgent, 'id'>): GraphAgent {
  return {
    agentId: `a-${over.id}`,
    kind: 'subagent',
    name: `agent-${over.id}`,
    agentType: null,
    description: null,
    model: null,
    state: 'working',
    costUsd: null,
    contextTokens: null,
    contextPct: null,
    parent: null,
    sessionId: 's-1',
    role: null,
    lane: 'subagent',
    charge: null,
    chargeKey: null,
    chargeSource: 'unlinked',
    links: [],
    ...over,
  }
}

const dispatch = (parent: number, child: number): GraphEdge => ({ kind: 'dispatch', parent, child })

// Orchestrator 1 → subagent 2 and teammate 3; subagent 2 → its dispatch 4.
const tiered: LiveGraph = {
  sessionId: 's-1',
  live: true,
  sessionCostUsd: null,
  runMs: null,
  agents: [
    agent({ id: 1, kind: 'orchestrator', agentId: null, name: 'lead' }),
    agent({ id: 2, parent: 1, name: 'builder' }),
    agent({ id: 3, kind: 'teammate', parent: 1, name: 'mate' }),
    agent({ id: 4, parent: 2, name: 'helper' }),
  ],
  edges: [dispatch(1, 2), { kind: 'teammate', parent: 1, child: 3 }, dispatch(2, 4)],
}

interface Props {
  graph?: LiveGraph | null
  active?: Workstream
  selected?: number | null
  onSelect?: (id: number) => void
}

function view({ graph = tiered, active = ws(1), selected = null, onSelect = () => {} }: Props) {
  return (
    <Tabs value={String(active.id)}>
      <Canvas
        error={null}
        status="live"
        active={active}
        graph={graph}
        selected={selected}
        onSelect={onSelect}
      />
    </Tabs>
  )
}

const node = (id: number) => screen.getByTestId(`node-${id}`)
const y = (id: number) => Number(node(id).dataset.y)
const pos = (id: number) => ({ x: node(id).dataset.x, y: node(id).dataset.y })
const card = (name: string) => screen.getByRole('button', { name: new RegExp(name) })
/** A card's charge chip: the last thing on its meta row. */
const chip = (card: HTMLElement) =>
  card.querySelector('[data-slot="meta"] [data-slot="charge"]') as HTMLElement

beforeEach(() => {
  resetFlow()
  layout.mockReset().mockImplementation((g) => real.layout(g))
})

describe('live graph', () => {
  it('draws the orchestrator on top, its agents below, their dispatches below those', async () => {
    render(view({}))
    await screen.findByTestId('flow')
    expect(y(1)).toBeLessThan(y(2))
    expect(y(3)).toBe(y(2))
    expect(y(2)).toBeLessThan(y(4))
    expect(within(node(1)).getByRole('button', { name: /lead/ })).toBeInTheDocument()
  })

  it('draws dispatch edges solid and teammate edges dashed, in the muted colour', async () => {
    render(view({}))
    await screen.findByTestId('flow')
    const solid = screen.getByTestId('edge-1-2')
    const dashed = screen.getByTestId('edge-1-3')
    expect(solid.dataset.dash).toBeUndefined()
    expect(dashed.dataset.dash).toBeTruthy()
    expect(solid.dataset.stroke).toBe('rgb(var(--muted-foreground) / 0.5)')
    expect(screen.getByTestId('edge-2-4')).toBeInTheDocument()
  })

  it('draws a directs edge solid and brighter than dispatch, with units under the director', async () => {
    const directed: LiveGraph = {
      ...tiered,
      agents: [
        ...tiered.agents,
        agent({ id: 5, kind: 'orchestrator', agentId: null, name: 'unit', parent: 1 }),
        agent({ id: 6, parent: 5, name: 'unit-helper' }),
      ],
      edges: [...tiered.edges, { kind: 'directs', parent: 1, child: 5 }, dispatch(5, 6)],
    }
    render(view({ graph: directed }))
    await screen.findByTestId('flow')
    const directs = screen.getByTestId('edge-1-5')
    expect(directs.dataset.dash).toBeUndefined()
    expect(directs.dataset.stroke).toBe('rgb(var(--muted-foreground))')
    expect(y(1)).toBeLessThan(y(5))
    expect(y(5)).toBeLessThan(y(6))
    expect(screen.getByTestId('edge-1-2').dataset.stroke).toBe('rgb(var(--muted-foreground) / 0.5)')
  })

  it('draws an edge kind it does not know in the default style', async () => {
    const launched: LiveGraph = {
      ...tiered,
      agents: [...tiered.agents, agent({ id: 5, kind: 'orchestrator', parent: 1, lane: 'cloud' })],
      edges: [...tiered.edges, { kind: 'launch', parent: 1, child: 5 }],
    }
    render(view({ graph: launched }))
    await screen.findByTestId('flow')
    const edge = screen.getByTestId('edge-1-5')
    expect(edge.dataset.dash).toBeUndefined()
    expect(edge.dataset.stroke).toBe('rgb(var(--muted-foreground) / 0.5)')
    expect(y(1)).toBeLessThan(y(5))
  })

  it('pins nodes in place: no dragging, no connecting, handles top and bottom', async () => {
    render(view({}))
    await screen.findByTestId('flow')
    expect(flowProps().nodesDraggable).toBe(false)
    expect(flowProps().nodesConnectable).toBe(false)
    const handles = within(node(2))
    expect(handles.getByTestId('handle-target')).toHaveAttribute('data-position', 'top')
    expect(handles.getByTestId('handle-source')).toHaveAttribute('data-position', 'bottom')
    expect(handles.getByTestId('handle-source')).toHaveAttribute('data-connectable', 'false')
  })

  it('draws dark, fits the first layout, and puts the dots on the panning surface', async () => {
    render(view({}))
    await screen.findByTestId('flow')
    expect(flowProps().colorMode).toBe('dark')
    expect(flowProps().fitView).toBe(true)
    // Space must stay a normal key everywhere else in the app.
    expect(flowProps().panActivationKeyCode).toBeNull()
    const dots = screen.getByTestId('background')
    expect(dots).toHaveAttribute('data-variant', 'dots')
    expect(dots).toHaveAttribute('data-gap', '20')
    expect(dots).toHaveAttribute('data-color', 'rgb(63 63 70 / 0.5)')
  })

  it('zooms out and in from the buttons', async () => {
    render(view({}))
    await screen.findByTestId('flow')
    fireEvent.click(screen.getByRole('button', { name: 'Zoom out' }))
    fireEvent.click(screen.getByRole('button', { name: 'Zoom in' }))
    expect(zoomCalls).toEqual(['out', 'in'])
  })
})

describe('node cards', () => {
  it('selects the agent on click and rings the selected one', async () => {
    const onSelect = vi.fn()
    render(view({ selected: 3, onSelect }))
    await screen.findByTestId('flow')
    fireEvent.click(card('builder'))
    expect(onSelect).toHaveBeenCalledWith(2)
    expect(card('mate')).toHaveAttribute('aria-pressed', 'true')
    expect(card('builder')).toHaveAttribute('aria-pressed', 'false')
    expect(card('builder')).toHaveAttribute('data-agent', '2')
  })

  it('shows role, charge chip, lane, state, $ and context % on a node', async () => {
    const graph: LiveGraph = {
      ...tiered,
      agents: [
        agent({
          id: 1,
          kind: 'orchestrator',
          agentId: null,
          name: 'director_topic_company-model',
          role: 'director',
          lane: 'interactive',
          charge: { kind: 'topic', repo: null, number: null, topic: 'company-model' },
          chargeKey: 'topic_company-model',
          chargeSource: 'name',
          state: 'needs_you',
          costUsd: 1.234,
          contextPct: 41.6,
        }),
      ],
      edges: [],
    }
    render(view({ graph }))
    await screen.findByTestId('flow')
    const node = card('director')
    // The role names the node; the charge has its own chip, so the full name isn't repeated.
    expect(node).not.toHaveTextContent('director_topic_company-model')
    expect(chip(node)).toHaveTextContent(/^topic_company-model/)
    expect(node).toHaveTextContent('interactive')
    expect(node).toHaveTextContent('needs you')
    expect(node).toHaveTextContent('$1.23')
    expect(node).toHaveTextContent('42%')
  })

  it('puts the short lane first and the chip last, the chip shrinking to fit the row', async () => {
    const graph: LiveGraph = {
      ...tiered,
      agents: [
        agent({
          id: 1,
          name: 'builder',
          lane: 'background-session',
          chargeKey: 'issue_strate-109',
          chargeSource: 'branch',
        }),
      ],
      edges: [],
    }
    render(view({ graph }))
    await screen.findByTestId('flow')
    const node = card('builder')
    const meta = node.querySelector('[data-slot="meta"]') as HTMLElement
    // The chip also speaks its source, hidden from sight.
    expect(meta).toHaveTextContent(/^background\s*issue_strate-109/)
    expect(meta.lastElementChild).toBe(chip(node))
    // A long key ellipsizes inside its chip; the lane keeps its room.
    const key = within(node).getByText('issue_strate-109')
    expect(key).toHaveClass('min-w-0', 'truncate')
    expect(chip(node)).toHaveClass('min-w-0', 'max-w-full')
    expect(within(meta).getByText('background')).toHaveClass('shrink-0')
  })

  it("keeps the chip to the badge's own type, with no second weight stacked on it", async () => {
    render(view({ graph: { ...tiered, agents: [agent({ id: 1, name: 'loose' })], edges: [] } }))
    await screen.findByTestId('flow')
    const weights = [...chip(card('loose')).classList].filter((c) =>
      /^font-(thin|extralight|light|normal|medium|semibold|bold|extrabold|black)$/.test(c),
    )
    expect(weights).toHaveLength(1)
  })

  it('reads $, state, lane and charge as separate words', async () => {
    const graph: LiveGraph = {
      ...tiered,
      agents: [
        agent({
          id: 1,
          name: 'builder',
          state: 'needs_you',
          costUsd: 1.234,
          lane: 'subagent',
          chargeKey: 'issue_strate-109',
          chargeSource: 'branch',
          contextPct: 41.6,
        }),
      ],
      edges: [],
    }
    render(view({ graph }))
    await screen.findByTestId('flow')
    const node = card('builder')
    expect(node).toHaveAccessibleName(/\$1\.23 needs you/)
    expect(node).toHaveAccessibleName(/subagent issue_strate-109\s/)
  })

  it('names a node with no role by its name, then type, then kind', async () => {
    const graph: LiveGraph = {
      ...tiered,
      agents: [
        agent({ id: 1, name: 'researcher' }),
        agent({ id: 2, name: null, agentType: 'implementer' }),
        agent({ id: 3, name: null, kind: 'teammate' }),
      ],
      edges: [],
    }
    render(view({ graph }))
    await screen.findByTestId('flow')
    expect(card('researcher')).toHaveAttribute('data-agent', '1')
    expect(card('implementer')).toHaveAttribute('data-agent', '2')
    expect(card('teammate')).toHaveAttribute('data-agent', '3')
  })

  it('shows an Unlinked chip, and the $ with no priced request or gauge', async () => {
    render(view({ graph: { ...tiered, agents: [agent({ id: 1, name: 'loose' })], edges: [] } }))
    await screen.findByTestId('flow')
    expect(chip(card('loose'))).toHaveTextContent(/^unlinked/)
    expect(card('loose')).toHaveAccessibleName(/no charge/)
    expect(card('loose')).toHaveTextContent('$—')
    expect(within(card('loose')).queryByRole('meter')).not.toBeInTheDocument()
  })

  it('says where an inherited charge came from', async () => {
    const graph: LiveGraph = {
      ...tiered,
      agents: [agent({ id: 1, name: 'helper', chargeKey: 'pr_strate-12', chargeSource: 'parent' })],
      edges: [],
    }
    render(view({ graph }))
    await screen.findByTestId('flow')
    expect(chip(card('helper'))).toHaveTextContent('pr_strate-12')
    // On the card itself, so keyboard and screen-reader users get it too, not only a mouse hover.
    expect(card('helper')).toHaveAccessibleName(/charge from parent/)
    expect(chip(card('helper'))).toHaveAttribute('title', 'charge from parent')
  })

  it('names each lane, and shows one it does not know as it came', async () => {
    const graph: LiveGraph = {
      ...tiered,
      agents: [
        agent({ id: 1, name: 'worker', lane: 'subagent' }),
        agent({ id: 2, name: 'spawned', lane: 'background-session' }),
        agent({ id: 3, name: 'remote', lane: 'cloud' }),
      ],
      edges: [],
    }
    render(view({ graph }))
    await screen.findByTestId('flow')
    expect(card('worker')).toHaveTextContent('subagent')
    expect(card('spawned')).toHaveTextContent('background')
    expect(card('remote')).toHaveTextContent('cloud')
  })

  it("offers the description on hover and as the card's description, the source on the chip", async () => {
    const graph: LiveGraph = {
      ...tiered,
      agents: [
        agent({
          id: 1,
          name: 'builder',
          description: 'Build the shell',
          chargeKey: 'issue_strate-109',
          chargeSource: 'name',
        }),
        agent({ id: 2, name: 'quiet' }),
      ],
      edges: [],
    }
    render(view({ graph }))
    await screen.findByTestId('flow')
    expect(card('builder')).toHaveAttribute('title', 'Build the shell')
    expect(card('builder')).toHaveAccessibleDescription('Build the shell')
    expect(chip(card('builder'))).toHaveAttribute('title', 'charge from name')
    expect(card('quiet')).not.toHaveAttribute('title')
    expect(chip(card('quiet'))).toHaveAttribute('title', 'no charge')
  })

  it('colours every gauge against the fixed nudge 80% and hard stop 95%', async () => {
    const at = (id: number, contextPct: number) =>
      agent({ id, name: `at-${contextPct}`, contextPct })
    const graph: LiveGraph = {
      ...tiered,
      agents: [at(1, 79), at(2, 80), at(3, 94), at(4, 95), agent({ id: 5, name: 'unknown' })],
      edges: [],
    }
    render(view({ graph }))
    await screen.findByTestId('flow')
    const fill = (name: string) =>
      (within(card(name)).getByRole('meter').firstElementChild as Element).className
    expect(fill('at-79')).toMatch(/bg-success/)
    expect(fill('at-80')).toMatch(/bg-warning/)
    expect(fill('at-94')).toMatch(/bg-warning/)
    expect(fill('at-95')).toMatch(/bg-destructive/)
    expect(card('at-79')).toHaveTextContent('79%')
    expect(within(card('unknown')).queryByRole('meter')).not.toBeInTheDocument()
  })
})

describe('keyboard', () => {
  type Box = { left: number; top: number; right: number; bottom: number }
  // happy-dom has no layout. The flow sits at `flow` on screen (the mock maps
  // flow coordinates to the screen 1:1), and every card's own box claims to be
  // inside it, as it does mid-focus while the browser scrolls the flow to
  // reveal it, before the flow resets that scroll.
  function flowAt(flow: Box) {
    const inside = {
      left: flow.left + 1,
      top: flow.top + 1,
      right: flow.left + 2,
      bottom: flow.top + 2,
    }
    return vi.spyOn(HTMLElement.prototype, 'getBoundingClientRect').mockImplementation(function (
      this: HTMLElement,
    ) {
      const box = this.classList.contains('react-flow') ? flow : inside
      return { ...box, x: box.left, y: box.top, width: 0, height: 0 } as DOMRect
    })
  }

  it('leaves the cards as the only tab stops', async () => {
    render(view({}))
    await screen.findByTestId('flow')
    expect(flowProps().nodesFocusable).toBe(false)
    expect(card('builder').tabIndex).toBe(0)
  })

  it('centres a card that takes focus off-screen, at the current zoom', async () => {
    render(view({}))
    await screen.findByTestId('flow')
    const spy = flowAt({ left: 5000, top: 5000, right: 5800, bottom: 5600 })
    fireEvent.focus(card('builder'))
    spy.mockRestore()
    expect(centerCalls).toEqual([
      { x: Number(pos(2).x) + NODE_WIDTH / 2, y: Number(pos(2).y) + NODE_HEIGHT / 2, zoom: 1 },
    ])
  })

  it('leaves the view alone when the focused card is in view', async () => {
    render(view({}))
    await screen.findByTestId('flow')
    const spy = flowAt({ left: -10_000, top: -10_000, right: 10_000, bottom: 10_000 })
    fireEvent.focus(card('builder'))
    spy.mockRestore()
    expect(centerCalls).toEqual([])
  })
})

describe('measured nodes', () => {
  const allMeasured = () =>
    screen.getAllByTestId(/^node-\d+$/).every((n) => n.dataset.measured === 'true')

  it('keeps every node measured and its edges drawn across data and structural updates', async () => {
    const { rerender } = render(view({}))
    await screen.findByTestId('flow')
    expect(allMeasured()).toBe(true)
    const busy = { ...tiered, agents: tiered.agents.map((a) => ({ ...a, state: 'idle' as const })) }
    rerender(view({ graph: busy }))
    await act(async () => {})
    expect(allMeasured()).toBe(true)
    expect(screen.getByTestId('edge-1-2')).toBeInTheDocument()
    const grown: LiveGraph = {
      ...busy,
      agents: [...busy.agents, agent({ id: 5, parent: 1 })],
      edges: [...busy.edges, dispatch(1, 5)],
    }
    rerender(view({ graph: grown }))
    await screen.findByTestId('edge-1-5')
    expect(allMeasured()).toBe(true)
    expect(screen.getByTestId('edge-2-4')).toBeInTheDocument()
  })

  it('ignores node changes that carry no size', async () => {
    render(view({}))
    await screen.findByTestId('flow')
    act(() =>
      flowProps().onNodesChange?.([
        { id: '1', type: 'select', selected: true },
        { id: '2', type: 'dimensions' },
      ]),
    )
    expect(allMeasured()).toBe(true)
  })

  it('lets clicks reach the cards', async () => {
    render(view({}))
    await screen.findByTestId('flow')
    expect(node(2)).toHaveAttribute('data-pointer-events', 'all')
  })
})

describe('layout', () => {
  it('draws nothing graph-side while the first layout is in flight, header included', async () => {
    const pending = deferred<ElkNode>()
    layout.mockReturnValueOnce(pending.promise)
    render(view({}))
    expect(screen.getByRole('heading', { name: 'ws-1' })).toBeInTheDocument()
    await act(async () => {})
    expect(screen.queryByTestId('flow')).not.toBeInTheDocument()
    await act(async () => pending.resolve(await real.layout(layout.mock.calls[0]?.[0] as ElkNode)))
    expect(screen.getByTestId('flow')).toBeInTheDocument()
  })

  it('updates state, cost and context without laying out again', async () => {
    const { rerender } = render(view({}))
    await screen.findByTestId('flow')
    const before = [1, 2, 3, 4].map(pos)
    const busy = {
      ...tiered,
      agents: tiered.agents.map((a) => ({ ...a, state: 'done' as const, contextPct: 50 })),
    }
    rerender(view({ graph: busy }))
    await act(async () => {})
    expect(layout).toHaveBeenCalledOnce()
    expect([1, 2, 3, 4].map(pos)).toEqual(before)
    expect(card('builder')).toHaveTextContent('done')
  })

  it('keeps every placed node where it was when an agent joins', async () => {
    const { rerender } = render(view({}))
    await screen.findByTestId('flow')
    const before = [1, 2, 3, 4].map(pos)
    const grown: LiveGraph = {
      ...tiered,
      agents: [...tiered.agents, agent({ id: 5, parent: 1, name: 'newcomer' })],
      edges: [...tiered.edges, dispatch(1, 5)],
    }
    rerender(view({ graph: grown }))
    // Until the new layout lands, the placed nodes stay and the newcomer waits.
    expect(screen.queryByTestId('node-5')).not.toBeInTheDocument()
    expect(screen.queryByTestId('edge-1-5')).not.toBeInTheDocument()
    await screen.findByTestId('node-5')
    expect(layout).toHaveBeenCalledTimes(2)
    expect([1, 2, 3, 4].map(pos)).toEqual(before)
    expect(y(5)).toBe(y(2))
    expect(screen.getByTestId('edge-1-5')).toBeInTheDocument()
  })

  it('lays a new tab out fresh, and drops a layout that lands after its tab was left', async () => {
    const stale = deferred<ElkNode>()
    layout.mockReturnValueOnce(stale.promise)
    const { rerender } = render(view({}))
    await act(async () => {})
    const other: LiveGraph = { ...tiered, sessionId: 's-2' }
    // Same agent ids, another tab: ELK's positions are used, none pinned.
    layout.mockImplementationOnce(async (g) => ({
      ...g,
      children: g.children?.map((c, i) => ({ ...c, x: 1000 + i, y: 500 })),
    }))
    rerender(view({ graph: other, active: ws(2) }))
    await screen.findByTestId('flow')
    await act(async () => stale.resolve({ id: 'root', children: [{ id: '1', x: 0, y: 0 }] }))
    expect(pos(1)).toEqual({ x: '1000', y: '500' })
    expect(screen.getByRole('heading', { name: 'ws-2' })).toBeInTheDocument()
  })

  it('says so when ELK fails, and retries on the next structural change', async () => {
    layout.mockRejectedValueOnce(new Error('worker crashed'))
    const { rerender } = render(view({}))
    expect(
      await screen.findByText("Couldn't lay out the graph: worker crashed"),
    ).toBeInTheDocument()
    expect(screen.queryByTestId('flow')).not.toBeInTheDocument()
    expect(screen.getByRole('heading', { name: 'ws-1' })).toBeInTheDocument()
    const grown: LiveGraph = {
      ...tiered,
      agents: [...tiered.agents, agent({ id: 5, parent: 1 })],
      edges: [...tiered.edges, dispatch(1, 5)],
    }
    rerender(view({ graph: grown }))
    await screen.findByTestId('flow')
    expect(screen.getByTestId('node-5')).toBeInTheDocument()
    expect(screen.queryByText(/Couldn't lay out/)).not.toBeInTheDocument()
  })

  it('keeps placed nodes pinned across a failed relayout', async () => {
    const { rerender } = render(view({}))
    await screen.findByTestId('flow')
    const before = [1, 2, 3, 4].map(pos)
    const join = (id: number): LiveGraph => ({
      ...tiered,
      agents: [...tiered.agents, agent({ id, parent: 1 })],
      edges: [...tiered.edges, dispatch(1, id)],
    })
    layout.mockRejectedValueOnce(new Error('worker crashed'))
    rerender(view({ graph: join(5) }))
    await screen.findByText("Couldn't lay out the graph: worker crashed")
    // The drawn graph stays up under the error line.
    expect([1, 2, 3, 4].map(pos)).toEqual(before)
    expect(screen.queryByTestId('node-5')).not.toBeInTheDocument()
    rerender(view({ graph: join(6) }))
    await screen.findByTestId('node-6')
    expect([1, 2, 3, 4].map(pos)).toEqual(before)
    expect(screen.queryByText(/Couldn't lay out/)).not.toBeInTheDocument()
  })

  it('lets the newest structure win when an older layout lands last', async () => {
    const { rerender } = render(view({}))
    await screen.findByTestId('flow')
    const join = (id: number): LiveGraph => ({
      ...tiered,
      agents: [...tiered.agents, agent({ id, parent: 1 })],
      edges: [...tiered.edges, dispatch(1, id)],
    })
    const older = deferred<ElkNode>()
    layout.mockReturnValueOnce(older.promise)
    rerender(view({ graph: join(5) }))
    await act(async () => {})
    rerender(view({ graph: join(6) }))
    await screen.findByTestId('node-6')
    await act(async () =>
      older.resolve({
        id: 'root',
        children: [
          { id: '1', x: 9999, y: 9999 },
          { id: '5', x: 9999, y: 9999 },
        ],
      }),
    )
    expect(screen.queryByTestId('node-5')).not.toBeInTheDocument()
    expect(node(1).dataset.x).not.toBe('9999')
  })

  it('retries a failed layout on a tab switch, and reports non-Error rejections', async () => {
    layout.mockRejectedValueOnce('out of memory')
    const { rerender } = render(view({}))
    expect(await screen.findByText("Couldn't lay out the graph: out of memory")).toBeInTheDocument()
    rerender(view({ graph: { ...tiered, sessionId: 's-2' }, active: ws(2) }))
    await screen.findByTestId('flow')
    expect(layout).toHaveBeenCalledTimes(2)
  })

  it('shows the empty state when the tab has no session', () => {
    render(view({ graph: null }))
    expect(screen.getByText('No session to draw yet.')).toBeInTheDocument()
    expect(layout).not.toHaveBeenCalled()
  })
})

describe('re-fit on resize', () => {
  let observers: { callback: ResizeObserverCallback; targets: Set<Element> }[] = []
  /** Every observed element reports `width` x `height`, as a real observer would on a resize. */
  const resize = (width: number, height: number) =>
    act(() => {
      for (const o of observers) {
        for (const target of o.targets) {
          const entry = { target, contentRect: { width, height } } as ResizeObserverEntry
          o.callback([entry], {} as ResizeObserver)
        }
      }
    })
  const settle = (ms = REFIT_MS) => act(() => vi.advanceTimersByTimeAsync(ms))

  /** The graph drawn, then fake timers for the fit delay. */
  async function drawn() {
    const rendered = render(view({}))
    await screen.findByTestId('flow')
    vi.useFakeTimers({ toFake: ['setTimeout', 'clearTimeout'] })
    return rendered
  }

  beforeEach(() => {
    observers = []
    vi.stubGlobal(
      'ResizeObserver',
      class {
        targets = new Set<Element>()
        constructor(public callback: ResizeObserverCallback) {
          observers.push(this)
        }
        observe(target: Element) {
          this.targets.add(target)
        }
        unobserve(target: Element) {
          this.targets.delete(target)
        }
        disconnect() {
          this.targets.clear()
        }
      },
    )
  })

  afterEach(() => {
    vi.useRealTimers()
    vi.unstubAllGlobals()
  })

  it('fits the graph to a new canvas size once the size settles', async () => {
    await drawn()
    // The first size is the one the flow already fit on its first draw.
    await resize(800, 600)
    await settle()
    expect(fitView).not.toHaveBeenCalled()
    // A handle drag: many sizes, one fit at the last.
    await resize(900, 600)
    await resize(1000, 600)
    await settle(REFIT_MS - 1)
    expect(fitView).not.toHaveBeenCalled()
    await settle(1)
    expect(fitView).toHaveBeenCalledOnce()
    expect(fitView).toHaveBeenCalledWith({ maxZoom: 1 })
    // The same size again is no change.
    await resize(1000, 600)
    await settle()
    expect(fitView).toHaveBeenCalledOnce()
  })

  it('skips the hidden 0x0 size and fits on the next one, as on return from Audit', async () => {
    await drawn()
    await resize(800, 600)
    await resize(900, 600)
    // Audit opens before that fit lands: nothing fits a hidden canvas.
    await resize(0, 0)
    await settle()
    expect(fitView).not.toHaveBeenCalled()
    await resize(900, 600)
    await settle()
    expect(fitView).toHaveBeenCalledOnce()
  })

  it('fits on the first size of a graph first drawn while hidden', async () => {
    await drawn()
    await resize(0, 0)
    await resize(800, 600)
    await settle()
    expect(fitView).toHaveBeenCalledOnce()
  })

  it('stops watching, and drops a pending fit, once the graph goes', async () => {
    const { unmount } = await drawn()
    await resize(800, 600)
    await resize(900, 600)
    unmount()
    await settle()
    expect(fitView).not.toHaveBeenCalled()
    expect(observers.every((o) => o.targets.size === 0)).toBe(true)
  })
})

describe('session line', () => {
  const MIN = 60_000
  const ran = (runMs: number): LiveGraph => ({ ...tiered, runMs })
  const running = (time: string) => screen.queryByText(new RegExp(`^running ${time} · `))

  beforeEach(() => {
    vi.useFakeTimers({ toFake: ['Date', 'setTimeout', 'clearTimeout'] })
    vi.setSystemTime(new Date('2026-10-12T11:00:00Z'))
  })

  afterEach(() => {
    vi.useRealTimers()
  })

  it("ticks the session's run time on from when it was read, each minute it crosses", async () => {
    render(view({ graph: ran(5 * MIN + 30_000) }))
    expect(running('5m')).toBeInTheDocument()
    await act(() => vi.advanceTimersByTimeAsync(29_999))
    expect(running('5m')).toBeInTheDocument()
    await act(() => vi.advanceTimersByTimeAsync(1))
    expect(running('6m')).toBeInTheDocument()
  })

  it('keeps ticking through a reread with the same run time, and restarts from a new one', async () => {
    const { rerender } = render(view({ graph: ran(5 * MIN) }))
    await act(() => vi.advanceTimersByTimeAsync(3 * MIN))
    expect(running('8m')).toBeInTheDocument()
    // A store change elsewhere rereads the graph; this session has no new record.
    rerender(view({ graph: ran(5 * MIN) }))
    expect(running('8m')).toBeInTheDocument()
    rerender(view({ graph: ran(9 * MIN) }))
    expect(running('9m')).toBeInTheDocument()
    await act(() => vi.advanceTimersByTimeAsync(MIN))
    expect(running('10m')).toBeInTheDocument()
  })

  it('keeps counting a session through a visit to another tab', async () => {
    const { rerender } = render(view({ graph: ran(5 * MIN) }))
    await act(() => vi.advanceTimersByTimeAsync(2 * MIN))
    rerender(view({ graph: { ...ran(5 * MIN), sessionId: 's-2' }, active: ws(2) }))
    await act(() => vi.advanceTimersByTimeAsync(MIN))
    rerender(view({ graph: ran(5 * MIN) }))
    expect(running('8m')).toBeInTheDocument()
  })

  it("restarts the count on another tab's session", async () => {
    const { rerender } = render(view({ graph: ran(5 * MIN) }))
    await act(() => vi.advanceTimersByTimeAsync(2 * MIN))
    rerender(view({ graph: { ...ran(5 * MIN), sessionId: 's-2' }, active: ws(2) }))
    expect(running('5m')).toBeInTheDocument()
  })
})
