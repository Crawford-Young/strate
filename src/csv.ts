// The Audit export: the full tree under the filter as RFC 4180 CSV, one row per node.
import type { Audit, AuditNode, AuditPath } from './api'

type Cell = string | number | boolean | null

/** Marks the file as UTF-8 for spreadsheets, which otherwise guess a legacy code page. */
const BOM = '\uFEFF'

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

interface Flat {
  node: AuditNode
  /** The labels from the top down to the node. */
  labels: string[]
  /** Its `[kind, key]` steps, which unlike labels are unique among siblings. */
  path: AuditPath
}

/** Every node under the root (not the root's own total), in tree order. */
function flatten(nodes: AuditNode[], above: Omit<Flat, 'node'> = { labels: [], path: [] }): Flat[] {
  return nodes.flatMap((node) => {
    const here: Omit<Flat, 'node'> = {
      labels: [...above.labels, node.label],
      path: [...above.path, [node.kind, node.key]],
    }
    return [{ node, ...here }, ...flatten(node.children, here)]
  })
}

/** USD to a hundredth of a cent, which also drops float noise such as 0.30000000000000004. */
const usd = (n: number) => Number(n.toFixed(4))

const METRICS: [header: string, value: (n: AuditNode) => Cell][] = [
  ['Elapsed (ms)', (n) => n.metrics.elapsedMs],
  ['Open', (n) => n.metrics.open],
  ['Active (ms)', (n) => n.metrics.activeMs],
  ['Agent-hours wall (ms)', (n) => n.metrics.agentMs],
  ['Waiting on you (ms)', (n) => n.metrics.waitMs],
  ['Cost (USD)', (n) => usd(n.metrics.costUsd)],
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
 * `audit` as CSV with CRLF line ends, after a UTF-8 BOM: the kind, one column per tree level
 * (the node's own label and its ancestors', blank below its depth), the node path as JSON
 * `[[kind, key], …]` (labels can repeat among siblings, paths can't), whether GitHub knows it,
 * then every metric in raw units (ms, USD, counts).
 */
export function toCsv(audit: Audit): string {
  const rows = flatten(audit.root.children)
  const depth = Math.max(1, ...rows.map((r) => r.labels.length))
  const levels = Array.from({ length: depth }, (_, i) => `Level ${i + 1}`)
  const header = ['Kind', ...levels, 'Path', 'GitHub', ...METRICS.map(([name]) => name)]
  const body = rows.map(({ node, labels, path }) => [
    field(node.kind),
    ...levels.map((_, i) => textField(labels[i] ?? '')),
    textField(JSON.stringify(path)),
    field(node.github),
    ...METRICS.map(([, value]) => field(value(node))),
  ])
  return BOM + [header.map(field), ...body].map((row) => `${row.join(',')}\r\n`).join('')
}
