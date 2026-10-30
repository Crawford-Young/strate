// The Audit tree table: one row per node, its level named by a badge, every spec metric,
// sortable, expandable, and its $ or agent-hours split by day, agent type and model (#54).
// Unlinked stays last under any sort, and a row's name selects it (by path) for the row
// detail (#27 U2).
import {
  Badge,
  DataTable,
  InlineStackedBar,
  Sparkbars,
  type ChartToken,
  type InlineStackedBarSegment,
} from '@crawfordyoung/ui'
import type { ColumnDef, Row } from '@tanstack/react-table'
import { memo, useMemo, type ReactNode } from 'react'
import type { AuditNode, AuditPath, Metrics, NodeKind, Slice } from './api'
import {
  agentTypeLabel,
  agentTypePalette,
  compact,
  hoursLabel,
  measured,
  modelLabel,
  modelPalette,
  segments,
  shortDay,
  type Measure,
  type Palette,
} from './audit'
import { duration, money } from './format'

/** Node kinds read from GitHub: only these can lack GitHub data worth flagging. */
const FROM_GITHUB = new Set(['epic', 'issue', 'pr'])

/** Each row's level, on its badge; the hierarchy has no Root or Group row. */
const LEVEL: Partial<Record<NodeKind, string>> = {
  repo: 'repo',
  topic: 'topic',
  unlinked: 'no issue',
  epic: 'epic',
  issue: 'issue',
  pr: 'PR',
  session: 'session',
  agent: 'agent',
}

// Literal, so Tailwind generates each; the same classes the library's bars use.
const SWATCH: Record<ChartToken, string> = {
  1: 'bg-chart-1',
  2: 'bg-chart-2',
  3: 'bg-chart-3',
  4: 'bg-chart-4',
  5: 'bg-chart-5',
  6: 'bg-chart-6',
}

/** A split's colours over the whole tree, and how its keys read. */
interface Split {
  name: string
  palette: Palette
  label: (key: string) => string
}

const pathOf = (row: Row<AuditNode>): AuditPath =>
  [...row.getParentRows(), row].map((r) => [r.original.kind, r.original.key])

const samePath = (a: AuditPath | null, b: AuditPath) => JSON.stringify(a) === JSON.stringify(b)

/**
 * A row id is its node path as JSON, so expanded rows stay put when a refresh inserts or
 * reorders siblings, and no key can read as two steps (a slug may hold `/` or `:`).
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
  /** The rollup root: its children are the rows, and its splits pick the colours. */
  root: AuditNode
  /** The range's local days, one By day bar each. */
  days: string[]
  /** What the breakdown columns measure. */
  measure: Measure
  selected: AuditPath | null
  onSelect: (path: AuditPath, label: string) => void
  /** Rows can't be picked: the tree on screen is an older filter's, being replaced. */
  disabled?: boolean
}

/** Memoized: Audit rerenders on every store change, which leaves these props alone. */
export const AuditTable = memo(function AuditTable({
  root,
  days,
  measure,
  selected,
  onSelect,
  disabled = false,
}: AuditTableProps) {
  const splits = useMemo(
    () => ({
      agentType: {
        name: 'Agent type',
        palette: agentTypePalette(root.metrics.byAgentType),
        label: agentTypeLabel,
      },
      model: { name: 'Model', palette: modelPalette(root.metrics.byModel), label: modelLabel },
    }),
    [root],
  )
  const columns = useMemo(
    () => columnsFor({ selected, onSelect, disabled, days, measure, splits }),
    [selected, onSelect, disabled, days, measure, splits],
  )
  return (
    <div className="space-y-4">
      <div className="flex flex-wrap gap-x-8 gap-y-1.5">
        <Legend split={splits.agentType} />
        <Legend split={splits.model} />
      </div>
      <DataTable
        columns={columns}
        data={root.children}
        getSubRows={(n) => (n.children.length ? n.children : undefined)}
        getRowId={rowId}
        getRowLabel={(n) => `${n.kind} ${n.label}`}
      />
    </div>
  )
})

/**
 * A split's colour key: each named series, then Other when keys fold into it. The markup
 * of the library's chart legend, which is not exported yet (component-library#139).
 */
function Legend({ split: { name, palette, label } }: { split: Split }) {
  // Keyed as the bar segments are, so Other never clashes with a key.
  const items = [...palette.named].map(([key, color]) => ({
    id: `slice:${key}`,
    label: label(key),
    color,
  }))
  if (palette.other !== null) items.push({ id: 'other', label: 'Other', color: palette.other })
  return (
    <div className="flex flex-wrap items-center gap-x-4 gap-y-1 text-xs">
      <span className="text-muted-foreground">{name}</span>
      <ul aria-label={`${name} colours`} className="flex flex-wrap gap-x-4 gap-y-1">
        {items.map((item) => (
          <li key={item.id} className="flex items-center gap-1.5">
            <span
              aria-hidden
              className={`inline-block size-2.5 rounded-sm ${SWATCH[item.color]}`}
            />
            {item.label}
          </li>
        ))}
      </ul>
    </div>
  )
}

/** A row's split as a thin stacked bar, its biggest share named under it. */
function SplitBar({
  name,
  bars,
  format,
}: {
  name: string
  bars: InlineStackedBarSegment[]
  format: (value: number) => string
}) {
  const total = bars.reduce((sum, b) => sum + b.value, 0)
  const top = bars.reduce<InlineStackedBarSegment | null>(
    (best, b) => (best === null || b.value > best.value ? b : best),
    null,
  )
  return (
    <InlineStackedBar
      name={name}
      segments={bars}
      format={format}
      label={
        top && total > 0 ? `${top.label} ${Math.round((top.value / total) * 100)}%` : undefined
      }
    />
  )
}

interface ColumnsFor {
  selected: AuditPath | null
  onSelect: AuditTableProps['onSelect']
  disabled: boolean
  days: string[]
  measure: Measure
  splits: { agentType: Split; model: Split }
}

/** A breakdown column: not sortable, a bar per row. */
const breakdown = (header: string, cell: (n: AuditNode) => ReactNode): ColumnDef<AuditNode> => ({
  id: header,
  header,
  enableSorting: false,
  cell: ({ row }) => cell(row.original),
})

function columnsFor({
  selected,
  onSelect,
  disabled,
  days,
  measure,
  splits,
}: ColumnsFor): ColumnDef<AuditNode>[] {
  const format = measure === 'usd' ? money : hoursLabel
  const split = ({ name, palette, label }: Split, slices: Slice[]) => (
    <SplitBar
      name={`By ${name.toLowerCase()}`}
      bars={segments(slices, palette, measure, label)}
      format={format}
    />
  )
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
            <Badge variant="outline" data-testid="level" className="text-muted-foreground">
              {LEVEL[node.kind]}
            </Badge>
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
    plain('Agent-hours (wall)', (m) => m.agentMs, duration),
    plain('Waiting on you', (m) => m.waitMs, duration),
    column(
      '$',
      (m) => m.costUsd,
      (m) => `${money(m.costUsd)}${m.unpriced > 0 ? ` +${m.unpriced} unpriced` : ''}`,
    ),
    breakdown('By day', (n) => {
      const byDay = new Map(n.metrics.byDay.map((s) => [s.key, measured(s, measure)]))
      return (
        <Sparkbars
          name="By day"
          values={days.map((d) => byDay.get(d) ?? 0)}
          labels={days.map(shortDay)}
          format={format}
        />
      )
    }),
    breakdown('By agent type', (n) => split(splits.agentType, n.metrics.byAgentType)),
    breakdown('By model', (n) => split(splits.model, n.metrics.byModel)),
    plain('Tokens', tokenTotal, compact),
    plain('Sessions', (m) => m.sessions),
    plain('Agents', (m) => m.agents),
    column(
      'Progress',
      (m) => (m.progress ? m.progress.completed / m.progress.total : -1),
      (m) => (m.progress ? `${m.progress.completed}/${m.progress.total}` : '—'),
    ),
  ]
}
