import { describe, expect, it } from 'vitest'
import { audit, node, slice } from '../tests/audit-fixtures'
import {
  agentTypeLabel,
  agentTypePalette,
  compact,
  dailyChart,
  dayRange,
  hasPath,
  hoursLabel,
  lastDays,
  localDay,
  measured,
  modelLabel,
  modelPalette,
  repoChips,
  segments,
  shortDay,
  UNLINKED,
} from './audit'

const HOUR = 3_600_000

describe('local days', () => {
  it('formats a date as its local YYYY-MM-DD', () => {
    expect(localDay(new Date(2026, 9, 5, 23, 59))).toBe('2026-10-05')
    expect(localDay(new Date(2026, 0, 1, 0, 0))).toBe('2026-01-01')
  })

  it('spans the last n local days, today included', () => {
    expect(lastDays(7, new Date(2026, 9, 13, 10))).toEqual({ from: '2026-10-07', to: '2026-10-13' })
    expect(lastDays(14, new Date(2026, 2, 3, 1))).toEqual({ from: '2026-02-18', to: '2026-03-03' })
    expect(lastDays(1, new Date(2026, 9, 13))).toEqual({ from: '2026-10-13', to: '2026-10-13' })
  })

  it('labels a day by its local date', () => {
    const want = new Intl.DateTimeFormat(undefined, { month: 'short', day: 'numeric' }).format(
      new Date(2026, 9, 5),
    )
    expect(shortDay('2026-10-05')).toBe(want)
  })

  it('lists every day from..=to, across a month end', () => {
    expect(dayRange('2026-09-29', '2026-10-02')).toEqual([
      '2026-09-29',
      '2026-09-30',
      '2026-10-01',
      '2026-10-02',
    ])
    expect(dayRange('2026-10-02', '2026-10-02')).toEqual(['2026-10-02'])
  })
})

describe('node paths', () => {
  const tree = audit([
    node('repo', 'acme/web', {}, [node('issue', '11')]),
    node('repo', 'acme/api'),
  ])

  it('finds a path step by step from the root', () => {
    expect(hasPath(tree.root, [['repo', 'acme/web']])).toBe(true)
    expect(
      hasPath(tree.root, [
        ['repo', 'acme/web'],
        ['issue', '11'],
      ]),
    ).toBe(true)
  })

  it('misses a path when any step is gone, or matches on kind or key alone', () => {
    expect(
      hasPath(tree.root, [
        ['repo', 'acme/api'],
        ['issue', '11'],
      ]),
    ).toBe(false)
    expect(hasPath(tree.root, [['epic', 'acme/web']])).toBe(false)
    expect(hasPath(tree.root, [['repo', 'acme/x']])).toBe(false)
  })
})

describe('numbers', () => {
  it('shortens counts to k and M', () => {
    expect(compact(0)).toBe('0')
    expect(compact(950)).toBe('950')
    expect(compact(1_234)).toBe('1.2k')
    expect(compact(3_400_000)).toBe('3.4M')
  })

  it('shows hours to one decimal', () => {
    expect(hoursLabel(0)).toBe('0h')
    expect(hoursLabel(1.25)).toBe('1.3h')
    expect(hoursLabel(12)).toBe('12h')
  })
})

describe('the daily chart', () => {
  // A tree's top rows, as the core sorts them: repos by slug, then Unlinked. Each carries its
  // $ and agent-hours per day.
  const tree = audit([
    node('repo', 'acme/api', {
      byDay: [slice('2026-10-07', 1, 3 * HOUR)],
    }),
    node('repo', 'acme/web', {
      byDay: [slice('2026-10-05', 2, 2 * HOUR), slice('2026-10-07', 4, HOUR)],
    }),
    node('unlinked', '', { byDay: [slice('2026-10-05', 0.5, HOUR / 2)] }, [], {
      label: 'Unlinked',
    }),
  ])

  it('stacks one series per top row of the tree, in its order, a row per day in range', () => {
    const chart = dailyChart(tree, 'usd', '2026-10-05', '2026-10-07')
    expect(chart.series.map((s) => s.label)).toEqual(['acme/api', 'acme/web', 'Unlinked'])
    const [api, web, unlinked] = chart.series.map((s) => s.key)
    expect(chart.data).toEqual([
      { day: '2026-10-05', [web as string]: 2, [unlinked as string]: 0.5 },
      { day: '2026-10-06' },
      { day: '2026-10-07', [api as string]: 1, [web as string]: 4 },
    ])
  })

  it('uses series keys a chart reads as plain fields, whatever the slug', () => {
    const chart = dailyChart(tree, 'usd', '2026-10-05', '2026-10-05')
    for (const s of chart.series) expect(s.key).toMatch(/^[a-z0-9]+$/)
  })

  it('charts agent-hours (wall) on the hours measure', () => {
    const chart = dailyChart(tree, 'hours', '2026-10-07', '2026-10-07')
    const [api, web] = chart.series.map((s) => s.key)
    expect(chart.data).toEqual([{ day: '2026-10-07', [api as string]: 3, [web as string]: 1 }])
  })
})

describe('repo chips', () => {
  it('lists the repos of the read, sorted, then Unlinked when the range has any', () => {
    const read = audit([], { repos: ['acme/api', 'acme/web'], unlinked: true })
    expect(repoChips(read, [])).toEqual(['acme/api', 'acme/web', UNLINKED])
    expect(repoChips(audit([], { repos: ['acme/web'] }), [])).toEqual(['acme/web'])
  })

  it('keeps a picked repo the range no longer holds, so it can be unpicked', () => {
    const read = audit([], { repos: ['acme/web'] })
    expect(repoChips(read, ['acme/old', UNLINKED])).toEqual(['acme/old', 'acme/web', UNLINKED])
  })

  it('lists only the picked ones before the first read lands', () => {
    expect(repoChips(null, [])).toEqual([])
    expect(repoChips(null, ['acme/web'])).toEqual(['acme/web'])
  })
})

describe('breakdown colours', () => {
  it('pins the orchestrator to --chart-6, the other agent types by $ to 1..4', () => {
    const p = agentTypePalette([
      slice('implementer', 5),
      slice('orchestrator', 9),
      slice('recon', 1),
      slice('reviewer', 2),
    ])
    expect([...p.named]).toEqual([
      ['orchestrator', 6],
      ['implementer', 1],
      ['reviewer', 2],
      ['recon', 3],
    ])
    expect(p.other).toBeNull()
  })

  it('gives the models by $ 4, 3, 2, 1 then 6', () => {
    const p = modelPalette([slice('model-a', 1), slice('model-b', 3), slice('model-c', 2)])
    expect([...p.named]).toEqual([
      ['model-b', 4],
      ['model-c', 3],
      ['model-a', 2],
    ])
  })

  it('keeps --chart-5 for Other in both splits, so no named series shares its colour', () => {
    const keys = ['a', 'b', 'c', 'd', 'e', 'f', 'g'].map((k, i) => slice(k, 10 - i))
    for (const p of [agentTypePalette(keys), modelPalette(keys)]) {
      expect(p.other).toBe(5)
      expect([...p.named.values()]).not.toContain(5)
    }
    expect([...modelPalette(keys).named.values()]).toEqual([4, 3, 2, 1, 6])
  })

  it('breaks a $ tie by key, so the colours never swap between reads', () => {
    const p = modelPalette([slice('b', 1), slice('a', 1)])
    expect([...p.named.keys()]).toEqual(['a', 'b'])
  })

  it('folds every key past the free colours into Other', () => {
    const types = ['a', 'b', 'c', 'd', 'e', 'f'].map((k, i) => slice(k, 10 - i))
    const p = agentTypePalette(types)
    expect([...p.named]).toEqual([
      ['a', 1],
      ['b', 2],
      ['c', 3],
      ['d', 4],
    ])
    expect(p.other).toBe(5)
    // Four agent types besides the orchestrator fit without folding.
    expect(agentTypePalette(types.slice(0, 4)).other).toBeNull()
    expect(agentTypePalette(types.slice(0, 5)).other).toBe(5)
  })

  // Segment keys can never clash: Other's is no slice key, and '' is one (Unknown, No requests).
  it('draws the slices of a row in legend order, Other last, on the chosen measure', () => {
    const p = agentTypePalette(['a', 'b', 'c', 'd', 'e', 'f'].map((k, i) => slice(k, 10 - i)))
    const row = [slice('f', 1, HOUR), slice('b', 2, 2 * HOUR), slice('e', 3), slice('x', 4)]
    expect(segments(row, p, 'usd', agentTypeLabel)).toEqual([
      { key: 'slice:b', label: 'b', value: 2, color: 2 },
      { key: 'other', label: 'Other', value: 8, color: 5 },
    ])
    expect(segments(row, p, 'hours', agentTypeLabel)).toEqual([
      { key: 'slice:b', label: 'b', value: 2, color: 2 },
      { key: 'other', label: 'Other', value: 1, color: 5 },
    ])
  })

  it('names the empty keys: an unknown agent type, a cell with no requests', () => {
    expect(agentTypeLabel('')).toBe('Unknown')
    expect(agentTypeLabel('recon')).toBe('recon')
    expect(modelLabel('')).toBe('No requests')
    expect(modelLabel('model-a')).toBe('model-a')
  })

  it('measures $, or agent-hours in hours', () => {
    expect(measured(slice('a', 2.5, 90 * 60_000), 'usd')).toBe(2.5)
    expect(measured(slice('a', 2.5, 90 * 60_000), 'hours')).toBe(1.5)
  })
})
