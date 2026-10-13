// The full-screen Audit view (#27): its own header with "‹ Live", the control bar, the headline
// KPIs, a daily chart stacked by repo, the tree table and, docked below, the selected row's
// timeline. It stays mounted while hidden, like LiveShell, reads nothing then, and takes focus
// each time it is shown.
import {
  Alert,
  AlertDescription,
  Button,
  EmptyState,
  ResizableHandle,
  ResizablePanel,
  ResizablePanelGroup,
  StackedBarChart,
} from '@crawfordyoung/ui'
import { save } from '@tauri-apps/plugin-dialog'
import { ChevronLeft } from 'lucide-react'
import { useEffect, useRef, useState } from 'react'
import { auditRefresh, writeExport, type Audit, type AuditPath, type Headline } from './api'
import { dailyChart, hoursLabel, lastDays, repoOptions } from './audit'
import { AuditControls, type Controls } from './AuditControls'
import { AuditDetail } from './AuditDetail'
import { AuditTable } from './AuditTable'
import { toCsv } from './csv'
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
  // The selected row, for the row detail. A path means nothing under another grouping.
  const [selected, setSelected] = useState<{ path: AuditPath; label: string } | null>(null)
  const select = (path: AuditPath, label: string) =>
    setSelected((s) =>
      s && JSON.stringify(s.path) === JSON.stringify(path) ? null : { path, label },
    )
  const [exportError, setExportError] = useState<string | null>(null)
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
  const filter = { ...days, repos, groupBy: controls.groupBy }
  const main = useAudit(filter, !hidden, storeVersion)
  // Every repo, so the repo select lists them all; the chart narrows to the picked one itself.
  const daily = useAudit({ ...days, repos: [], groupBy: 'day' }, !hidden, storeVersion)
  const chart =
    daily.audit && dailyChart(daily.audit, controls.measure, controls.repo, days.from, days.to)

  // The full tree under the current filter, saved wherever the user picks.
  const exportCsv = async () => {
    setExportError(null)
    try {
      const path = await save({
        defaultPath: `strate-audit-${days.from}_${days.to}.csv`,
        filters: [{ name: 'CSV', extensions: ['csv'] }],
      })
      if (path !== null) await writeExport(path, toCsv(main.audit as Audit))
    } catch (e) {
      setExportError(String(e))
    }
  }

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
        actions={
          <Button
            variant="outline"
            size="sm"
            disabled={main.audit === null || main.pending}
            onClick={() => void exportCsv()}
          >
            Export CSV
          </Button>
        }
      />
      <ResizablePanelGroup id="strate-audit" orientation="vertical" className="min-h-0 flex-1">
        <ResizablePanel id="audit-main" minSize="20%">
          <main
            aria-busy={main.pending}
            className="h-full space-y-4 overflow-auto p-4 aria-busy:opacity-70"
          >
            {exportError !== null && (
              <Alert variant="destructive">
                <AlertDescription>Export failed: {exportError}</AlertDescription>
              </Alert>
            )}
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
                    nodes={main.audit.root.children}
                    selected={selected?.path ?? null}
                    onSelect={select}
                  />
                </section>
              </>
            )}
          </main>
        </ResizablePanel>
        {selected && (
          <>
            <ResizableHandle />
            <ResizablePanel id="audit-detail" defaultSize="38%" minSize="15%">
              <AuditDetail
                filter={filter}
                path={selected.path}
                label={selected.label}
                active={!hidden}
                storeVersion={storeVersion}
                onClose={() => setSelected(null)}
              />
            </ResizablePanel>
          </>
        )}
      </ResizablePanelGroup>
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
