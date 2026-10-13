import { act, renderHook } from '@testing-library/react'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { auditTimeline, type AuditFilter, type AuditPath, type TimelineDay } from './api'
import { REFETCH_DEBOUNCE_MS } from './useAudit'
import { useTimeline } from './useTimeline'

vi.mock('./api', () => ({ auditTimeline: vi.fn() }))

const timelineMock = vi.mocked(auditTimeline)

const filter: AuditFilter = {
  from: '2026-10-07',
  to: '2026-10-13',
  repos: [],
  groupBy: 'hierarchy',
}
const repo: AuditPath = [['repo', 'acme/web']]
const issue: AuditPath = [
  ['repo', 'acme/web'],
  ['issue', '11'],
]
const day = (d: string): TimelineDay => ({ day: d, spans: [], marks: [] })

const settle = () => act(() => vi.advanceTimersByTimeAsync(0))

beforeEach(() => {
  vi.useFakeTimers()
  timelineMock
    .mockReset()
    .mockImplementation(() => Promise.resolve([day(`read-${timelineMock.mock.calls.length}`)]))
})

afterEach(() => {
  vi.useRealTimers()
})

interface Props {
  path: AuditPath | null
  active: boolean
  version: number
}

function setup(path: AuditPath | null, active: boolean) {
  let props: Props = { path, active, version: 0 }
  const hook = renderHook((p: Props) => useTimeline(filter, p.path, p.active, p.version), {
    initialProps: props,
  })
  const rerender = (next: Partial<Props>) => hook.rerender((props = { ...props, ...next }))
  return { result: hook.result, rerender }
}

describe('useTimeline', () => {
  it('reads the selected row with the filter and path', async () => {
    const { result } = setup(repo, true)
    await settle()
    expect(timelineMock).toHaveBeenCalledExactlyOnceWith(filter, repo)
    expect(result.current.days?.[0]?.day).toBe('read-1')
    expect(result.current.error).toBeNull()
  })

  it('reads nothing with no row selected, or while Audit is hidden', async () => {
    const { rerender } = setup(null, true)
    await settle()
    rerender({ path: repo, active: false })
    await settle()
    expect(timelineMock).not.toHaveBeenCalled()
    rerender({ active: true })
    await settle()
    expect(timelineMock).toHaveBeenCalledOnce()
  })

  it('reads again for another row, and shows nothing of the old one meanwhile', async () => {
    const { result, rerender } = setup(repo, true)
    await settle()
    rerender({ path: issue })
    expect(result.current.days).toBeNull()
    await settle()
    expect(timelineMock).toHaveBeenLastCalledWith(filter, issue)
    expect(result.current.days?.[0]?.day).toBe('read-2')
  })

  it('rereads once after a burst of store changes has been quiet for 1 s', async () => {
    const { result, rerender } = setup(repo, true)
    await settle()
    for (let version = 1; version <= 10; version++) {
      rerender({ version })
      await act(() => vi.advanceTimersByTimeAsync(100))
    }
    expect(timelineMock).toHaveBeenCalledOnce()
    await act(() => vi.advanceTimersByTimeAsync(REFETCH_DEBOUNCE_MS))
    expect(timelineMock).toHaveBeenCalledTimes(2)
    expect(result.current.days?.[0]?.day).toBe('read-2')
  })

  it('reports a failed read, and clears it with the next row', async () => {
    timelineMock.mockRejectedValueOnce('bad day: 2026-13-01')
    const { result, rerender } = setup(repo, true)
    await settle()
    expect(result.current.error).toBe('bad day: 2026-13-01')
    expect(result.current.days).toBeNull()
    rerender({ path: issue })
    await settle()
    expect(result.current.error).toBeNull()
    expect(result.current.days).toHaveLength(1)
  })

  it('has no days or error with no row selected', () => {
    const { result } = setup(null, true)
    expect(result.current).toEqual({ days: null, error: null })
  })
})
