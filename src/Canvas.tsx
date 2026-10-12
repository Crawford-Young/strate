// The canvas centre. Until #18 swaps in React Flow, it lists the selected
// tab's live-graph agents as node cards, so node select already works.
import { AgentNodeCard, BorderTrace, EmptyState, TabsContent } from '@crawfordyoung/ui'
import type { GraphAgent, LiveGraph, Status, Workstream } from './api'
import { nodeState, sessionLine } from './format'
import { useClock } from './hooks'

export const agentName = (a: GraphAgent) => a.name ?? a.agentType ?? a.kind

interface CanvasProps {
  error: string | null
  status: Status | null
  active: Workstream | null
  /** Undefined while the active tab's graph loads; null when it has no session. */
  graph: LiveGraph | null | undefined
  selected: number | null
  onSelect: (agent: number) => void
}

export function Canvas({ error, status, active, graph, selected, onSelect }: CanvasProps) {
  if (error !== null) return <EmptyState message={`Couldn't read the store: ${error}`} />
  if (status !== 'live') {
    return (
      <div className="flex h-full items-center justify-center gap-2 text-sm text-muted-foreground">
        <span aria-hidden="true" className="flex">
          <BorderTrace size="sm" />
        </span>
        Indexing…
      </div>
    )
  }
  if (active === null) return <EmptyState message="No workstreams yet." />
  return (
    <TabsContent value={String(active.id)} className="mt-0 flex flex-col gap-4 p-4 pt-16">
      <SessionHeader ws={active} graph={graph ?? null} />
      {graph === null && <EmptyState message="No session to draw yet." />}
      {graph && (
        <div className="flex flex-wrap gap-3">
          {graph.agents.map((a) => (
            <AgentNodeCard
              key={a.id}
              className="w-48"
              name={agentName(a)}
              state={nodeState(a.state)}
              detail={a.description ?? a.model ?? undefined}
              context={a.contextPct ?? undefined}
              contextLabel={a.contextPct === null ? undefined : `${Math.round(a.contextPct)}%`}
              selected={a.id === selected}
              data-agent={a.id}
              onClick={() => onSelect(a.id)}
            />
          ))}
        </div>
      )}
    </TabsContent>
  )
}

/** The tab's name and the one line over the canvas, ticking as its minutes change. */
function SessionHeader({ ws, graph }: { ws: Workstream; graph: LiveGraph | null }) {
  const now = useClock(ws.startedAt ? Date.parse(ws.startedAt) : 0)
  return (
    <div className="pointer-events-none absolute left-4 top-3.5 flex flex-col gap-0.5">
      <h2 className="font-mono text-sm font-semibold">{ws.name}</h2>
      <p className="text-xs tabular-nums text-muted-foreground">{sessionLine(ws, graph, now)}</p>
    </div>
  )
}
