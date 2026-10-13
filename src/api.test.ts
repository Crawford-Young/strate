import { invoke } from '@tauri-apps/api/core'
import { listen } from '@tauri-apps/api/event'
import { beforeEach, describe, expect, it, vi } from 'vitest'
import {
  STORE_CHANGED,
  auditRefresh,
  auditRollup,
  auditTimeline,
  liveGraph,
  needsYou,
  onStoreChanged,
  status,
  workstreamCosts,
  workstreams,
  type Audit,
  type AuditFilter,
  type AuditPath,
  type LiveGraph,
  type TimelineDay,
} from './api'

vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn() }))
vi.mock('@tauri-apps/api/event', () => ({ listen: vi.fn() }))

const invokeMock = vi.mocked(invoke)
const listenMock = vi.mocked(listen)

beforeEach(() => {
  invokeMock.mockReset()
  listenMock.mockReset()
})

describe('commands', () => {
  it.each([
    ['status', status, 'live'],
    ['workstreams', workstreams, []],
    ['workstream_costs', workstreamCosts, []],
    ['needs_you', needsYou, []],
  ] as const)('%s invokes its command with no arguments', async (command, call, answer) => {
    invokeMock.mockResolvedValueOnce(answer)
    await expect(call()).resolves.toEqual(answer)
    expect(invokeMock).toHaveBeenCalledWith(command)
  })

  it('live_graph passes the workstream id', async () => {
    const graph: LiveGraph = {
      sessionId: 's-alpha',
      live: true,
      agents: [
        {
          id: 1,
          agentId: null,
          kind: 'orchestrator',
          name: 'alpha-1',
          agentType: null,
          description: null,
          model: 'claude-haiku-4-5',
          state: 'working',
          costUsd: 0.01,
          contextTokens: 50000,
          contextPct: 25,
          nudgePct: 70,
          hardStopPct: 90,
          parent: null,
        },
      ],
      edges: [],
    }
    invokeMock.mockResolvedValueOnce(graph)
    await expect(liveGraph(7)).resolves.toEqual(graph)
    expect(invokeMock).toHaveBeenCalledWith('live_graph', { workstream: 7 })
  })

  it('audit_rollup passes the filter and returns the tree', async () => {
    const filter: AuditFilter = {
      from: '2026-10-01',
      to: '2026-10-31',
      repos: ['demo'],
      groupBy: 'agent_type',
    }
    const audit: Audit = {
      root: {
        kind: 'root',
        key: '',
        label: 'All',
        github: false,
        metrics: {
          costUsd: 0.065,
          unpriced: 0,
          tokens: { input: 1, output: 2, cacheRead: 3, cacheWrite: 4 },
          agentMs: 180000,
          approximate: true,
          activeMs: 470000,
          waitMs: 295000,
          sessions: 1,
          agents: 1,
          rotations: 0,
          elapsedMs: null,
          open: false,
          progress: { completed: 1, total: 2 },
        },
        children: [],
      },
      headline: {
        issuesClosed: 1,
        medianElapsedMs: null,
        usdPerClosedIssue: 0.065,
        usdPerMergedPr: null,
        cloudUncounted: true,
      },
      githubErrors: [{ repo: 'other', error: 'gh: not logged in' }],
    }
    invokeMock.mockResolvedValueOnce(audit)
    await expect(auditRollup(filter)).resolves.toEqual(audit)
    expect(invokeMock).toHaveBeenCalledWith('audit_rollup', { filter })
  })

  it('audit_timeline passes the filter and node path and returns the days', async () => {
    const filter: AuditFilter = {
      from: '2026-10-01',
      to: '2026-10-31',
      repos: [],
      groupBy: 'hierarchy',
    }
    const path: AuditPath = [
      ['repo', 'demo'],
      ['issue', '9'],
    ]
    const days: TimelineDay[] = [
      {
        day: '2026-10-05',
        spans: [
          { startMs: 300000, endMs: 310000, kind: 'active' },
          { startMs: 300000, endMs: 305000, kind: 'agent' },
          { startMs: 310000, endMs: 320000, kind: 'wait' },
        ],
        marks: [{ atMs: 25200000, label: 'PR #12 merged' }],
      },
    ]
    invokeMock.mockResolvedValueOnce(days)
    await expect(auditTimeline(filter, path)).resolves.toEqual(days)
    expect(invokeMock).toHaveBeenCalledWith('audit_timeline', { filter, path })
  })

  it('audit_refresh asks for a GitHub refresh and resolves with nothing', async () => {
    invokeMock.mockResolvedValueOnce(undefined)
    await expect(auditRefresh()).resolves.toBeUndefined()
    expect(invokeMock).toHaveBeenCalledWith('audit_refresh')
  })

  it('a command error rejects with the message', async () => {
    invokeMock.mockRejectedValueOnce('no such table: workstreams')
    await expect(workstreams()).rejects.toBe('no such table: workstreams')
  })
})

describe('onStoreChanged', () => {
  it('calls back on each store-changed event and returns the unlisten', async () => {
    const unlisten = vi.fn()
    let handler: (() => void) | undefined
    listenMock.mockImplementationOnce((event, h) => {
      expect(event).toBe(STORE_CHANGED)
      handler = () => h({ event: STORE_CHANGED, id: 1, payload: null })
      return Promise.resolve(unlisten)
    })
    const callback = vi.fn()
    const stop = await onStoreChanged(callback)
    expect(STORE_CHANGED).toBe('store-changed')
    handler?.()
    handler?.()
    expect(callback).toHaveBeenCalledTimes(2)
    stop()
    expect(unlisten).toHaveBeenCalledOnce()
  })
})
