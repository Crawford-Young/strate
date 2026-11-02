import { invoke } from '@tauri-apps/api/core'
import { beforeEach, describe, expect, it, vi } from 'vitest'
import {
  auditPerformance,
  auditRollup,
  auditTimeline,
  type AuditFilter,
  type AuditPath,
} from './audit-types'

vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn() }))

const invokeMock = vi.mocked(invoke)

beforeEach(() => {
  invokeMock.mockReset()
})

const filter: AuditFilter = { from: '2026-10-01', to: '2026-10-31', repos: ['demo'] }
const sentinel = { sentinel: true }

describe('audit commands', () => {
  it('auditRollup invokes audit_rollup with the filter', async () => {
    invokeMock.mockResolvedValueOnce(sentinel)
    await expect(auditRollup(filter)).resolves.toBe(sentinel)
    expect(invokeMock).toHaveBeenCalledWith('audit_rollup', { filter })
  })

  it('auditTimeline invokes audit_timeline with the filter and the path', async () => {
    const path: AuditPath = [
      ['repo', 'demo'],
      ['issue', '9'],
    ]
    invokeMock.mockResolvedValueOnce(sentinel)
    await expect(auditTimeline(filter, path)).resolves.toBe(sentinel)
    expect(invokeMock).toHaveBeenCalledWith('audit_timeline', { filter, path })
  })

  it('auditPerformance invokes audit_performance with the filter', async () => {
    invokeMock.mockResolvedValueOnce(sentinel)
    await expect(auditPerformance(filter)).resolves.toBe(sentinel)
    expect(invokeMock).toHaveBeenCalledWith('audit_performance', { filter })
  })
})
