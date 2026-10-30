import { describe, expect, it } from 'vitest'
import { audit, node } from '../tests/audit-fixtures'
import { toCsv } from './csv'

const BOM = '\uFEFF'

/**
 * The CSV's rows after its BOM, split on CRLF with the trailing break dropped (a quoted break
 * stays in its cell).
 */
const lines = (csv: string) => csv.slice(BOM.length).split('\r\n').slice(0, -1)

// Core keys an epic, issue or PR by its number, and a repo by its slug.
const tree = audit([
  node('repo', 'acme/web', { costUsd: 6.5, unpriced: 2, agentMs: 3_600_000 }, [
    node(
      'epic',
      '10',
      {
        costUsd: 4,
        elapsedMs: 7_200_000,
        open: true,
        progress: { completed: 1, total: 2 },
        tokens: { input: 1, output: 2, cacheRead: 3, cacheWrite: 4 },
      },
      [node('issue', '11', { costUsd: 4 }, [], { label: '#11 Fix', github: true })],
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
  'Path',
  'GitHub',
  'Elapsed (ms)',
  'Open',
  'Active (ms)',
  'Agent-hours wall (ms)',
  'Waiting on you (ms)',
  'Cost (USD)',
  'Unpriced requests',
  'Input tokens',
  'Output tokens',
  'Cache read tokens',
  'Cache write tokens',
  'Sessions',
  'Agents',
  'Progress completed',
  'Progress total',
]

/** A row's cells; the tests that use it hold no quoted commas. */
const cells = (row: string | undefined) => (row as string).split(',')

describe('toCsv', () => {
  it('has a header, then one CRLF-ended row per node of the whole tree, parents first', () => {
    const csv = toCsv(tree)
    expect(csv.endsWith('\r\n')).toBe(true)
    const rows = lines(csv)
    expect(rows[0]).toBe(HEADER.join(','))
    expect(rows.slice(1).map((r) => r.split(',').slice(0, 4))).toEqual([
      ['repo', 'acme/web', '', ''],
      ['epic', 'acme/web', '#10 Epic', ''],
      ['issue', 'acme/web', '#10 Epic', '#11 Fix'],
      ['unlinked', 'Unlinked', '', ''],
    ])
  })

  it('opens with a UTF-8 BOM, so a spreadsheet reads non-ASCII names right', () => {
    expect(toCsv(tree).startsWith(BOM)).toBe(true)
  })

  it('has no row for the root total, only the nodes under it', () => {
    const rooted = { ...tree, root: { ...tree.root, label: 'All work' } }
    const rows = lines(toCsv(rooted))
    expect(rows).toHaveLength(5)
    expect(rows.some((r) => r.startsWith('root,') || r.includes('All work'))).toBe(false)
  })

  it('keys each row by its node path, so same-named siblings stay apart', () => {
    const csv = toCsv(
      audit([
        node('repo', 'acme/web', {}, [
          node('session', 's-1', {}, [], { label: 'acme-web-3' }),
          node('session', 's-2', {}, [], { label: 'acme-web-3' }),
        ]),
      ]),
    )
    expect(csv).toContain(',"[[""repo"",""acme/web""],[""session"",""s-1""]]",')
    expect(csv).toContain(',"[[""repo"",""acme/web""],[""session"",""s-2""]]",')
  })

  it('writes every metric as a number, an unknown one empty', () => {
    const rows = lines(toCsv(tree))
    // The Path cell is quoted JSON with commas, so read the metrics from the row's end.
    const at = (row: string | undefined, name: string) =>
      cells(row).slice(-(HEADER.length - HEADER.indexOf(name)))[0]
    const [, web, epic] = rows
    expect(at(epic, 'Elapsed (ms)')).toBe('7200000')
    expect(at(epic, 'Open')).toBe('true')
    expect(at(epic, 'GitHub')).toBe('true')
    expect(at(epic, 'Cost (USD)')).toBe('4')
    expect(at(epic, 'Input tokens')).toBe('1')
    expect(at(epic, 'Output tokens')).toBe('2')
    expect(at(epic, 'Cache read tokens')).toBe('3')
    expect(at(epic, 'Cache write tokens')).toBe('4')
    expect(at(epic, 'Progress completed')).toBe('1')
    expect(at(epic, 'Progress total')).toBe('2')
    expect(at(web, 'GitHub')).toBe('false')
    expect(at(web, 'Elapsed (ms)')).toBe('')
    expect(at(web, 'Progress total')).toBe('')
    expect(at(web, 'Agent-hours wall (ms)')).toBe('3600000')
    expect(at(web, 'Unpriced requests')).toBe('2')
  })

  it('rounds $ to a hundredth of a cent, without float noise', () => {
    const rows = lines(
      toCsv(
        audit([node('repo', 'a', { costUsd: 0.1 + 0.2 }), node('repo', 'b', { costUsd: 1 / 3 })]),
      ),
    )
    const cost = (row: string | undefined) =>
      cells(row).slice(-(HEADER.length - HEADER.indexOf('Cost (USD)')))[0]
    expect(cost(rows[1])).toBe('0.3')
    expect(cost(rows[2])).toBe('0.3333')
  })

  it('quotes cells that hold a comma, a quote or a line break, doubling quotes (RFC 4180)', () => {
    const csv = toCsv(
      audit([
        node('issue', '1', {}, [], { label: 'Fix "x", then\nretry' }),
        node('issue', '2', {}, [], { label: 'plain' }),
      ]),
    )
    expect(csv).toContain('issue,"Fix ""x"", then\nretry",')
    expect(csv).toContain('issue,plain,')
  })

  it('keeps a title that opens like a spreadsheet formula from running as one', () => {
    expect(
      toCsv(audit([node('issue', '1', {}, [], { label: '=HYPERLINK("http://x")' })])),
    ).toContain(`issue,"'=HYPERLINK(""http://x"")",`)
    expect(toCsv(audit([node('issue', '1', {}, [], { label: '-1 off' })]))).toContain(
      "issue,'-1 off,",
    )
  })

  it('is only a header, with one path column, for an empty rollup', () => {
    const rows = lines(toCsv(audit([])))
    expect(rows).toHaveLength(1)
    expect(rows[0]?.startsWith('Kind,Level 1,Path,GitHub,')).toBe(true)
  })
})
