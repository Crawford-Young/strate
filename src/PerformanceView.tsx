// The Audit performance view (#51), over the spend chart and tree: Outcome (is the harness
// getting better), Agents & models (routing), Bottleneck (your own time in the loop) and Waste &
// rework (what to change in the harness), in the approved mockup's order and copy.
import {
  Badge,
  DataTable,
  Progress,
  StackedBarChart,
  StatTile,
  TrendChart,
  type StatTileDelta,
} from '@crawfordyoung/ui'
import type { ColumnDef } from '@tanstack/react-table'
import { memo, type ReactNode } from 'react'
import type {
  AgentRow,
  Bottleneck,
  Headline,
  Outcome,
  OutcomeWeek,
  Performance,
  Waste,
} from './api'
import { hoursLabel, shortDay } from './audit'
import { duration, money } from './format'
import {
  bottleneckChart,
  byRepo,
  change,
  hours,
  rangeMean,
  share,
  successPct,
  totals,
  trendRows,
  weekLabel,
  type RepoSums,
} from './performance'

interface PerformanceViewProps {
  /** The rollup's: its median elapsed and cloud note only, the rest coming from the weeks. */
  headline: Headline
  performance: Performance
}

/** Memoized: Audit rerenders on every store change, which leaves both reads alone. */
export const PerformanceView = memo(function PerformanceView({
  headline,
  performance: p,
}: PerformanceViewProps) {
  return (
    <>
      <OutcomeSection headline={headline} outcome={p.outcome} />
      <AgentsSection agents={p.agents} />
      <BottleneckSection bottleneck={p.bottleneck} />
      <WasteSection waste={p.waste} />
    </>
  )
})

/** A view section: its heading, then the question it answers. */
export function Section({
  title,
  sub,
  children,
}: {
  title: string
  sub: ReactNode
  children: ReactNode
}) {
  return (
    <section aria-label={title} className="space-y-3">
      <div className="flex flex-wrap items-baseline gap-x-3 gap-y-1">
        <h2 className="text-sm font-semibold">{title}</h2>
        <p className="text-xs text-muted-foreground">{sub}</p>
      </div>
      {children}
    </section>
  )
}

function Panel({ title, children }: { title?: string; children: ReactNode }) {
  return (
    <section
      aria-label={title}
      className="min-w-0 overflow-x-auto rounded-lg border border-border bg-surface px-3.5 py-3"
    >
      {title && <h3 className="mb-2 text-xs font-medium text-muted-foreground">{title}</h3>}
      {children}
    </section>
  )
}

const Tiles = ({ children }: { children: ReactNode }) => (
  <div className="grid grid-cols-[repeat(auto-fit,minmax(170px,1fr))] gap-2.5">{children}</div>
)

const named = (s: string) => s || 'unknown'
const percent = (n: number | null) => (n === null ? '—' : `${n}%`)
const perIssueHours = (ms: number | null) => (ms === null ? '—' : hours(ms, 2))

/** A range-wide mean over the weeks, as the hint: `range: $8.22 over 28`. */
function rangeHint(
  weeks: OutcomeWeek[],
  value: (w: OutcomeWeek) => number | null,
  count: (w: OutcomeWeek) => number,
  show: (n: number) => string,
  unit = '',
) {
  const r = rangeMean(weeks, value, count)
  return r === null ? 'range: none' : `range: ${show(r.mean)} over ${r.count}${unit}`
}

/** A delta from the week before, or none in a range of one week. */
function versus(
  now: number | null,
  was: OutcomeWeek | undefined,
  pick: (w: OutcomeWeek) => number | null,
  show: (n: number | null) => string,
  goodWhen: StatTileDelta['goodWhen'],
): StatTileDelta | undefined {
  if (was === undefined) return undefined
  return { change: change(now, pick(was)), goodWhen, label: `vs ${show(pick(was))}` }
}

function OutcomeSection({ headline: h, outcome: o }: { headline: Headline; outcome: Outcome }) {
  const now = o.weeks.at(-1)
  const was = o.weeks.at(-2)
  const perIssue = now?.usdPerClosedIssue ?? null
  const perIssueMs = now?.activeMsPerClosedIssue ?? null
  const perPr = now?.usdPerMergedPr ?? null
  const closed = now?.issuesClosed ?? 0
  return (
    <Section
      title="Outcome"
      sub={
        <>
          Is the harness getting better? Each closed issue costs all its{' '}
          <code>&lt;repo&gt;-&lt;issue&gt;</code> work, counted in the week it closed.
        </>
      }
    >
      <Tiles>
        <StatTile
          label="$ per closed issue"
          value={money(perIssue)}
          delta={versus(perIssue, was, (w) => w.usdPerClosedIssue, money, 'down')}
          hint={rangeHint(
            o.weeks,
            (w) => w.usdPerClosedIssue,
            (w) => w.issuesClosed,
            money,
          )}
        />
        <StatTile
          label="Hours per closed issue"
          value={perIssueHours(perIssueMs)}
          delta={versus(perIssueMs, was, (w) => w.activeMsPerClosedIssue, perIssueHours, 'down')}
          hint={rangeHint(
            o.weeks,
            (w) => w.activeMsPerClosedIssue,
            (w) => w.issuesClosed,
            perIssueHours,
          )}
        />
        <StatTile
          label="$ per merged PR"
          value={money(perPr)}
          delta={versus(perPr, was, (w) => w.usdPerMergedPr, money, 'down')}
          hint={rangeHint(
            o.weeks,
            (w) => w.usdPerMergedPr,
            (w) => w.prsMerged,
            money,
            ' PRs',
          )}
        />
        <StatTile
          label="Issues closed"
          value={closed}
          delta={versus(closed, was, (w) => w.issuesClosed, String, 'up')}
          hint={`range: ${o.weeks.reduce((n, w) => n + w.issuesClosed, 0)}, completed only`}
        />
        <StatTile
          label="No linked outcome"
          value={money(o.unlinkedUsd)}
          delta={{ label: `${o.unlinkedWorkstreams} workstreams` }}
          hint="no <repo>-<issue> name"
        />
        <StatTile
          label="Spent on open issues"
          value={money(o.openUsd)}
          delta={{ label: `${o.openIssues} issues` }}
          hint="not yet an outcome"
        />
        <StatTile
          label="Median elapsed"
          value={h.medianElapsedMs === null ? '—' : duration(h.medianElapsedMs)}
          hint="range: first window to close"
        />
      </Tiles>
      {now && (
        <p className="text-xs text-muted-foreground">
          Tiles: {weekLabel(now)}
          {was && ` vs ${weekLabel(was)}`}. Hints: the whole range. Hours: active orchestrator time.
        </p>
      )}
      {h.cloudUncounted && (
        <p className="text-xs text-muted-foreground">
          Cloud sessions are not counted: their spend is not in the store (#29).
        </p>
      )}
      <Panel title="Per outcome, by week">
        <TrendChart
          data={trendRows(o.weeks)}
          xKey="week"
          xLabel="Week"
          series={[
            { key: 'perIssue', label: '$ per closed issue' },
            { key: 'perPr', label: '$ per merged PR' },
          ]}
          format={money}
          background={{ key: 'spend', label: 'total spend that week', format: money }}
          emptyMessage="No issue closed or PR merged in range"
        />
      </Panel>
    </Section>
  )
}

/** A whole percent as a small bar and its number. */
function Meter({ label, pct, text = `${pct}%` }: { label: string; pct: number; text?: string }) {
  return (
    <span className="inline-flex items-center gap-1.5">
      <Progress aria-label={label} value={pct} size="sm" className="w-14" />
      {text}
    </span>
  )
}

/** A count to look into, flagged when it is not 0. */
const Flag = ({ n }: { n: number }) => (n > 0 ? <Badge variant="warning">{n}</Badge> : <>0</>)

/**
 * A sortable column over an agent row, its cell the value unless `cell` says otherwise. An
 * orchestrator row has no dispatches: with `dispatch` set its cell is a dash and it sorts last,
 * as does a missing value. Orchestrator rows are muted.
 */
function agentColumn(
  header: string,
  value: (r: AgentRow) => number | string | null,
  {
    cell = value,
    dispatch = false,
  }: { cell?: (r: AgentRow) => ReactNode; dispatch?: boolean } = {},
): ColumnDef<AgentRow> {
  const none = (r: AgentRow) => dispatch && r.orchestrator
  return {
    id: header,
    header,
    accessorFn: (r) => (none(r) ? undefined : (value(r) ?? undefined)),
    sortUndefined: 'last',
    cell: ({ row: { original: r } }) => (
      <span className={r.orchestrator ? 'text-muted-foreground' : undefined}>
        {none(r) ? '—' : cell(r)}
      </span>
    ),
  }
}

const inputTokens = ({ tokens: t }: AgentRow) => t.input + t.cacheRead + t.cacheWrite

const AGENT_COLUMNS: ColumnDef<AgentRow>[] = [
  agentColumn('Agent', (r) => named(r.agentType)),
  agentColumn('Model', (r) => named(r.model)),
  agentColumn('Dispatches', (r) => r.dispatches, { dispatch: true }),
  agentColumn('Success', successPct, {
    cell: (r) => {
      const pct = successPct(r)
      return pct === null ? '—' : <Meter label="Success" pct={pct} />
    },
    dispatch: true,
  }),
  agentColumn('Blocked', (r) => r.blocked, { cell: (r) => <Flag n={r.blocked} />, dispatch: true }),
  agentColumn('No report', (r) => r.noReport, {
    cell: (r) => <Flag n={r.noReport} />,
    dispatch: true,
  }),
  agentColumn('Follow-ups', (r) => r.followups, { dispatch: true }),
  agentColumn('$ total', (r) => r.costUsd, { cell: (r) => money(r.costUsd) }),
  agentColumn('$ / dispatch', (r) => r.usdPerDispatch, {
    cell: (r) => money(r.usdPerDispatch),
    dispatch: true,
  }),
  agentColumn('Input Mtok', inputTokens, { cell: (r) => (inputTokens(r) / 1e6).toFixed(1) }),
  agentColumn('Output ktok', (r) => r.tokens.output, {
    cell: (r) => Math.round(r.tokens.output / 1e3),
  }),
  agentColumn('Cache hit', (r) => r.cacheHitPct, {
    cell: ({ cacheHitPct: pct }) =>
      pct === null ? '—' : <Meter label="Cache hit" pct={pct} text={`${pct.toFixed(1)}%`} />,
  }),
]

const agentRowId = (r: AgentRow) => JSON.stringify([r.orchestrator, r.agentType, r.model])

function AgentsSection({ agents }: { agents: AgentRow[] }) {
  return (
    <Section
      title="Agents & models"
      sub="Routing: which model for which lane. Success = completed, with no NEEDS_CONTEXT in the final report. Sorted by $."
    >
      <Panel>
        <DataTable columns={AGENT_COLUMNS} data={agents} getRowId={agentRowId} />
      </Panel>
    </Section>
  )
}

const waitedMs = (r: RepoSums) => r.askMs + r.promptWaitMs + r.blockedMs

/** A per-repo column; Unlinked sorts last either way, as in the tree table. */
function repoColumn(
  header: string,
  value: (r: RepoSums) => number | string,
  cell: (r: RepoSums) => ReactNode,
): ColumnDef<RepoSums> {
  return {
    id: header,
    header,
    accessorFn: (r) => (r.repo === null ? undefined : value(r)),
    sortUndefined: 'last',
    cell: ({ row }) => cell(row.original),
  }
}

const hoursColumn = (header: string, ms: (r: RepoSums) => number) =>
  repoColumn(header, ms, (r) => hours(ms(r)))

const waitShare = (r: RepoSums) => share(waitedMs(r), r.activeMs)

const REPO_COLUMNS: ColumnDef<RepoSums>[] = [
  repoColumn(
    'Repo',
    (r) => r.repo as string,
    (r) => r.repo ?? 'Unlinked',
  ),
  hoursColumn('Questions', (r) => r.askMs),
  hoursColumn('Prompt wait', (r) => r.promptWaitMs),
  hoursColumn('Blocked', (r) => r.blockedMs),
  hoursColumn('Active', (r) => r.activeMs),
  repoColumn(
    'Wait share',
    (r) => waitShare(r) ?? -1,
    (r) => {
      const pct = waitShare(r)
      return pct === null ? '—' : <Meter label="Wait share" pct={pct} />
    },
  ),
]

const repoRowId = (r: RepoSums) => JSON.stringify(r.repo)

function BottleneckSection({ bottleneck }: { bottleneck: Bottleneck }) {
  const t = totals(bottleneck.days)
  const chart = bottleneckChart(bottleneck.days)
  return (
    <Section
      title="Bottleneck"
      sub="Your own time in the loop: answering questions and plan approvals, idle until your next prompt, and agents blocked on you."
    >
      <Tiles>
        <StatTile
          label="Answering questions"
          value={hours(t.askMs)}
          delta={{ label: `${percent(share(t.askMs, t.activeMs))} of ${hours(t.activeMs)} active` }}
          hint="AskUserQuestion, ExitPlanMode"
        />
        <StatTile
          label="Waiting for your prompt"
          value={hours(t.promptWaitMs)}
          delta={{ label: `${percent(share(t.promptWaitMs, t.activeMs))} of active` }}
          hint="last reply → your next message"
        />
        <StatTile
          label="Blocked (proxy)"
          value={hours(t.blockedMs)}
          delta={{ label: `${t.blockedReports} blocked reports` }}
          hint="NEEDS_CONTEXT → next prompt or dispatch"
        />
        {/* `permissionWaits` has one value until hooks record the prompts (#52). */}
        <StatTile
          label="Permission prompts"
          unavailable="Not recorded"
          hint="hooks are off. strate#52 wires them."
        />
      </Tiles>
      <div className="grid gap-3 lg:grid-cols-[minmax(0,3fr)_minmax(0,2fr)]">
        <Panel title="Per day, hours">
          <StackedBarChart
            data={chart.data}
            series={chart.series}
            xKey="day"
            xLabel="Day"
            xFormat={shortDay}
            format={hoursLabel}
            emptyMessage="No time in range"
          />
        </Panel>
        <Panel title="Per repo">
          <DataTable columns={REPO_COLUMNS} data={byRepo(bottleneck.days)} getRowId={repoRowId} />
        </Panel>
      </div>
    </Section>
  )
}

/** A count tile; a real zero is muted. */
function Count({
  label,
  n,
  value = String(n),
  of,
  hint,
}: {
  label: string
  n: number
  value?: string
  of?: string
  hint?: string
}) {
  return (
    <StatTile
      label={label}
      value={value}
      muted={n === 0}
      delta={of === undefined ? undefined : { label: of }}
      hint={hint}
    />
  )
}

function WasteSection({ waste: w }: { waste: Waste }) {
  const top = w.noReportTop
  const peak = w.deepestPeak
  return (
    <Section
      title="Waste & rework"
      sub="What to change in the harness. Zeros are real counts, not missing data."
    >
      <Tiles>
        <Count
          label="Follow-ups to agents"
          n={w.followups}
          of={`of ${w.sendMessages} SendMessage`}
          hint="a dispatch that needed more"
        />
        <Count
          label="Blocked reports"
          n={w.blocked}
          of={`of ${w.dispatches} dispatches`}
          hint="NEEDS_CONTEXT"
        />
        <Count
          label="No report back"
          n={w.noReport}
          of={top ? `${named(top.agentType)} · ${named(top.model)}` : undefined}
          hint="no task notification"
        />
        <Count label="Compactions" n={w.compactions} of={`${w.clears} /clear rotations`} />
        <Count
          label="Context peaks ≥ 80%"
          n={w.contextPeaksOver80}
          of={peak ? `deepest ${Math.round(peak.pct)}% (${peak.model})` : undefined}
        />
        <Count
          label="API retries · errors"
          n={w.apiRetries + w.apiErrors}
          value={`${w.apiRetries} · ${w.apiErrors}`}
          of={`${w.promptTooLong} prompt-too-long`}
        />
        <Count label="Reopened issues" n={w.reopenedIssues} of={`${w.repos} repos`} />
        <Count label="Reverted PRs" n={w.revertedPrs} of={`${w.repos} repos`} />
      </Tiles>
    </Section>
  )
}
