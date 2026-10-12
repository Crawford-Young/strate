import { Badge, StatusDot, TabsTrigger } from '@crawfordyoung/ui'
import type { Workstream } from './api'
import { money, nodeState } from './format'

const STATE_LABEL = {
  working: 'working',
  needs_you: 'needs you',
  idle: 'idle',
  done: 'done',
} as const

/** One orchestrator tab: status dot, name, total $ and a needs-you badge. */
export function OrchestratorTab({ ws }: { ws: Workstream }) {
  return (
    <TabsTrigger value={String(ws.id)} className="h-full gap-2 px-3.5 font-mono text-[13px]">
      <StatusDot state={nodeState(ws.state)} label={STATE_LABEL[ws.state]} />
      {ws.fallback ? (
        <span
          className="italic text-muted-foreground"
          title="Unnamed: no /rename yet, so named from its cwd and branch"
        >
          {ws.name}
          <span className="sr-only"> (unnamed)</span>
        </span>
      ) : (
        ws.name
      )}
      <span className="font-sans text-xs tabular-nums text-muted-foreground">
        {money(ws.costUsd)}
      </span>
      {ws.needsYou > 0 && (
        // The dot already says "needs you" to assistive tech.
        <Badge variant="warning" aria-hidden="true" className="px-2 py-px font-sans text-[11px]">
          needs you
        </Badge>
      )}
    </TabsTrigger>
  )
}
