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
import { ChevronLeft } from 'lucide-react'
import { useCallback, useEffect, useMemo, useRef, useState } from 'react'
import { flushSync } from 'react-dom'
import {
  auditRefresh,
  writeExport,
  type Audit,
  type AuditFilter,
  type AuditPath,
  type GithubError,
  type Headline,
} from './api'
import { dailyChart, hasPath, hoursLabel, lastDays, repoOptions, shortDay } from './audit'
import { AuditControls, PRESET_DAYS, type Controls } from './AuditControls'
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

const samePath = (a: AuditPath, b: AuditPath) => JSON.stringify(a) === JSON.stringify(b)

export function AuditView({ hidden, onBack }: AuditViewProps) {
  const headingRef = useRef<HTMLHeadingElement>(null)
  useEffect(() => {
    if (!hidden) (headingRef.current as HTMLHeadingElement).focus()
  }, [hidden])

  const [controls, setControls] = useState(START)
  // The selected row, for the row detail. A path means nothing under another grouping.
  const [selected, setSelected] = useState<{ path: AuditPath; label: string } | null>(null)
  // Stable, so the memoized table skips the rerender a store change brings.
  const select = useCallback(
    (path: AuditPath, label: string) =>
      setSelected((s) => (s && samePath(s.path, path) ? null : { path, label })),
    [],
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
      : lastDays(PRESET_DAYS[controls.range], new Date())
  const storeVersion = useStoreVersion()
  const filter: AuditFilter = {
    ...days,
    repos: controls.repo === null ? [] : [controls.repo],
    groupBy: controls.groupBy,
  }
  // Every repo, so the repo select lists them all; the chart narrows to the picked one itself.
  const daily = useAudit({ ...days, repos: [], groupBy: 'day' }, !hidden, storeVersion)
  // By day over every repo, the table's rollup is the chart's, so one read serves both.
  const sameRead = controls.groupBy === 'day' && controls.repo === null
  const own = useAudit(filter, !hidden && !sameRead, storeVersion)
  const main = sameRead ? daily : own
  const chart = useMemo(
    () =>
      daily.audit && dailyChart(daily.audit, controls.measure, controls.repo, days.from, days.to),
    [daily.audit, controls.measure, controls.repo, days.from, days.to],
  )
  const repos = useMemo(() => (daily.audit ? repoOptions(daily.audit) : []), [daily.audit])
  // A row a new repo or range no longer holds is no longer selected.
  const shown = selected && main.audit && hasPath(main.audit.root, selected.path) ? selected : null

  const breakdownRef = useRef<HTMLElement>(null)
  // Focus goes back to the row the detail showed, or to the table when that row is collapsed.
  // Looked up after the close renders, since the table redraws its rows on a selection change.
  const closeDetail = () => {
    const path = JSON.stringify((shown as NonNullable<typeof shown>).path)
    flushSync(() => setSelected(null))
    const breakdown = breakdownRef.current as HTMLElement
    const rows = breakdown.querySelectorAll<HTMLElement>('button[data-path]')
    const row = [...rows].find((b) => b.dataset.path === path)
    ;(row ?? breakdown).focus()
  }

  // The full tree under the current filter; the app asks where to save it.
  const exportCsv = async () => {
    setExportError(null)
    try {
      await writeExport(toCsv(main.audit as Audit), `strate-audit-${days.from}_${days.to}.csv`)
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
        repos={repos}
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
                  <GithubBanner errors={main.audit.githubErrors} />
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
                      xLabel="Day"
                      xFormat={shortDay}
                      format={controls.measure === 'usd' ? money : hoursLabel}
                    />
                  </section>
                )}
                <section
                  ref={breakdownRef}
                  aria-label="Breakdown"
                  tabIndex={-1}
                  className="outline-none"
                >
                  <AuditTable
                    nodes={main.audit.root.children}
                    selected={shown?.path ?? null}
                    onSelect={select}
                    // The tree on screen may be the last filter's while this one is read.
                    disabled={main.pending}
                  />
                </section>
              </>
            )}
          </main>
        </ResizablePanel>
        {shown && (
          <>
            <ResizableHandle />
            <ResizablePanel id="audit-detail" defaultSize="38%" minSize="15%">
              <AuditDetail
                filter={filter}
                path={shown.path}
                label={shown.label}
                active={!hidden}
                storeVersion={storeVersion}
                onClose={closeDetail}
              />
            </ResizablePanel>
          </>
        )}
      </ResizablePanelGroup>
    </div>
  )
}

/** The repos whose GitHub data is missing or stale, and a Refresh that asks for it again. */
function GithubBanner({ errors }: { errors: GithubError[] }) {
  const [refresh, setRefresh] = useState<{ busy: boolean; error: string | null }>({
    busy: false,
    error: null,
  })
  const ask = async () => {
    setRefresh({ busy: true, error: null })
    try {
      await auditRefresh()
      setRefresh({ busy: false, error: null })
    } catch (e) {
      setRefresh({ busy: false, error: String(e) })
    }
  }
  return (
    <Alert variant="warning" className="flex items-start gap-3">
      <AlertDescription className="flex-1">
        GitHub data is missing or stale for {errors.map((e) => `${e.repo}: ${e.error}`).join('; ')}.
        {refresh.error !== null && <span>{` Refresh failed: ${refresh.error}`}</span>}
      </AlertDescription>
      <Button variant="outline" size="sm" disabled={refresh.busy} onClick={() => void ask()}>
        {refresh.busy ? 'Refreshing…' : 'Refresh'}
      </Button>
    </Alert>
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
