// The Audit row detail (#27 U2): the per-day timeline of the selected row, docked at the bottom.
// It reads only while mounted, and not while Audit is hidden.
import { Alert, AlertDescription, Button, DayTimeline, EmptyState } from '@crawfordyoung/ui'
import { X } from 'lucide-react'
import type { AuditFilter, AuditPath } from './api'
import { shortDay } from './audit'
import { useTimeline } from './useTimeline'

// Legend order, which also sets each kind's colour. The agent spans are non-idle subagent time,
// not the table column's wall time, and run in the sub lane under the orchestrator's.
const KINDS = [
  { key: 'active', label: 'Active' },
  { key: 'wait', label: 'Waiting on you' },
  { key: 'agent', label: 'Agent (non-idle)', lane: 'sub' },
] as const

interface AuditDetailProps {
  filter: AuditFilter
  path: AuditPath
  label: string
  /** False while Audit is hidden. */
  active: boolean
  storeVersion: number
  onClose: () => void
}

export function AuditDetail({
  filter,
  path,
  label,
  active,
  storeVersion,
  onClose,
}: AuditDetailProps) {
  const { days, error } = useTimeline(filter, path, active, storeVersion)
  // Marks read `#9 closed` with no repo, which is ambiguous in a row that holds several: a day,
  // agent type or model group (a PR group is one repo, and so is anything below a top level).
  const spansRepos = path.length === 1 && path[0]?.[0] === 'group' && filter.groupBy !== 'pr'
  return (
    <aside
      aria-label="Row detail"
      className="flex h-full flex-col border-t border-border bg-surface"
    >
      <header className="flex flex-none items-center gap-3 border-b border-border px-4 py-2">
        <h2 className="truncate text-sm font-semibold">{label}</h2>
        <p className="flex-1 truncate text-xs text-muted-foreground">
          {spansRepos
            ? 'This row spans repos: a mark names its issue or PR without the repo.'
            : 'Active, waiting and subagent time (non-idle) per day.'}
        </p>
        <Button variant="ghost" size="icon" aria-label="Close row detail" onClick={onClose}>
          <X aria-hidden className="size-4" />
        </Button>
      </header>
      <div className="min-h-0 flex-1 overflow-auto p-4">
        {error !== null && (
          <Alert variant="destructive">
            <AlertDescription>{error}</AlertDescription>
          </Alert>
        )}
        {days === null ? (
          error === null && <EmptyState message="Loading…" />
        ) : (
          <DayTimeline
            days={days}
            dayFormat={shortDay}
            kinds={KINDS}
            emptyMessage="No activity in this range."
          />
        )}
      </div>
    </aside>
  )
}
