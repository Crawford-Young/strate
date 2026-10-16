// The header's "N need you" chip and its popover: every wait on the user, tab or not (#69).
// Longest wait first; a tabbed wait's action goes to its tab, a tab-less one's copies the command
// that resumes it.
import {
  Button,
  Popover,
  PopoverContent,
  PopoverTrigger,
  StatusDot,
  badgeVariants,
} from '@crawfordyoung/ui'
import { ChevronDown } from 'lucide-react'
import { useState } from 'react'
import type { NeedsYou, Workstream } from './api'
import { duration } from './format'
import { useClock } from './hooks'

interface NeedsYouChipProps {
  /** At least one: the header leaves the chip out when nothing waits. */
  waits: NeedsYou[]
  /** Every workstream, live or not: names each wait. */
  sessions: Workstream[]
  /** The workstreams with a tab of their own. */
  tabs: Workstream[]
  onGoToTab: (id: number) => void
}

type Action = { kind: 'tab'; tab: number } | { kind: 'copy'; sessionId: string } | null

interface Row {
  key: string
  name: string
  what: string
  sinceMs: number | null
  action: Action
}

/** A wait's row: named for its workstream, acting on its tab or, a unit's, its director's. */
function toRow(wait: NeedsYou, sessions: Workstream[], tabIds: Set<number>): Row {
  const ws = sessions.find((w) => w.id === wait.workstream)
  const tab = ws && [ws.id, ws.director].find((id) => id !== null && tabIds.has(id))
  const action: Action =
    tab != null
      ? { kind: 'tab', tab }
      : wait.sessionId !== null
        ? { kind: 'copy', sessionId: wait.sessionId }
        : null
  return {
    key: `${wait.source}:${wait.sessionId}:${wait.agentId}`,
    name:
      ws?.name ?? (wait.sessionId ? `session ${wait.sessionId.slice(0, 8)}` : 'unknown session'),
    // A registry wait with nothing named is a background session blocked on you.
    what: wait.waitingFor ?? (wait.source === 'registry' ? 'blocked' : 'waiting'),
    sinceMs: wait.sinceMs,
    action,
  }
}

/**
 * Oldest start first; a wait with no start goes last. Sorting is stable, so ties (two with no start
 * give NaN, which sorts as equal) keep the read's order.
 */
const byLongestWait = (a: Row, b: Row) => (a.sinceMs ?? Infinity) - (b.sinceMs ?? Infinity)

export function NeedsYouChip({ waits, sessions, tabs, onGoToTab }: NeedsYouChipProps) {
  const [open, setOpen] = useState(false)
  const tabIds = new Set(tabs.map((w) => w.id))
  const rows = waits.map((w) => toRow(w, sessions, tabIds)).sort(byLongestWait)
  const count = waits.length
  const goToTab = (id: number) => {
    setOpen(false)
    onGoToTab(id)
  }
  return (
    <Popover open={open} onOpenChange={setOpen}>
      <PopoverTrigger
        // whitespace-nowrap until the library's Badge has it (component-library#139).
        className={`${badgeVariants({ variant: 'outline' })} gap-1.5 whitespace-nowrap hover:bg-item-hover`}
      >
        <StatusDot state="needs-you" />
        {count === 1 ? '1 needs you' : `${count} need you`}
        <ChevronDown aria-hidden className="size-3.5" />
      </PopoverTrigger>
      <PopoverContent aria-label="Waiting on you" align="end" className="w-[400px] p-1">
        <div className="flex justify-between px-2 pb-1.5 pt-2 text-xs text-muted-foreground">
          <span className="font-semibold text-foreground">Waiting on you</span>
          <span>{count}</span>
        </div>
        <ul>
          {rows.map((row) => (
            <WaitRow key={row.key} row={row} onGoToTab={goToTab} />
          ))}
        </ul>
      </PopoverContent>
    </Popover>
  )
}

type Copied = 'idle' | 'copied' | 'failed'

const COPY_LABEL = { idle: 'copy resume', copied: 'copied', failed: 'copy failed' } as const

/** One dense line: dot, name, what it waits for, how long, then its action. */
function WaitRow({ row, onGoToTab }: { row: Row; onGoToTab: (id: number) => void }) {
  const now = useClock(row.sinceMs ?? 0)
  const [copied, setCopied] = useState<Copied>('idle')
  const { action } = row
  const actionButton = (label: string, onClick: () => void) => (
    <Button
      variant="link"
      size="sm"
      aria-label={`${label} for ${row.name}`}
      onClick={onClick}
      className="mr-1 h-7 flex-none px-2 text-xs"
    >
      {label}
    </Button>
  )
  return (
    <li className="flex items-center gap-1 rounded hover:bg-item-hover [&+&]:border-t [&+&]:border-border">
      <div className="flex min-w-0 flex-1 items-center gap-2 px-2 py-1.5">
        <StatusDot state="needs-you" />
        <span className="max-w-[9.5em] truncate font-mono text-[13px]">{row.name}</span>
        <span className="min-w-0 flex-1 truncate text-xs text-muted-foreground">{row.what}</span>
        {row.sinceMs !== null && (
          <span className="whitespace-nowrap text-xs tabular-nums text-muted-foreground">
            {duration(now - row.sinceMs)}
          </span>
        )}
      </div>
      {action?.kind === 'tab' && actionButton('go to tab', () => onGoToTab(action.tab))}
      {action?.kind === 'copy' &&
        actionButton(COPY_LABEL[copied], () => {
          navigator.clipboard.writeText(`claude --resume ${action.sessionId}`).then(
            () => setCopied('copied'),
            () => setCopied('failed'),
          )
        })}
    </li>
  )
}
