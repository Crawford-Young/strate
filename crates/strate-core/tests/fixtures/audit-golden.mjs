// audit-golden.mjs: the parity golden for strate #14. Keeps the numbers strate
// compares from claude-config's `audit.mjs --json` over cost-home/ (the full
// report also lists the machine's installed skills, which a public repo must not
// carry), and checks a live run against the committed golden.
//
// Regenerate (after a cost-home/ change), from crates/strate-core/tests:
//   bun <claude-config>/scripts/audit.mjs --root fixtures/cost-home/projects --json > audit.json
//   bun fixtures/audit-golden.mjs audit.json > fixtures/audit-golden.json
// Check (CI): node fixtures/audit-golden.mjs --check fixtures/audit-golden.json audit.json
// fails when any number differs by more than 1%, or anything else differs.

import { readFileSync } from 'node:fs'
import process from 'node:process'

const read = (file) => JSON.parse(readFileSync(file, 'utf8'))
const pick = (o, keys) => Object.fromEntries(keys.map((k) => [k, o[k]]))
const sorted = (rows, key) => [...rows].sort((a, b) => a[key].localeCompare(b[key]))

function reduce(r) {
  return {
    pricesVerified: r.pricesVerified,
    totals: r.totals,
    unpriced: r.unpriced,
    sessions: sorted(r.sessions, 'sessionId').map((s) =>
      pick(s, ['sessionId', 'requests', 'usd', 'subagentUsd']),
    ),
    time: {
      idleGapMin: r.time.idleGapMin,
      sessions: sorted(r.time.sessions, 'sessionId').map((s) =>
        pick(s, ['sessionId', 'wallMs', 'activeMs', 'idleMs', 'modelMs', 'toolsMs', 'userMs', 'otherMs']),
      ),
      agentRuns: sorted(r.time.agentRuns, 'id').map((a) => pick(a, ['id', 'sessionId', 'wallMs', 'usd'])),
    },
  }
}

function diff(want, got, path, out) {
  if (typeof want === 'number' && typeof got === 'number') {
    if (Math.abs(want - got) > 0.01 * Math.max(Math.abs(want), Math.abs(got))) out.push(`${path}: ${want} -> ${got}`)
  } else if (want && got && typeof want === 'object' && typeof got === 'object') {
    for (const k of new Set([...Object.keys(want), ...Object.keys(got)])) diff(want[k], got[k], `${path}.${k}`, out)
  } else if (want !== got) {
    out.push(`${path}: ${JSON.stringify(want)} -> ${JSON.stringify(got)}`)
  }
  return out
}

const args = process.argv.slice(2)
if (args[0] === '--check') {
  const out = diff(read(args[1]), reduce(read(args[2])), 'golden', [])
  if (out.length) {
    process.stderr.write(`audit.mjs disagrees with the golden beyond 1%:\n${out.join('\n')}\n`)
    process.exit(1)
  }
  process.stdout.write('audit.mjs matches the golden within 1%\n')
} else {
  process.stdout.write(`${JSON.stringify(reduce(read(args[0])), null, 2)}\n`)
}
