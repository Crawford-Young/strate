// The Audit tree table: one row per node, every spec metric, sortable, expandable. Unlinked
// stays last under any sort, and a row's name selects it (by path) for the row detail (#27 U2).
import { Badge, DataTable } from '@crawfordyoung/ui'
import type { ColumnDef, Row } from '@tanstack/react-table'
import { memo, useMemo, type ReactNode } from 'react'
import type { AuditNode, AuditPath, Metrics } from './api'
import { compact } from './audit'
import { duration, money } from './format'

/** Node kinds read from GitHub: only these can lack GitHub data worth flagging. */
const FROM_GITHUB = new Set(['epic', 'issue', 'pr'])

const pathOf = (row: Row<AuditNode>): AuditPath =>
  [...row.getParentRows(), row].map((r) => [r.original.kind, r.original.key])

const samePath = (a: AuditPath | null, b: AuditPath) => JSON.stringify(a) === JSON.stringify(b)

/**
 * A row id is its node path as JSON, so expanded rows stay put when a refresh inserts or
 * reorders siblings, an id from another grouping simply matches nothing, and no key can read
 * as two steps (a slug may hold `/` or `:`).
 */
const rowId = (n: AuditNode, _index: number, parent?: Row<AuditNode>): string =>
  JSON.stringify([...(parent ? (JSON.parse(parent.id) as AuditPath) : []), [n.kind, n.key]])

const tokenTotal = ({ tokens: t }: Metrics) => t.input + t.output + t.cacheRead + t.cacheWrite

/**
 * A sortable column over a node's metrics. Unlinked's sort value is undefined and undefined
 * sorts last in either direction, which keeps Unlinked last; its cell still shows its value.
 */
function column(
  header: string,
  value: (m: Metrics, node: AuditNode) => number | string,
  cell: (m: Metrics, node: AuditNode) => ReactNode,
): ColumnDef<AuditNode> {
  return {
    id: header,
    header,
    accessorFn: (n) => (n.kind === 'unlinked' ? undefined : value(n.metrics, n)),
    sortUndefined: 'last',
    cell: ({ row }) => cell(row.original.metrics, row.original),
  }
}

/** A metric whose sort value is its shown number. */
const plain = (header: string, value: (m: Metrics) => number, show = (n: number) => String(n)) =>
  column(header, value, (m) => show(value(m)))

interface AuditTableProps {
  /** The rollup root's children. */
  nodes: AuditNode[]
  selected: AuditPath | null
  onSelect: (path: AuditPath, label: string) => void
  /** Rows can't be picked: the tree on screen is an older filter's, being replaced. */
  disabled?: boolean
}

/** Memoized: Audit rerenders on every store change, which leaves these props alone. */
export const AuditTable = memo(function AuditTable({
  nodes,
  selected,
  onSelect,
  disabled = false,
}: AuditTableProps) {
  const columns = useMemo(
    () => columnsFor(selected, onSelect, disabled),
    [selected, onSelect, disabled],
  )
  return (
    <DataTable
      columns={columns}
      data={nodes}
      getSubRows={(n) => (n.children.length ? n.children : undefined)}
      getRowId={rowId}
      getRowLabel={(n) => `${n.kind} ${n.label}`}
    />
  )
})

function columnsFor(
  selected: AuditPath | null,
  onSelect: AuditTableProps['onSelect'],
  disabled: boolean,
): ColumnDef<AuditNode>[] {
  return [
    {
      id: 'Name',
      header: 'Name',
      accessorFn: (n) => (n.kind === 'unlinked' ? undefined : n.label),
      sortUndefined: 'last',
      cell: ({ row }) => {
        const node = row.original
        const path = pathOf(row)
        return (
          <span className="inline-flex items-center gap-1.5">
            <button
              type="button"
              aria-pressed={samePath(selected, path)}
              data-path={JSON.stringify(path)}
              disabled={disabled}
              onClick={() => onSelect(path, node.label)}
              className="rounded text-left hover:underline focus:outline-none focus-visible:ring-2 focus-visible:ring-ring aria-pressed:font-semibold aria-pressed:text-accent"
            >
              {node.label}
            </button>
            {!node.github && FROM_GITHUB.has(node.kind) && (
              <Badge variant="outline" title="Known only from a window name: no GitHub data">
                no GitHub
              </Badge>
            )}
          </span>
        )
      },
    },
    column(
      'Elapsed',
      (m) => m.elapsedMs ?? -1,
      (m) => (m.elapsedMs === null ? '—' : `${duration(m.elapsedMs)}${m.open ? ' · open' : ''}`),
    ),
    plain('Active', (m) => m.activeMs, duration),
    // Subagent wall time, which the row detail's non-idle timeline does not match.
    column(
      'Agent-hours (wall)',
      (m) => m.agentMs,
      (m) =>
        m.approximate ? (
          <span title="Approximate: includes a background subagent's estimated time">
            ≈{duration(m.agentMs)}
          </span>
        ) : (
          duration(m.agentMs)
        ),
    ),
    plain('Waiting on you', (m) => m.waitMs, duration),
    column(
      '$',
      (m) => m.costUsd,
      (m) => `${money(m.costUsd)}${m.unpriced > 0 ? ` +${m.unpriced} unpriced` : ''}`,
    ),
    plain('Tokens', tokenTotal, compact),
    plain('Sessions', (m) => m.sessions),
    plain('Agents', (m) => m.agents),
    plain('Rotations', (m) => m.rotations),
    column(
      'Progress',
      (m) => (m.progress ? m.progress.completed / m.progress.total : -1),
      (m) => (m.progress ? `${m.progress.completed}/${m.progress.total}` : '—'),
    ),
  ]
}
