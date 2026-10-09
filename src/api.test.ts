import { invoke } from '@tauri-apps/api/core'
import { listen } from '@tauri-apps/api/event'
import { beforeEach, describe, expect, it, vi } from 'vitest'
import {
  STORE_CHANGED,
  liveGraph,
  needsYou,
  onStoreChanged,
  status,
  workstreamCosts,
  workstreams,
  type LiveGraph,
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
          parent: null,
        },
      ],
      edges: [],
    }
    invokeMock.mockResolvedValueOnce(graph)
    await expect(liveGraph(7)).resolves.toEqual(graph)
    expect(invokeMock).toHaveBeenCalledWith('live_graph', { workstream: 7 })
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
