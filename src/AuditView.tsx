// The full-screen Audit view (#27): its own header with "‹ Live", the control bar, the headline
// KPIs, a daily chart stacked by repo and the tree table. It stays mounted while hidden, like
// LiveShell, reads nothing then, and takes focus each time it is shown.
import { Alert, AlertDescription, Button, EmptyState, StackedBarChart } from '@crawfordyoung/ui'
import { ChevronLeft } from 'lucide-react'
import { useEffect, useRef, useState } from 'react'
import { auditRefresh, type AuditPath, type Headline } from './api'
import { dailyChart, hoursLabel, lastDays, repoOptions } from './audit'
import { AuditControls, type Controls } from './AuditControls'
import { AuditTable } from './AuditTable'
import { duration, money } from './format'
import { useStoreVersion } from './hooks'
import { useAudit } from './useAudit'

interface AuditViewProps {
  hidden: boolean
  onBack: () => void
}

const START: Controls = {
  range: '7d',
  custom: { from: '', to: '' },
  repo: null,
  groupBy: 'hierarchy',
  measure: 'usd',
}

const dayLabel = new Intl.DateTimeFormat(undefined, { month: 'short', day: 'numeric' })

export function AuditView({ hidden, onBack }: AuditViewProps) {
  const headingRef = useRef<HTMLHeadingElement>(null)
  useEffect(() => {
    if (!hidden) (headingRef.current as HTMLHeadingElement).focus()
  }, [hidden])

  const [controls, setControls] = useState(START)
  // The selected row, for the row detail (#27 U2). A path means nothing under another grouping.
  const [selected, setSelected] = useState<AuditPath | null>(null)
  const change = (patch: Partial<Controls>) => {
    if (patch.groupBy !== undefined && patch.groupBy !== controls.groupBy) setSelected(null)
    setControls((c) => ({ ...c, ...patch }))
  }

  // Recomputed each render, so the range rolls over at local midnight.
  const days =
    controls.range === 'custom'
      ? controls.custom
      : lastDays(Number.parseInt(controls.range), new Date())
  const repos = controls.repo === null ? [] : [controls.repo]
  const storeVersion = useStoreVersion()
  const main = useAudit({ ...days, repos, groupBy: controls.groupBy }, !hidden, storeVersion)
  // Every repo, so the repo select lists them all; the chart narrows to the picked one itself.
  const daily = useAudit({ ...days, repos: [], groupBy: 'day' }, !hidden, storeVersion)
  const chart =
    daily.audit && dailyChart(daily.audit, controls.measure, controls.repo, days.from, days.to)

  return (
    <div hidden={hidden} className="flex h-screen flex-col bg-background text-foreground">
      <header className="flex h-12 flex-none items-center gap-3 border-b border-border bg-surface px-3">
        <Button variant="ghost" size="sm" onClick={onBack} className="gap-1">
          <ChevronLeft aria-hidden className="size-4" />
          Live
        </Button>
        <h1
          ref={headingRef}
          tabIndex={-1}
          className="text-sm font-bold tracking-tight outline-none"
        >
          Audit
        </h1>
      </header>
      <AuditControls
        value={controls}
        days={days}
        onChange={change}
        repos={daily.audit ? repoOptions(daily.audit) : []}
      />
      <main
        aria-busy={main.pending}
        className="min-h-0 flex-1 space-y-4 overflow-auto p-4 aria-busy:opacity-70"
      >
        {main.error !== null && (
          <Alert variant="destructive">
            <AlertDescription>{main.error}</AlertDescription>
          </Alert>
        )}
        {main.audit === null ? (
          main.error === null && <EmptyState message="Loading…" />
        ) : (
          <>
            {main.audit.githubErrors.length > 0 && (
              <Alert variant="warning" className="flex items-start gap-3">
                <AlertDescription className="flex-1">
                  GitHub data is missing or stale for{' '}
                  {main.audit.githubErrors.map((e) => `${e.repo}: ${e.error}`).join('; ')}.
                </AlertDescription>
                <Button variant="outline" size="sm" onClick={() => void auditRefresh()}>
                  Refresh
                </Button>
              </Alert>
            )}
            <Kpis headline={main.audit.headline} />
            {chart && (
              <section aria-label="Per day" className="space-y-2">
                <h2 className="text-sm font-semibold">
                  {controls.measure === 'usd' ? '$' : 'Agent-hours'} per day, stacked by repo
                </h2>
                <StackedBarChart
                  data={chart.data}
                  series={chart.series}
                  xKey="day"
                  xFormat={(day) => dayLabel.format(new Date(`${day}T00:00`))}
                  format={controls.measure === 'usd' ? money : hoursLabel}
                />
              </section>
            )}
            <section aria-label="Breakdown">
              <AuditTable
                // Expanded rows are kept by index, which means nothing under another grouping.
                key={controls.groupBy}
                nodes={main.audit.root.children}
                selected={selected}
                onSelect={setSelected}
              />
            </section>
          </>
        )}
      </main>
    </div>
  )
}

function Kpis({ headline: h }: { headline: Headline }) {
  const kpis: [string, string][] = [
    ['Issues closed', String(h.issuesClosed)],
    ['Median elapsed', h.medianElapsedMs === null ? '—' : duration(h.medianElapsedMs)],
    ['$ per closed issue', money(h.usdPerClosedIssue)],
    ['$ per merged PR', money(h.usdPerMergedPr)],
  ]
  return (
    <section aria-label="Headline" className="space-y-2">
      <dl className="grid grid-cols-4 gap-3">
        {kpis.map(([label, value]) => (
          <div key={label} className="rounded-lg border border-border bg-surface px-4 py-3">
            <dt className="text-xs text-muted-foreground">{label}</dt>
            <dd className="text-xl font-semibold tabular-nums">{value}</dd>
          </div>
        ))}
      </dl>
      {h.cloudUncounted && (
        <p className="text-xs text-muted-foreground">
          Cloud sessions are not counted: their spend is not in the store (#29).
        </p>
      )}
    </section>
  )
}
