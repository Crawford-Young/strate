import type { ElkNode } from 'elkjs/lib/elk-api'
import ELK from 'elkjs/lib/elk.bundled.js'
import { describe, expect, it, vi } from 'vitest'
import type { GraphAgent, GraphEdge, LiveGraph } from './api'
import {
  NODE_GAP,
  NODE_HEIGHT,
  NODE_WIDTH,
  layoutGraph,
  parentsOf,
  structureKey,
  type ElkLike,
  type Layout,
  type Positions,
} from './graphLayout'

const elk = new ELK()

function agent(id: number, parent: number | null = null): GraphAgent {
  return {
    id,
    agentId: parent === null ? null : `a-${id}`,
    kind: parent === null ? 'orchestrator' : 'subagent',
    name: null,
    agentType: null,
    description: null,
    model: null,
    state: 'working',
    costUsd: null,
    contextTokens: null,
    contextPct: null,
    parent,
  }
}

const dispatch = (parent: number, child: number): GraphEdge => ({ kind: 'dispatch', parent, child })

function graph(agents: GraphAgent[], edges: GraphEdge[]): LiveGraph {
  return { sessionId: 's', live: true, sessionCostUsd: null, runMs: null, agents, edges }
}

// Orchestrator 1 → subagents 2 and 3, teammate 4; subagent 2 → its own dispatch 5.
const tiered = graph(
  [agent(1), agent(2, 1), agent(3, 1), { ...agent(4, 1), kind: 'teammate' }, agent(5, 2)],
  [dispatch(1, 2), dispatch(1, 3), { kind: 'teammate', parent: 1, child: 4 }, dispatch(2, 5)],
)

const at = (positions: Positions, id: number) => positions.get(id) as { x: number; y: number }

/** A previous layout of `g` that placed its agents at `positions`. */
const placed = (g: LiveGraph, positions: Positions): Layout => ({
  positions,
  parents: parentsOf(g),
})

const overlaps = (a: { x: number; y: number }, b: { x: number; y: number }) =>
  Math.abs(a.x - b.x) < NODE_WIDTH && Math.abs(a.y - b.y) < NODE_HEIGHT

describe('layoutGraph', () => {
  it('lays a directed unit under its director with its own agents beneath', async () => {
    const directed = graph(
      [agent(1), agent(2, 1), agent(3, 1), agent(4, 3)],
      [dispatch(1, 2), { kind: 'directs', parent: 1, child: 3 }, dispatch(3, 4)],
    )
    const { positions } = await layoutGraph(elk, directed)
    expect(at(positions, 1).y).toBeLessThan(at(positions, 3).y)
    expect(at(positions, 3).y).toBe(at(positions, 2).y)
    expect(at(positions, 3).y).toBeLessThan(at(positions, 4).y)
    expect(structureKey(directed)).toContain('directs:1>3')
  })

  it('lays the graph out top-down in tiers', async () => {
    const { positions } = await layoutGraph(elk, tiered)
    expect([...positions.keys()].sort()).toEqual([1, 2, 3, 4, 5])
    expect(at(positions, 1).y).toBeLessThan(at(positions, 2).y)
    expect(at(positions, 2).y).toBe(at(positions, 3).y)
    expect(at(positions, 4).y).toBe(at(positions, 2).y)
    expect(at(positions, 2).y).toBeLessThan(at(positions, 5).y)
  })

  it('centres a parent over its children', async () => {
    const fan = graph(
      [agent(1), agent(2, 1), agent(3, 1), agent(4, 1), agent(5, 1)],
      [dispatch(1, 2), dispatch(1, 3), dispatch(1, 4), dispatch(1, 5)],
    )
    const { positions } = await layoutGraph(elk, fan)
    const xs = [2, 3, 4, 5].map((id) => at(positions, id).x)
    const mid = (Math.min(...xs) + Math.max(...xs)) / 2
    expect(Math.abs(at(positions, 1).x - mid)).toBeLessThanOrEqual(1)
  })

  it('never overlaps two nodes', async () => {
    const positions = [...(await layoutGraph(elk, tiered)).positions.values()]
    positions.forEach((a, i) =>
      positions.slice(i + 1).forEach((b) => expect(overlaps(a, b)).toBe(false)),
    )
  })

  it('keeps pinned nodes exactly where they were and places the new ones', async () => {
    const first = await layoutGraph(elk, tiered)
    const pinned = new Map([...first.positions].map(([id, p]) => [id, { x: p.x + 7, y: p.y + 3 }]))
    const grown = graph([...tiered.agents, agent(6, 3)], [...tiered.edges, dispatch(3, 6)])
    const { positions: next } = await layoutGraph(elk, grown, placed(tiered, pinned))
    for (const [id, p] of pinned) expect(next.get(id)).toEqual(p)
    expect(at(next, 6).y).toBeGreaterThan(at(next, 3).y)
  })

  it('forgets pinned nodes that left the graph', async () => {
    const pinned = new Map([
      [1, { x: 0, y: 0 }],
      [99, { x: 500, y: 500 }],
    ])
    const one = graph([agent(1)], [])
    const { positions: next } = await layoutGraph(elk, one, placed(one, pinned))
    expect([...next.keys()]).toEqual([1])
  })

  it('nudges a new node right, past every pinned node it would overlap', async () => {
    const fake: ElkLike = {
      layout: vi.fn(async (g: ElkNode) => ({
        ...g,
        children: g.children?.map((c) => ({ ...c, x: 10, y: 4 })),
      })),
    }
    const pinned = new Map([
      [1, { x: 0, y: 0 }],
      [2, { x: NODE_WIDTH + NODE_GAP, y: 0 }],
      [3, { x: 0, y: 400 }],
    ])
    const flat = graph([agent(1), agent(2, 1), agent(3, 1), agent(4, 1)], [])
    const { positions: next } = await layoutGraph(fake, flat, placed(flat, pinned))
    expect(at(next, 4)).toEqual({ x: 2 * (NODE_WIDTH + NODE_GAP), y: 4 })
  })

  it('re-places a node whose parent changed, as if it were new', async () => {
    // Subagent 6 shows up before its dispatch edge: ELK lays it out as a root.
    const orphan = graph([...tiered.agents, agent(6, 3)], tiered.edges)
    const first = await layoutGraph(elk, orphan)
    expect(at(first.positions, 6).y).toBeLessThan(at(first.positions, 3).y)
    const adopted = graph(orphan.agents, [...tiered.edges, dispatch(3, 6)])
    const { positions: next } = await layoutGraph(elk, adopted, first)
    expect(at(next, 6).y).toBeGreaterThan(at(next, 3).y)
    for (const id of [1, 2, 3, 4, 5]) expect(next.get(id)).toEqual(first.positions.get(id))
  })

  it('places nothing when ELK returns no children', async () => {
    const fake: ElkLike = { layout: vi.fn(async () => ({ id: 'root' })) }
    expect((await layoutGraph(fake, graph([], []))).positions.size).toBe(0)
  })

  it('drops edges to agents that are not in the graph', async () => {
    const { positions } = await layoutGraph(elk, graph([agent(1)], [dispatch(1, 2)]))
    expect([...positions.keys()]).toEqual([1])
  })

  it('asks ELK for a layered, top-down layout of fixed-size nodes', async () => {
    const fake: ElkLike = { layout: vi.fn(async (g) => g) }
    const { positions } = await layoutGraph(fake, graph([agent(1)], []))
    const sent = vi.mocked(fake.layout).mock.calls[0]?.[0]
    expect(sent?.layoutOptions).toMatchObject({
      'elk.algorithm': 'layered',
      'elk.direction': 'DOWN',
    })
    expect(sent?.children).toEqual([{ id: '1', width: NODE_WIDTH, height: NODE_HEIGHT }])
    // A node ELK left unplaced lands at the origin.
    expect(positions.get(1)).toEqual({ x: 0, y: 0 })
  })
})

describe('parentsOf', () => {
  it('keys each agent by its sorted parents, roots by the empty key', () => {
    const g = graph([agent(1), agent(2), agent(3, 2)], [dispatch(2, 3), dispatch(1, 3)])
    expect(parentsOf(g)).toEqual(
      new Map([
        [1, ''],
        [2, ''],
        [3, '1,2'],
      ]),
    )
  })
})

describe('structureKey', () => {
  it('ignores state, cost and context', () => {
    const busy = graph(
      tiered.agents.map((a) => ({ ...a, state: 'idle' as const, costUsd: 2, contextPct: 90 })),
      tiered.edges,
    )
    expect(structureKey(busy)).toBe(structureKey(tiered))
  })

  it('ignores edges the layout drops, to agents not in the graph', () => {
    const dangling = graph(tiered.agents, [...tiered.edges, dispatch(5, 99)])
    expect(structureKey(dangling)).toBe(structureKey(tiered))
  })

  it('ignores order', () => {
    const shuffled = graph([...tiered.agents].reverse(), [...tiered.edges].reverse())
    expect(structureKey(shuffled)).toBe(structureKey(tiered))
  })

  it('changes when an agent or an edge is added or removed', () => {
    const key = structureKey(tiered)
    expect(structureKey(graph([...tiered.agents, agent(6)], tiered.edges))).not.toBe(key)
    expect(structureKey(graph(tiered.agents.slice(1), tiered.edges))).not.toBe(key)
    expect(structureKey(graph(tiered.agents, [...tiered.edges, dispatch(3, 5)]))).not.toBe(key)
    expect(structureKey(graph(tiered.agents, tiered.edges.slice(1)))).not.toBe(key)
    const retyped = tiered.edges.map((e) => ({ ...e, kind: 'teammate' as const }))
    expect(structureKey(graph(tiered.agents, retyped))).not.toBe(key)
  })
})
