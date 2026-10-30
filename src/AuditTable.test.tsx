import { fireEvent, render, screen, within } from '@testing-library/react'
import type { ComponentProps } from 'react'
import { describe, expect, it } from 'vitest'
import { node, slice } from '../tests/audit-fixtures'
import type { AuditNode } from './api'
import { shortDay } from './audit'
import { AuditTable } from './AuditTable'

const HOUR = 3_600_000

const web = node('repo', 'acme/web', {}, [node('issue', '1', {}, [], { label: '#1 Fix' })])
const api = node('repo', 'acme/api', {}, [node('issue', '2', {}, [], { label: '#2 Add' })])

const root = (children: AuditNode[], over: Parameters<typeof node>[2] = {}) =>
  node('root', '', over, children)

/** The table over `children`, on the $ measure over Oct 5..7 unless `props` say otherwise. */
const table = (children: AuditNode[], props: Partial<ComponentProps<typeof AuditTable>> = {}) => (
  <AuditTable
    root={root(children)}
    days={['2026-10-05', '2026-10-06', '2026-10-07']}
    measure="usd"
    selected={null}
    onSelect={() => {}}
    {...props}
  />
)

const shown = () => screen.getAllByRole('button', { name: /^(acme|#)/ }).map((b) => b.textContent)
/** The table row holding the name button `name`. */
const row = (name: string) => screen.getByRole('button', { name }).closest('tr') as HTMLElement

describe('AuditTable', () => {
  it('keeps a row expanded when a refresh inserts a sibling before it', () => {
    const { rerender } = render(table([web]))
    fireEvent.click(screen.getByRole('button', { name: 'Expand repo acme/web' }))
    expect(shown()).toEqual(['acme/web', '#1 Fix'])
    rerender(table([api, web]))
    expect(shown()).toEqual(['acme/api', 'acme/web', '#1 Fix'])
  })

  it('keeps rows apart whose keys would join into the same id', () => {
    // A repo `a` with issue `b`, and a repo whose slug reads like that pair joined.
    const x = node('repo', 'a', {}, [
      node('issue', 'b', {}, [node('session', 's', {}, [], { label: 'S1' })], { label: 'b' }),
    ])
    const y = node('repo', 'a/issue:b', {}, [node('session', 't', {}, [], { label: 'T1' })])
    render(table([x, y]))
    fireEvent.click(screen.getByRole('button', { name: 'Expand repo a/issue:b' }))
    fireEvent.click(screen.getByRole('button', { name: 'Expand repo a' }))
    expect(screen.getByRole('button', { name: 'Expand issue b' })).toBeInTheDocument()
    expect(screen.queryByRole('button', { name: 'S1' })).toBeNull()
  })

  it('disables picking a row while the rollup on screen is being replaced', () => {
    const picked: unknown[] = []
    render(table([web], { onSelect: (p) => picked.push(p), disabled: true }))
    const name = screen.getByRole('button', { name: 'acme/web' })
    expect(name).toBeDisabled()
    fireEvent.click(name)
    expect(picked).toEqual([])
  })

  it('passes the path and label of the row picked', () => {
    const picked: unknown[] = []
    render(table([web], { onSelect: (...args) => picked.push(args) }))
    fireEvent.click(screen.getByRole('button', { name: 'acme/web' }))
    expect(picked).toEqual([[[['repo', 'acme/web']], 'acme/web']])
  })
})

describe('level badges', () => {
  it('names the level of every row, Unlinked as no issue, with no › chain anywhere', () => {
    const chain = node('repo', 'acme/web', {}, [
      node(
        'epic',
        '10',
        {},
        [
          node(
            'issue',
            '11',
            {},
            [
              node(
                'pr',
                '12',
                {},
                [
                  node('session', 's', {}, [node('agent', '', {}, [], { label: 'orchestrator' })], {
                    label: 'acme-11',
                  }),
                ],
                { label: 'PR #12' },
              ),
            ],
            { label: '#11 Fix' },
          ),
        ],
        { label: '#10 Epic' },
      ),
    ])
    const unlinked = node('unlinked', '', {}, [], { label: 'Unlinked' })
    const topic = node('topic', 'docs', {}, [], { label: 'docs' })
    render(table([chain, topic, unlinked]))
    fireEvent.click(screen.getByRole('button', { name: 'Expand all' }))
    const level = (name: string) => within(row(name)).getByTestId('level').textContent
    expect(level('acme/web')).toBe('repo')
    expect(level('#10 Epic')).toBe('epic')
    expect(level('#11 Fix')).toBe('issue')
    expect(level('PR #12')).toBe('PR')
    expect(level('acme-11')).toBe('session')
    expect(level('orchestrator')).toBe('agent')
    expect(level('docs')).toBe('topic')
    expect(level('Unlinked')).toBe('no issue')
    expect(document.body.textContent).not.toContain('›')
  })
})

describe('breakdown columns', () => {
  // Oct 5 and 7 have work; the orchestrator and an implementer on two models.
  const busy = node('repo', 'acme/web', {
    costUsd: 3,
    agentMs: 2 * HOUR,
    byDay: [slice('2026-10-05', 1, HOUR), slice('2026-10-07', 2, HOUR)],
    byAgentType: [slice('implementer', 2, 2 * HOUR), slice('orchestrator', 1)],
    byModel: [slice('model-a', 1), slice('model-b', 2, 2 * HOUR)],
  })
  // Only the implementer: its one segment keeps the implementer's colour.
  const solo = node('repo', 'acme/api', {
    costUsd: 1,
    byAgentType: [slice('implementer', 1)],
    byModel: [slice('model-b', 1)],
  })
  const tree = root([busy, solo], {
    byAgentType: [slice('implementer', 3, 2 * HOUR), slice('orchestrator', 1)],
    byModel: [slice('model-a', 1), slice('model-b', 3, 2 * HOUR)],
  })
  const days = ['2026-10-05', '2026-10-06', '2026-10-07']
  const [d5, d6, d7] = days.map(shortDay)

  it('come after $, unsortable, before Tokens', () => {
    render(table([busy]))
    const headers = screen.getAllByRole('columnheader').map((h) => h.textContent)
    const at = headers.indexOf('$')
    expect(headers.slice(at, at + 5)).toEqual([
      '$',
      'By day',
      'By agent type',
      'By model',
      'Tokens',
    ])
    for (const name of ['By day', 'By agent type', 'By model'])
      expect(within(screen.getByRole('columnheader', { name })).queryByRole('button')).toBeNull()
  })

  it('show $ per day of the range, a quiet day as $0', () => {
    render(table([busy], { root: tree }))
    expect(within(row('acme/web')).getByRole('img', { name: /^By day/ })).toHaveAccessibleName(
      `By day: ${d5} $1.00, ${d6} $0.00, ${d7} $2.00`,
    )
  })

  it('split $ by agent type and model, naming the biggest share under the bar', () => {
    render(table([busy], { root: tree }))
    const r = within(row('acme/web'))
    expect(r.getByRole('img', { name: /^By agent type/ })).toHaveAccessibleName(
      'By agent type: orchestrator $1.00 (33%), implementer $2.00 (67%)',
    )
    expect(r.getByRole('img', { name: /^By model/ })).toHaveAccessibleName(
      'By model: model-b $2.00 (67%), model-a $1.00 (33%)',
    )
    expect(r.getByText('implementer 67%')).toBeInTheDocument()
    expect(r.getByText('model-b 67%')).toBeInTheDocument()
  })

  it('switch to agent-hours on the hours measure', () => {
    render(table([busy], { root: tree, measure: 'hours' }))
    const r = within(row('acme/web'))
    expect(r.getByRole('img', { name: /^By day/ })).toHaveAccessibleName(
      `By day: ${d5} 1h, ${d6} 0h, ${d7} 1h`,
    )
    expect(r.getByRole('img', { name: /^By agent type/ })).toHaveAccessibleName(
      'By agent type: implementer 2h (100%)',
    )
    expect(r.getByText('implementer 100%')).toBeInTheDocument()
  })

  it('keep a series in one colour on every row, picked over the whole tree', () => {
    render(table([busy, solo], { root: tree }))
    const colours = (name: string, breakdown: string) =>
      [
        ...within(row(name))
          .getByRole('img', { name: new RegExp(`^${breakdown}`) })
          .querySelectorAll('[data-segment]'),
      ].map((s) => s.className.match(/bg-chart-\d/)?.[0])
    expect(colours('acme/web', 'By agent type')).toEqual(['bg-chart-6', 'bg-chart-1'])
    expect(colours('acme/api', 'By agent type')).toEqual(['bg-chart-1'])
    expect(colours('acme/web', 'By model')).toEqual(['bg-chart-4', 'bg-chart-3'])
    expect(colours('acme/api', 'By model')).toEqual(['bg-chart-4'])
  })

  it('name no biggest share on a row with nothing to split', () => {
    const idle = node('repo', 'acme/idle')
    render(table([idle], { root: root([idle], { byAgentType: tree.metrics.byAgentType }) }))
    expect(within(row('acme/idle')).queryByText(/%$/)).toBeNull()
  })

  it('are keyed by colour above the tree, for agent types and models', () => {
    render(table([busy, solo], { root: tree }))
    const legend = (name: string) =>
      within(screen.getByRole('list', { name }))
        .getAllByRole('listitem')
        .map((li) => ({
          label: li.textContent,
          swatch: li.querySelector('span')?.className.match(/bg-chart-\d/)?.[0],
        }))
    expect(legend('Agent type colours')).toEqual([
      { label: 'orchestrator', swatch: 'bg-chart-6' },
      { label: 'implementer', swatch: 'bg-chart-1' },
    ])
    expect(legend('Model colours')).toEqual([
      { label: 'model-b', swatch: 'bg-chart-4' },
      { label: 'model-a', swatch: 'bg-chart-3' },
    ])
  })

  it('key the folded series as Other in the legend', () => {
    const types = ['a', 'b', 'c', 'd', 'e', 'f'].map((k, i) => slice(k, 10 - i))
    render(table([busy], { root: root([busy], { byAgentType: types }) }))
    const items = within(screen.getByRole('list', { name: 'Agent type colours' }))
      .getAllByRole('listitem')
      .map((li) => li.textContent)
    expect(items).toEqual(['a', 'b', 'c', 'd', 'Other'])
  })
})
