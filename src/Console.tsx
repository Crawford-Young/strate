// The console body: the selected agent's transcript, read-only, every record in order with its
// time. Each store change reads only what followed the last read (#116): new entries append and
// results complete their pending calls in place. It follows the newest entry while the reader is at
// the bottom. Mount one per agent (`key` it by the agent): a new agent starts at its own bottom.
import {
  EmptyState,
  Transcript,
  TranscriptText,
  TranscriptThinking,
  TranscriptTodo,
  TranscriptTodos,
  TranscriptToolCall,
  TranscriptUserTurn,
  type TranscriptToolCallProps,
} from '@crawfordyoung/ui'
import { useLayoutEffect, useMemo, useRef, type ReactNode } from 'react'
import {
  agentTranscript,
  type TranscriptEntry,
  type TranscriptPage,
  type TranscriptToolStatus,
} from './api'
import { clockTime } from './format'
import { useLoad } from './hooks'

/** How close to the bottom, in pixels, still counts as reading the newest entry. */
const FOLLOW_SLACK_PX = 16

/** The longest result line shown under a call; the rest is in the whole result. */
const SUMMARY_MAX = 160

/**
 * A call still waiting on its result runs only while its agent works; an agent that stopped (done,
 * idle, waiting on the user) left it pending.
 */
function toolStatus(
  status: TranscriptToolStatus,
  working: boolean,
): NonNullable<TranscriptToolCallProps['status']> {
  if (status === 'pending') return working ? 'running' : 'pending'
  return status
}

type ToolCall = Extract<TranscriptEntry, { kind: 'tool_call' }>

const time = (iso: string | null) =>
  iso === null ? undefined : (
    <time dateTime={iso} title={iso}>
      {clockTime(iso)}
    </time>
  )

/** A result's first line to show under its call, and the whole result to open onto when there is more. */
function result(call: ToolCall): Pick<TranscriptToolCallProps, 'resultSummary' | 'result'> {
  if (call.result === null) return {}
  const lines = call.result.split('\n').filter((line) => line.trim() !== '')
  const first = lines[0] ?? '(no output)'
  // By code point, so a cut never splits a surrogate pair.
  const points = Array.from(first)
  const capped = points.length > SUMMARY_MAX
  const more = lines.length > 1 || call.resultTruncated || capped
  return {
    resultSummary: capped ? `${points.slice(0, SUMMARY_MAX).join('')}…` : first,
    result: more ? `${call.result}${call.resultTruncated ? '\n… (truncated)' : ''}` : undefined,
  }
}

function toolCall(call: ToolCall, working: boolean): ReactNode {
  const timestamp = time(call.timestamp)
  // A TodoWrite shows its list; a failed one shows as the failed call it is.
  if (call.todos !== null && call.status !== 'error') {
    return (
      <TranscriptTodos key={call.key} title={call.args} timestamp={timestamp}>
        {call.todos.map((todo, i) => (
          <TranscriptTodo key={i} status={todo.status}>
            {todo.content}
          </TranscriptTodo>
        ))}
      </TranscriptTodos>
    )
  }
  return (
    <TranscriptToolCall
      key={call.key}
      head={call.args}
      status={toolStatus(call.status, working)}
      timestamp={timestamp}
      {...result(call)}
    />
  )
}

function entry(e: TranscriptEntry, working: boolean): ReactNode {
  const timestamp = time(e.timestamp)
  switch (e.kind) {
    case 'user':
      return (
        <TranscriptUserTurn key={e.key} timestamp={timestamp} className="whitespace-pre-wrap">
          {e.text}
        </TranscriptUserTurn>
      )
    case 'text':
      return (
        <TranscriptText key={e.key} timestamp={timestamp} className="whitespace-pre-wrap">
          {e.text}
        </TranscriptText>
      )
    case 'thinking':
      return (
        <TranscriptThinking key={e.key} timestamp={timestamp} className="whitespace-pre-wrap">
          {e.text}
        </TranscriptThinking>
      )
    case 'tool_call':
      return toolCall(e, working)
  }
}

/** What the console has read of an agent's transcript, and where its next read starts. */
interface Read {
  cursor: number | null
  entries: TranscriptEntry[]
}

const UNREAD: Read = { cursor: null, entries: [] }

/**
 * `read` with `page` added: each update completes the last call with its id (one it never saw
 * completes none), then the new entries follow. The same entries when the page changes none of
 * them, and the same read when its cursor did not move either, so nothing redraws.
 */
function merged(read: Read, page: TranscriptPage): Read {
  let entries = read.entries
  for (const done of page.updates) {
    const at = entries.reduce(
      (found, e, i) => (e.kind === 'tool_call' && e.id === done.id ? i : found),
      -1,
    )
    if (at === -1) continue
    if (entries === read.entries) entries = [...entries]
    entries[at] = {
      ...(entries[at] as ToolCall),
      status: done.status,
      result: done.result,
      resultTruncated: done.resultTruncated,
      resultAt: done.resultAt,
    }
  }
  if (page.entries.length > 0) entries = [...entries, ...page.entries]
  if (entries === read.entries && page.cursor === read.cursor) return read
  return { cursor: page.cursor, entries }
}

interface AgentTranscriptProps {
  /** The agent's row id. */
  agent: number
  /** Whether the agent is at work now: its unanswered calls run, else they wait. */
  working: boolean
  /** The store version; each bump reads what followed the last read. */
  version: number
  /** The id of the visible heading that names the scroll region. */
  labelledBy: string
}

export function AgentTranscript({ agent, working, version, labelledBy }: AgentTranscriptProps) {
  const target = String(agent)
  const loaded = useLoad(target, version, async (last: Read | undefined) => {
    const read = last ?? UNREAD
    return merged(read, await agentTranscript(agent, read.cursor))
  })
  // Another agent's read never shows under this one, even for the moment before its own lands.
  const shown = loaded?.target === target ? loaded : null
  const entries = shown && 'value' in shown ? shown.value.entries : null
  // Drawn again only when an entry changed: a read that added nothing leaves the list be.
  const list = useMemo(
    () => entries && <Transcript>{entries.map((e) => entry(e, working))}</Transcript>,
    [entries, working],
  )
  const box = useRef<HTMLDivElement>(null)
  const follow = useRef(true)
  // Each new read lands at the bottom while the reader is there.
  useLayoutEffect(() => {
    const el = box.current
    if (el && follow.current) el.scrollTop = el.scrollHeight
  }, [entries])

  if (shown === null) return <EmptyState message="Reading the transcript…" />
  if ('error' in shown)
    return <EmptyState message={`Couldn't read the transcript: ${shown.error}`} />
  if (shown.value.entries.length === 0) return <EmptyState message="No transcript records yet." />
  return (
    // A stop of its own, so the keyboard can scroll it too; the console's heading names it and
    // the list inside carries a name of its own.
    <div
      ref={box}
      role="region"
      aria-labelledby={labelledBy}
      tabIndex={0}
      onScroll={(e) => {
        const el = e.currentTarget
        follow.current = el.scrollHeight - el.scrollTop - el.clientHeight <= FOLLOW_SLACK_PX
      }}
      className="relative min-h-0 flex-1 overflow-y-auto p-3"
    >
      {/* Each entry keeps its record's key for good, so a completed call updates in place. */}
      {list}
    </div>
  )
}
