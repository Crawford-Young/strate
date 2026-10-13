// The Audit export: the full tree under the filter as RFC 4180 CSV, one row per node.
import type { Audit, AuditNode } from './api'

type Cell = string | number | boolean | null

/** A cell's text: quoted when it holds a comma, quote or line break, quotes doubled. */
function field(cell: Cell): string {
  const text = cell === null ? '' : String(cell)
  return /[",\r\n]/.test(text) ? `"${text.replaceAll('"', '""')}"` : text
}

/**
 * A name from the store or GitHub. Spreadsheets run a cell that opens with `=`, `+`, `-`, `@`,
 * tab or CR as a formula, and an issue title is anyone's text, so those get a leading `'`.
 */
const textField = (text: string) => field(/^[=+\-@\t\r]/.test(text) ? `'${text}` : text)

/** Every node under the root in tree order, each with the labels from the top down to it. */
function flatten(nodes: AuditNode[], above: string[] = []): { node: AuditNode; path: string[] }[] {
  return nodes.flatMap((node) => {
    const path = [...above, node.label]
    return [{ node, path }, ...flatten(node.children, path)]
  })
}

const METRICS: [header: string, value: (n: AuditNode) => Cell][] = [
  ['Elapsed (ms)', (n) => n.metrics.elapsedMs],
  ['Open', (n) => n.metrics.open],
  ['Active (ms)', (n) => n.metrics.activeMs],
  ['Agent-hours wall (ms)', (n) => n.metrics.agentMs],
  ['Approximate agent-hours', (n) => n.metrics.approximate],
  ['Waiting on you (ms)', (n) => n.metrics.waitMs],
  ['Cost (USD)', (n) => n.metrics.costUsd],
  ['Unpriced requests', (n) => n.metrics.unpriced],
  ['Input tokens', (n) => n.metrics.tokens.input],
  ['Output tokens', (n) => n.metrics.tokens.output],
  ['Cache read tokens', (n) => n.metrics.tokens.cacheRead],
  ['Cache write tokens', (n) => n.metrics.tokens.cacheWrite],
  ['Sessions', (n) => n.metrics.sessions],
  ['Agents', (n) => n.metrics.agents],
  ['Rotations', (n) => n.metrics.rotations],
  ['Progress completed', (n) => n.metrics.progress?.completed ?? null],
  ['Progress total', (n) => n.metrics.progress?.total ?? null],
]

/**
 * `audit` as CSV with CRLF line ends: the kind, one column per tree level (the node's own label
 * and its ancestors', blank below its depth), whether GitHub knows it, then every metric in raw
 * units (ms, USD, counts).
 */
export function toCsv(audit: Audit): string {
  const rows = flatten(audit.root.children)
  const depth = Math.max(1, ...rows.map((r) => r.path.length))
  const levels = Array.from({ length: depth }, (_, i) => `Level ${i + 1}`)
  const header = ['Kind', ...levels, 'GitHub', ...METRICS.map(([name]) => name)]
  const body = rows.map(({ node, path }) => [
    field(node.kind),
    ...levels.map((_, i) => textField(path[i] ?? '')),
    field(node.github),
    ...METRICS.map(([, value]) => field(value(node))),
  ])
  return [header.map(field), ...body].map((row) => `${row.join(',')}\r\n`).join('')
}
