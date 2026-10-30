import { describe, expect, it } from 'vitest'
import type { GraphAgent, LiveGraph, Workstream } from './api'
import { duration, money, nodeState, sessionLine } from './format'

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
    parent: null,
  }
}

/** A session that has run 1h 2m and spent $0.42 in this workstream. */
function graph(over: Partial<LiveGraph> = {}): LiveGraph {
  return {
    sessionId: 's',
    live: true,
    sessionCostUsd: 0.42,
    runMs: 62 * MIN,
    agents: [agent(0.3)],
    edges: [],
    ...over,
  }
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

describe('sessionLine', () => {
  it('shows run time, session $, total $ and needs-you for a live tab', () => {
    expect(sessionLine({ ...ws, needsYou: 2 }, graph(), 0)).toBe(
      'running 1h 2m · $0.42 this session · $3.10 total · 2 needs you',
    )
  })

  it('adds the time since the graph was read to the session run time', () => {
    expect(sessionLine(ws, graph(), 3 * MIN)).toBe(
      'running 1h 5m · $0.42 this session · $3.10 total',
    )
  })

  it("takes the session $ and run time from the graph, never its agents or the tab's start", () => {
    const early = { ...ws, startedAt: '2026-10-01T00:00:00Z' }
    expect(sessionLine(early, graph({ agents: [agent(9), agent(4)] }), 0)).toBe(
      'running 1h 2m · $0.42 this session · $3.10 total',
    )
  })

  it('says finished once the session is not live', () => {
    expect(sessionLine({ ...ws, live: false }, graph({ sessionCostUsd: null }), 0)).toBe(
      'finished · $— this session · $3.10 total',
    )
  })

  it('says running without a time until the graph has a run time', () => {
    expect(sessionLine(ws, null, 0)).toBe('running · $— this session · $3.10 total')
    expect(sessionLine(ws, graph({ runMs: null }), 5 * MIN)).toBe(
      'running · $0.42 this session · $3.10 total',
    )
  })
})
