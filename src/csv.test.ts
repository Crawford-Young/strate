import { describe, expect, it } from 'vitest'
import { audit, node } from '../tests/audit-fixtures'
import { toCsv } from './csv'

/** The CSV's rows, split on CRLF with the trailing break dropped (a quoted break stays in its cell). */
const lines = (csv: string) => csv.split('\r\n').slice(0, -1)

const tree = audit([
  node('repo', 'acme/web', { costUsd: 6.5, unpriced: 2, agentMs: 3_600_000, approximate: true }, [
    node(
      'epic',
      'acme/web#10',
      {
        costUsd: 4,
        elapsedMs: 7_200_000,
        open: true,
        progress: { completed: 1, total: 2 },
        tokens: { input: 1, output: 2, cacheRead: 3, cacheWrite: 4 },
      },
      [node('issue', 'acme/web#11', { costUsd: 4 }, [], { label: '#11 Fix', github: true })],
      { label: '#10 Epic', github: true },
    ),
  ]),
  node('unlinked', 'unlinked', { costUsd: 9 }, [], { label: 'Unlinked' }),
])

const HEADER = [
  'Kind',
  'Level 1',
  'Level 2',
  'Level 3',
  'GitHub',
  'Elapsed (ms)',
  'Open',
  'Active (ms)',
  'Agent-hours wall (ms)',
  'Approximate agent-hours',
  'Waiting on you (ms)',
  'Cost (USD)',
  'Unpriced requests',
  'Input tokens',
  'Output tokens',
  'Cache read tokens',
  'Cache write tokens',
  'Sessions',
  'Agents',
  'Rotations',
  'Progress completed',
  'Progress total',
]

describe('toCsv', () => {
  it('has a header, then one CRLF-ended row per node of the whole tree, parents first', () => {
    const csv = toCsv(tree)
    expect(csv.endsWith('\r\n')).toBe(true)
    const rows = lines(csv)
    expect(rows[0]).toBe(HEADER.join(','))
    expect(rows.slice(1).map((r) => r.split(',').slice(0, 5))).toEqual([
      ['repo', 'acme/web', '', '', 'false'],
      ['epic', 'acme/web', '#10 Epic', '', 'true'],
      ['issue', 'acme/web', '#10 Epic', '#11 Fix', 'true'],
      ['unlinked', 'Unlinked', '', '', 'false'],
    ])
  })

  it('writes every metric as a number, an unknown one empty', () => {
    const [, web, epic] = lines(toCsv(tree)).map((r) => r.split(','))
    const at = (cells: string[] | undefined, name: string) =>
      (cells as string[])[HEADER.indexOf(name)]
    expect(epic).toHaveLength(HEADER.length)
    expect(at(epic, 'Elapsed (ms)')).toBe('7200000')
    expect(at(epic, 'Open')).toBe('true')
    expect(at(epic, 'Cost (USD)')).toBe('4')
    expect(at(epic, 'Input tokens')).toBe('1')
    expect(at(epic, 'Output tokens')).toBe('2')
    expect(at(epic, 'Cache read tokens')).toBe('3')
    expect(at(epic, 'Cache write tokens')).toBe('4')
    expect(at(epic, 'Progress completed')).toBe('1')
    expect(at(epic, 'Progress total')).toBe('2')
    expect(at(web, 'Elapsed (ms)')).toBe('')
    expect(at(web, 'Progress total')).toBe('')
    expect(at(web, 'Approximate agent-hours')).toBe('true')
    expect(at(web, 'Agent-hours wall (ms)')).toBe('3600000')
    expect(at(web, 'Unpriced requests')).toBe('2')
  })

  it('quotes cells that hold a comma, a quote or a line break, doubling quotes (RFC 4180)', () => {
    const csv = toCsv(
      audit([
        node('issue', 'a#1', {}, [], { label: 'Fix "x", then\nretry' }),
        node('issue', 'a#2', {}, [], { label: 'plain' }),
      ]),
    )
    expect(csv).toContain('issue,"Fix ""x"", then\nretry",')
    expect(csv).toContain('issue,plain,')
  })

  it('keeps a title that opens like a spreadsheet formula from running as one', () => {
    expect(
      toCsv(audit([node('issue', 'a#1', {}, [], { label: '=HYPERLINK("http://x")' })])),
    ).toContain(`issue,"'=HYPERLINK(""http://x"")",`)
    expect(toCsv(audit([node('issue', 'a#1', {}, [], { label: '-1 off' })]))).toContain(
      "issue,'-1 off,",
    )
  })

  it('is only a header, with one path column, for an empty rollup', () => {
    const rows = lines(toCsv(audit([])))
    expect(rows).toHaveLength(1)
    expect(rows[0]?.startsWith('Kind,Level 1,GitHub,')).toBe(true)
  })
})
