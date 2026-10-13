import { act, renderHook } from '@testing-library/react'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { outcome, performance } from '../tests/performance-fixtures'
import { auditPerformance, type AuditFilter } from './api'
import { usePerformance } from './usePerformance'

vi.mock('./api', () => ({ auditPerformance: vi.fn() }))

const performanceMock = vi.mocked(auditPerformance)

const week: AuditFilter = { from: '2026-10-07', to: '2026-10-13', repos: [], groupBy: 'hierarchy' }
// Each read answers with the number of reads before it, in `openIssues`.
const answer = () =>
  Promise.resolve(
    performance({ outcome: outcome({ openIssues: performanceMock.mock.calls.length }) }),
  )
const shown = (result: { current: ReturnType<typeof usePerformance> }) =>
  result.current.performance?.outcome.openIssues

const settle = () => act(() => vi.advanceTimersByTimeAsync(0))

beforeEach(() => {
  vi.useFakeTimers()
  performanceMock.mockReset().mockImplementation(answer)
})

afterEach(() => {
  vi.useRealTimers()
})

interface Props {
  filter: AuditFilter
  active: boolean
  version: number
}

function setup(filter: AuditFilter, active: boolean) {
  let props: Props = { filter, active, version: 0 }
  const hook = renderHook((p: Props) => usePerformance(p.filter, p.active, p.version), {
    initialProps: props,
  })
  const rerender = (next: Partial<Props>) => hook.rerender((props = { ...props, ...next }))
  return { result: hook.result, rerender, bump: () => rerender({ version: props.version + 1 }) }
}

describe('usePerformance', () => {
  it('reads the filter once active', async () => {
    const { result } = setup(week, true)
    expect(result.current).toEqual({ performance: null, error: null, pending: true })
    await settle()
    expect(performanceMock).toHaveBeenCalledExactlyOnceWith(week)
    expect(shown(result)).toBe(1)
    expect(result.current.pending).toBe(false)
  })

  it('reads nothing while inactive, and on activation reads at once', async () => {
    const { rerender, bump } = setup(week, false)
    act(() => bump())
    await settle()
    expect(performanceMock).not.toHaveBeenCalled()
    rerender({ active: true })
    await settle()
    expect(performanceMock).toHaveBeenCalledTimes(1)
  })

  it('rereads when the version moves, showing the stale read meanwhile', async () => {
    const { result, bump } = setup(week, true)
    await settle()
    act(() => bump())
    expect(shown(result)).toBe(1)
    expect(result.current.pending).toBe(true)
    await settle()
    expect(shown(result)).toBe(2)
  })

  it('ignores the grouping, which the read does not depend on', async () => {
    const { result, rerender } = setup(week, true)
    await settle()
    rerender({ filter: { ...week, groupBy: 'model' } })
    expect(result.current.pending).toBe(false)
    await settle()
    expect(performanceMock).toHaveBeenCalledTimes(1)
  })

  it('caches per range and repos', async () => {
    const { result, rerender } = setup(week, true)
    await settle()
    rerender({ filter: { ...week, repos: ['acme/web'] } })
    await settle()
    expect(shown(result)).toBe(2)
    rerender({ filter: week })
    expect(shown(result)).toBe(1)
    await settle()
    expect(performanceMock).toHaveBeenCalledTimes(2)
  })

  it('reports a failed read', async () => {
    performanceMock.mockRejectedValueOnce('bad day')
    const { result } = setup(week, true)
    await settle()
    expect(result.current).toEqual({ performance: null, error: 'bad day', pending: true })
  })
})
