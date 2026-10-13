import { act, renderHook } from '@testing-library/react'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { audit, node } from '../tests/audit-fixtures'
import { auditRollup, type AuditFilter } from './api'
import { useAudit } from './useAudit'

vi.mock('./api', () => ({ auditRollup: vi.fn() }))

const rollupMock = vi.mocked(auditRollup)

const week: AuditFilter = { from: '2026-10-07', to: '2026-10-13', repos: [], groupBy: 'hierarchy' }
const byDay: AuditFilter = { ...week, groupBy: 'day' }
// Each read answers with a tree naming its filter and how many reads came before it.
const answer = (filter: AuditFilter) =>
  Promise.resolve(audit([node('repo', `${filter.groupBy}#${rollupMock.mock.calls.length}`)]))
const shown = (result: { current: ReturnType<typeof useAudit> }) =>
  result.current.audit?.root.children[0]?.key

/** Lets resolved reads land. */
const settle = () => act(() => vi.advanceTimersByTimeAsync(0))

beforeEach(() => {
  vi.useFakeTimers()
  rollupMock.mockReset().mockImplementation(answer)
})

afterEach(() => {
  vi.useRealTimers()
})

interface Props {
  filter: AuditFilter
  active: boolean
  version: number
}

/** The hook under `props`, plus `storeChanged`, which bumps the store version as the event does. */
function setup(filter: AuditFilter, active: boolean) {
  let props: Props = { filter, active, version: 0 }
  const hook = renderHook((p: Props) => useAudit(p.filter, p.active, p.version), {
    initialProps: props,
  })
  const rerender = (next: Partial<Props>) => hook.rerender((props = { ...props, ...next }))
  const storeChanged = () => rerender({ version: props.version + 1 })
  return { result: hook.result, unmount: hook.unmount, rerender, storeChanged }
}

describe('useAudit', () => {
  it('reads the filter once active', async () => {
    const { result } = setup(week, true)
    expect(result.current).toEqual({ audit: null, error: null, pending: true })
    await settle()
    expect(rollupMock).toHaveBeenCalledExactlyOnceWith(week)
    expect(shown(result)).toBe('hierarchy#1')
    expect(result.current.pending).toBe(false)
  })

  it('keeps showing the last rollup that landed while a new filter is first read', async () => {
    const { result, rerender } = setup(week, true)
    await settle()
    rerender({ filter: byDay, active: true })
    expect(shown(result)).toBe('hierarchy#1')
    expect(result.current.pending).toBe(true)
    await settle()
    expect(shown(result)).toBe('day#2')
    expect(result.current.pending).toBe(false)
  })

  it('reads nothing while inactive, store changes included, and reads at once on activation', async () => {
    const { result, rerender, storeChanged } = setup(week, false)
    await settle()
    for (let i = 0; i < 5; i++) act(() => storeChanged())
    await act(() => vi.advanceTimersByTimeAsync(5_000))
    expect(rollupMock).not.toHaveBeenCalled()
    rerender({ filter: week, active: true })
    await settle()
    expect(rollupMock).toHaveBeenCalledTimes(1)
    expect(shown(result)).toBe('hierarchy#1')
  })

  it('turns a burst of store changes into one trailing read, 1 s after the last', async () => {
    const { result, storeChanged } = setup(week, true)
    await settle()
    for (let i = 0; i < 10; i++) {
      act(() => storeChanged())
      await act(() => vi.advanceTimersByTimeAsync(100))
    }
    expect(rollupMock).toHaveBeenCalledTimes(1)
    await act(() => vi.advanceTimersByTimeAsync(899))
    expect(rollupMock).toHaveBeenCalledTimes(1)
    await act(() => vi.advanceTimersByTimeAsync(1))
    expect(rollupMock).toHaveBeenCalledTimes(2)
    expect(shown(result)).toBe('hierarchy#2')
  })

  it('serves a filter seen before from its cache, without a read', async () => {
    const { result, rerender } = setup(week, true)
    await settle()
    rerender({ filter: byDay, active: true })
    await settle()
    expect(shown(result)).toBe('day#2')
    rerender({ filter: { ...week }, active: true })
    expect(shown(result)).toBe('hierarchy#1')
    await settle()
    expect(rollupMock).toHaveBeenCalledTimes(2)
  })

  it('shows a stale cached filter while it rereads', async () => {
    const { result, rerender, storeChanged } = setup(week, true)
    await settle()
    rerender({ filter: byDay, active: true })
    await settle()
    act(() => storeChanged())
    await act(() => vi.advanceTimersByTimeAsync(1_000))
    expect(rollupMock).toHaveBeenCalledTimes(3)
    rerender({ filter: week, active: true })
    expect(shown(result)).toBe('hierarchy#1')
    await settle()
    expect(rollupMock).toHaveBeenCalledTimes(4)
    expect(shown(result)).toBe('hierarchy#4')
  })

  it('rereads a filter that went stale while hidden as soon as it is shown again', async () => {
    const { result, rerender, storeChanged } = setup(week, true)
    await settle()
    rerender({ filter: week, active: false })
    act(() => storeChanged())
    await act(() => vi.advanceTimersByTimeAsync(1_000))
    expect(rollupMock).toHaveBeenCalledTimes(1)
    rerender({ filter: week, active: true })
    await settle()
    expect(rollupMock).toHaveBeenCalledTimes(2)
    expect(shown(result)).toBe('hierarchy#2')
  })

  it('keeps a newer rollup when an older read of the same filter lands after it', async () => {
    let landOld!: () => void
    rollupMock.mockImplementationOnce(
      () => new Promise((r) => (landOld = () => r(audit([node('repo', 'old')])))),
    )
    const { result, rerender, storeChanged } = setup(week, true)
    await settle()
    // Away to another filter while the first read hangs, a store change, then back: a new read.
    rerender({ filter: byDay })
    await settle()
    act(() => storeChanged())
    await act(() => vi.advanceTimersByTimeAsync(1_000))
    rerender({ filter: week })
    await settle()
    expect(shown(result)).toBe('hierarchy#4')
    await act(async () => landOld())
    expect(shown(result)).toBe('hierarchy#4')
    expect(result.current.pending).toBe(false)
    await settle()
    expect(rollupMock).toHaveBeenCalledTimes(4)
  })

  it('caches the 8 filters read most recently, rereading one dropped from it', async () => {
    const day = (d: number): AuditFilter => ({ ...week, from: `2026-10-0${d}` })
    const { rerender } = setup(day(1), true)
    await settle()
    for (let d = 2; d <= 9; d++) {
      rerender({ filter: day(d) })
      await settle()
    }
    expect(rollupMock).toHaveBeenCalledTimes(9)
    rerender({ filter: day(2) })
    await settle()
    expect(rollupMock).toHaveBeenCalledTimes(9)
    rerender({ filter: day(1) })
    await settle()
    expect(rollupMock).toHaveBeenCalledTimes(10)
  })

  describe('a reread queued behind a slow read', () => {
    /** A read in flight, with a store change queued behind it. */
    async function queued() {
      let land!: () => void
      rollupMock.mockImplementationOnce(
        (f) => new Promise((r) => (land = () => r(audit([node('repo', f.groupBy)])))),
      )
      const hook = setup(week, true)
      await settle()
      act(() => hook.storeChanged())
      await act(() => vi.advanceTimersByTimeAsync(1_000))
      return { ...hook, land: () => act(async () => land()) }
    }

    it('runs once the read lands while still active', async () => {
      const { land } = await queued()
      await land()
      await settle()
      expect(rollupMock).toHaveBeenCalledTimes(2)
    })

    it('is dropped when the view goes inactive first', async () => {
      const { rerender, land } = await queued()
      rerender({ active: false })
      await land()
      await settle()
      expect(rollupMock).toHaveBeenCalledTimes(1)
    })

    it('is dropped when the view unmounts first', async () => {
      const { unmount, land } = await queued()
      unmount()
      await land()
      await settle()
      expect(rollupMock).toHaveBeenCalledTimes(1)
    })
  })

  it('reports a failed read for its own filter only', async () => {
    rollupMock.mockRejectedValueOnce('bad day')
    const { result, rerender } = setup(week, true)
    await settle()
    expect(result.current).toEqual({ audit: null, error: 'bad day', pending: true })
    rerender({ filter: byDay, active: true })
    await settle()
    expect(result.current.error).toBeNull()
    expect(shown(result)).toBe('day#2')
  })
})
