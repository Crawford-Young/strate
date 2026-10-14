import { describe, expect, it } from 'vitest'
import type { GraphAgent, LiveGraph, Workstream } from './api'
import { duration, money, nodeState, sessionCost, sessionLine } from './format'

const MIN = 60_000

function agent(costUsd: number | null): GraphAgent {
  return {
    id: 1,
    agentId: null,
    kind: 'orchestrator',
    name: null,
    agentType: null,
    description: null,
    model: null,
    state: 'working',
    costUsd,
    contextTokens: null,
    contextPct: null,
    nudgePct: 80,
    hardStopPct: 95,
    parent: null,
  }
}

function graph(...costs: (number | null)[]): LiveGraph {
  return { sessionId: 's', live: true, agents: costs.map(agent), edges: [] }
}

const ws: Workstream = {
  id: 1,
  name: 'strate-17',
  fallback: false,
  costUsd: 3.1,
  unpriced: 0,
  state: 'working',
  needsYou: 0,
  sessionId: 's',
  live: true,
  startedAt: '2026-10-12T10:00:00Z',
  director: null,
}
const at = (iso: string) => Date.parse(iso)

describe('money', () => {
  it('shows dollars to the cent', () => {
    expect(money(0.4)).toBe('$0.40')
    expect(money(12.345)).toBe('$12.35')
    expect(money(0)).toBe('$0.00')
  })

  it('shows a dash when nothing is priced', () => {
    expect(money(null)).toBe('$—')
  })
})

describe('duration', () => {
  it('shows minutes under an hour', () => {
    expect(duration(0)).toBe('0m')
    expect(duration(59 * MIN)).toBe('59m')
  })

  it('shows hours and minutes under a day', () => {
    expect(duration(62 * MIN)).toBe('1h 2m')
  })

  it('shows days and hours from a day', () => {
    expect(duration((26 * 60 + 5) * MIN)).toBe('1d 2h')
  })

  it('counts whole minutes elapsed, like a clock, and never goes negative', () => {
    expect(duration(119_999)).toBe('1m')
    expect(duration(120_000)).toBe('2m')
    expect(duration(-5 * MIN)).toBe('0m')
  })
})

describe('nodeState', () => {
  it('maps the api states onto the library states', () => {
    expect(nodeState('needs_you')).toBe('needs-you')
    expect(nodeState('working')).toBe('working')
    expect(nodeState('idle')).toBe('idle')
    expect(nodeState('done')).toBe('done')
  })
})

describe('sessionCost', () => {
  it('sums the drawn agents, skipping unpriced ones', () => {
    expect(sessionCost(graph(0.25, null, 0.5))).toBe(0.75)
  })

  it('leaves out directed units and everything beneath them', () => {
    const at = (id: number, costUsd: number): GraphAgent => ({ ...agent(costUsd), id })
    const director: LiveGraph = {
      sessionId: 's',
      live: true,
      agents: [at(1, 1), at(2, 10), at(3, 100), at(4, 1000)],
      edges: [
        { kind: 'dispatch', parent: 1, child: 2 },
        { kind: 'directs', parent: 1, child: 3 },
        { kind: 'dispatch', parent: 3, child: 4 },
        { kind: 'directs', parent: 1, child: 4 },
      ],
    }
    expect(sessionCost(director)).toBe(11)
  })

  it('is null with no graph or nothing priced', () => {
    expect(sessionCost(null)).toBeNull()
    expect(sessionCost(graph(null, null))).toBeNull()
  })
})

describe('sessionLine', () => {
  it('shows running time, session $, total $ and needs-you for a live tab', () => {
    const line = sessionLine({ ...ws, needsYou: 2 }, graph(0.42), at('2026-10-12T11:02:00Z'))
    expect(line).toBe('running 1h 2m · $0.42 this session · $3.10 total · 2 needs you')
  })

  it('leaves needs-you out at zero', () => {
    expect(sessionLine(ws, graph(0.42), at('2026-10-12T10:05:00Z'))).toBe(
      'running 5m · $0.42 this session · $3.10 total',
    )
  })

  it('says finished once the session is not live', () => {
    expect(sessionLine({ ...ws, live: false }, null, at('2026-10-12T11:00:00Z'))).toBe(
      'finished · $— this session · $3.10 total',
    )
  })

  it('says running without a time when the start is unknown', () => {
    expect(sessionLine({ ...ws, startedAt: null }, null, 0)).toBe(
      'running · $— this session · $3.10 total',
    )
  })
})
