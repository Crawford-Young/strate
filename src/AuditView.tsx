// The full-screen Audit view (#27): one bar (the "Audit" heading, the controls, Refresh GitHub,
// Export CSV and "‹ Live"), the performance sections (#51), then Spend: a daily chart stacked by
// the tree's top rows, the tree table with its breakdown columns (#54) and, docked below, the
// selected row's timeline. One read serves the chart and the tree. It stays mounted while
// hidden, like LiveShell, reads nothing then, and takes focus each time it is shown. It reads when
// shown, when a control changes and after Refresh GitHub, never on a store change or a timer.
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
} from './api'
import { dailyChart, dayRange, hasPath, hoursLabel, lastDays, repoChips, shortDay } from './audit'
import { AuditControls, PRESET_DAYS, type Controls } from './AuditControls'
import { AuditDetail } from './AuditDetail'
import { AuditTable } from './AuditTable'
import { toCsv } from './csv'
import { money } from './format'
import { PerformanceView, Section } from './PerformanceView'
import { useAudit } from './useAudit'
import { usePerformance } from './usePerformance'

interface AuditViewProps {
  hidden: boolean
  onBack: () => void
}

const START: Controls = {
  range: '7d',
  custom: { from: '', to: '' },
  repos: [],
  measure: 'usd',
}

const samePath = (a: AuditPath, b: AuditPath) => JSON.stringify(a) === JSON.stringify(b)

export function AuditView({ hidden, onBack }: AuditViewProps) {
  const headingRef = useRef<HTMLHeadingElement>(null)
  useEffect(() => {
    if (!hidden) (headingRef.current as HTMLHeadingElement).focus()
  }, [hidden])
  // Moving it rereads what is shown: each time Audit is shown, and after a GitHub refresh.
  const [version, setVersion] = useState(0)
  const [wasHidden, setWasHidden] = useState(hidden)
  if (hidden !== wasHidden) {
    setWasHidden(hidden)
    if (!hidden) setVersion((v) => v + 1)
  }
  const [github, setGithub] = useState({ busy: false, error: null as string | null })
  // auditRefresh resolves once the fetch is stored, so the reread after it sees the new rows.
  const refreshGithub = async () => {
    setGithub({ busy: true, error: null })
    try {
      await auditRefresh()
      setGithub({ busy: false, error: null })
      setVersion((v) => v + 1)
    } catch (e) {
      setGithub({ busy: false, error: String(e) })
    }
  }

  const [controls, setControls] = useState(START)
  // The selected row, for the row detail.
  const [selected, setSelected] = useState<{ path: AuditPath; label: string } | null>(null)
  // Stable, so the memoized table skips rerenders that change none of its props.
  const select = useCallback(
    (path: AuditPath, label: string) =>
      setSelected((s) => (s && samePath(s.path, path) ? null : { path, label })),
    [],
  )
  const [exportError, setExportError] = useState<string | null>(null)
  const change = (patch: Partial<Controls>) => setControls((c) => ({ ...c, ...patch }))

  // Recomputed each render, so the range rolls over at local midnight.
  const days =
    controls.range === 'custom'
      ? controls.custom
      : lastDays(PRESET_DAYS[controls.range], new Date())
  const filter: AuditFilter = { ...days, repos: controls.repos }
  const main = useAudit(filter, !hidden, version)
  const perf = usePerformance(filter, !hidden, version)
  const chart = useMemo(
    () => main.audit && dailyChart(main.audit, controls.measure, days.from, days.to),
    [main.audit, controls.measure, days.from, days.to],
  )
  // Stable per range, so the memoized table only redraws when something it shows changes.
  const range = useMemo(() => dayRange(days.from, days.to), [days.from, days.to])
  // Listed from the read itself, which names every repo in range whatever the filter.
  const repos = repoChips(main.audit, controls.repos)
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
    // The page never scrolls: the main panel does, alone.
    <div
      hidden={hidden}
      className="flex h-screen flex-col overflow-hidden bg-background text-foreground"
    >
      <AuditControls
        title={
          <h1
            ref={headingRef}
            tabIndex={-1}
            className="pl-1 text-sm font-bold tracking-tight outline-none"
          >
            Audit
          </h1>
        }
        value={controls}
        days={days}
        onChange={change}
        repos={repos}
        actions={
          <>
            <Button
              variant="outline"
              size="sm"
              disabled={github.busy}
              onClick={() => void refreshGithub()}
            >
              {github.busy ? 'Refreshing…' : 'Refresh GitHub'}
            </Button>
            <Button
              variant="outline"
              size="sm"
              disabled={main.audit === null || main.pending}
              onClick={() => void exportCsv()}
            >
              Export CSV
            </Button>
            {/* "‹ Live" sits where the live header's Audit button does, so the two swap in place. */}
            <Button variant="outline" size="sm" onClick={onBack} className="gap-1.5">
              <ChevronLeft aria-hidden className="size-3.5" />
              Live
            </Button>
          </>
        }
      />
      <ResizablePanelGroup id="strate-audit" orientation="vertical" className="min-h-0 flex-1">
        <ResizablePanel id="audit-main" minSize="20%">
          {/* The panel's own box scrolls (react-resizable-panels sets overflow: auto on it), so
              main is not a scroll box too: nested, the two drew two scrollbars (#54). */}
          <main aria-busy={main.pending} className="space-y-4 p-4 aria-busy:opacity-70">
            {exportError !== null && (
              <Alert variant="destructive">
                <AlertDescription>Export failed: {exportError}</AlertDescription>
              </Alert>
            )}
            {github.error !== null && (
              <Alert variant="destructive">
                <AlertDescription>GitHub refresh failed: {github.error}</AlertDescription>
              </Alert>
            )}
            {main.error !== null && (
              <Alert variant="destructive">
                <AlertDescription>{main.error}</AlertDescription>
              </Alert>
            )}
            {perf.error !== null && (
              <Alert variant="destructive">
                <AlertDescription>Performance read failed: {perf.error}</AlertDescription>
              </Alert>
            )}
            {main.audit === null ? (
              main.error === null && <EmptyState message="Loading…" />
            ) : (
              <>
                {main.audit.githubErrors.length > 0 && (
                  <GithubBanner errors={main.audit.githubErrors} />
                )}
                {perf.performance && (
                  <PerformanceView headline={main.audit.headline} performance={perf.performance} />
                )}
                <Section title="Spend" sub="Where the $ and time went.">
                  {chart && (
                    <section aria-label="Per day" className="space-y-2">
                      <h3 className="text-xs font-medium text-muted-foreground">
                        {controls.measure === 'usd' ? '$' : 'Agent-hours'} per day, stacked by repo
                      </h3>
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
                      root={main.audit.root}
                      days={range}
                      measure={controls.measure}
                      selected={shown?.path ?? null}
                      onSelect={select}
                      // The tree on screen may be the last filter's while this one is read.
                      disabled={main.pending}
                    />
                  </section>
                </Section>
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
                version={version}
                onClose={closeDetail}
              />
            </ResizablePanel>
          </>
        )}
      </ResizablePanelGroup>
    </div>
  )
}

/** The repos whose GitHub data is missing or stale; the control bar's Refresh GitHub asks again. */
function GithubBanner({ errors }: { errors: GithubError[] }) {
  return (
    <Alert variant="warning">
      <AlertDescription>
        GitHub data is missing or stale for {errors.map((e) => `${e.repo}: ${e.error}`).join('; ')}.
      </AlertDescription>
    </Alert>
  )
}
