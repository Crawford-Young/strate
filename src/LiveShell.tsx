// The live view: the header, then two resizable panels, the canvas centre and the console right.
// The console opens when a node is selected and shows its transcript. Audit is the only full-screen view:
// while it shows, this view stays mounted, `hidden` instead, so its state survives the visit.
import {
  Button,
  EmptyState,
  ResizableHandle,
  ResizablePanel,
  ResizablePanelGroup,
  Tabs,
} from '@crawfordyoung/ui'
import { X } from 'lucide-react'
import { useRef, useState, type RefObject } from 'react'
import { liveGraph, needsYou, status, workstreams, type GraphAgent, type Workstream } from './api'
import { Canvas, agentName } from './Canvas'
import { AgentTranscript } from './Console'
import { Header } from './Header'
import { useLoad, useStoreVersion } from './hooks'
import { MIN_SIZE, type Layout } from './layout'
import { usePanels } from './usePanels'

/** The tab and node the user picked; `tab` null means "none yet", so the first live tab shows. */
interface Selection {
  tab: number | null
  agent: number | null
}
const NONE: Selection = { tab: null, agent: null }

/** A tab of its own: live and not nested under a director's tab. */
const hasTab = (w: Workstream) => w.live && w.director === null

const isLive = (sessions: Workstream[], id: number | null) =>
  sessions.some((w) => hasTab(w) && w.id === id)

interface LiveShellProps {
  hidden: boolean
  auditButtonRef: RefObject<HTMLButtonElement | null>
  onOpenAudit: () => void
}

/** The console heading's id: it names the transcript's scroll region. */
const HEADING_ID = 'console-heading'

/** An agent's charge key and where it came from, or that it has none. */
const chargeLine = (a: GraphAgent) =>
  a.chargeKey === null ? 'no charge' : `charge ${a.chargeKey} from ${a.chargeSource}`

export function LiveShell({ hidden, auditButtonRef, onOpenAudit }: LiveShellProps) {
  const version = useStoreVersion(hidden)
  const [selection, setSelection] = useState<Selection>(NONE)
  const overview = useLoad('overview', version, async () => {
    const loaded = await Promise.all([status(), workstreams(), needsYou()])
    // A pick whose session is gone is forgotten, with its node, so neither returns if the tab
    // reappears. Done as the data lands, in the same batch as the data, since that is the only time the live set changes.
    setSelection((s) => (s.tab === null || isLive(loaded[1], s.tab) ? s : NONE))
    return loaded
  })
  // A new tab shows with no node selected.
  const pickTab = (tab: number) => setSelection({ tab, agent: null })
  // Returns the same object when nothing changes, so a repeated layout event rerenders nothing.
  const setAgent = (agent: number | null) =>
    setSelection((s) => (s.agent === agent ? s : { ...s, agent }))
  const panels = usePanels()
  const canvasRef = useRef<HTMLElement>(null)

  const [storeStatus, sessions, waits] =
    overview && 'value' in overview ? overview.value : [null, [], []]
  // Only live, top-level sessions get a tab: past ones belong to the audit view, and units
  // show on their director's canvas.
  const liveSessions = sessions.filter(hasTab)
  const active = liveSessions.find((w) => w.id === selection.tab) ?? liveSessions[0] ?? null
  const activeKey = active && String(active.id)
  const picked = selection.agent

  const graphLoad = useLoad(activeKey, version, () => liveGraph(Number(activeKey)))
  const currentGraphLoad = graphLoad?.target === activeKey ? graphLoad : null
  const graph = currentGraphLoad && 'value' in currentGraphLoad ? currentGraphLoad.value : undefined
  const error =
    (overview && 'error' in overview ? overview.error : null) ??
    (currentGraphLoad && 'error' in currentGraphLoad ? currentGraphLoad.error : null)
  // A picked orchestrator names its whole `/clear` chain: after a /clear the chain draws its next
  // link, a new row, and the pick follows it there (#133).
  const selectedAgent =
    picked === null
      ? undefined
      : graph?.agents.find((a) => a.id === picked || a.links.includes(picked))
  const agentId = selectedAgent?.id ?? picked
  // Kept, so the latest link's transcript stays open should the chain leave the canvas too.
  if (agentId !== picked) setAgent(agentId)
  // A finished agent leaves the canvas but its transcript stays open, so the heading keeps its name.
  const [lastNamed, setLastNamed] = useState<{ id: number; name: string; charge: string } | null>(
    null,
  )
  const named = selectedAgent && {
    id: selectedAgent.id,
    name: agentName(selectedAgent),
    charge: chargeLine(selectedAgent),
  }
  if (
    named &&
    (lastNamed?.id !== named.id ||
      lastNamed.name !== named.name ||
      lastNamed.charge !== named.charge)
  )
    setLastNamed(named)
  const shown = named ?? (lastNamed?.id === agentId ? lastNamed : null)
  const heading = shown ? shown.name : 'Console'

  const selectAgent = (id: number) => {
    setAgent(id)
    panels.open('console')
  }
  const closeConsole = () => {
    // The console unmounts once collapsed; keep focus on the node it showed.
    const canvas = canvasRef.current as HTMLElement
    const card = canvas.querySelector<HTMLElement>(`[data-agent="${agentId}"]`)
    ;(card ?? canvas).focus()
    panels.close('console')
  }
  // However the console closes (×, drag), its node selection goes with it.
  const onLayout = (layout: Layout) => {
    if (layout.console === 0) setAgent(null)
    panels.onLayout(layout)
  }

  return (
    <Tabs
      value={activeKey ?? ''}
      onValueChange={(value) => pickTab(Number(value))}
      hidden={hidden}
      className="flex h-screen flex-col bg-background text-foreground"
    >
      <Header
        sessions={sessions}
        liveSessions={liveSessions}
        waits={waits}
        onGoToTab={pickTab}
        activeKey={activeKey}
        auditButtonRef={auditButtonRef}
        onOpenAudit={onOpenAudit}
      />

      <ResizablePanelGroup
        id="strate-columns"
        orientation="horizontal"
        defaultLayout={panels.columns}
        onLayoutChanged={onLayout}
        className="min-h-0 flex-1"
      >
        <ResizablePanel id="canvas" minSize={`${MIN_SIZE.canvas}%`} className="relative">
          <main
            ref={canvasRef}
            tabIndex={-1}
            className="relative h-full overflow-hidden bg-background outline-none"
          >
            <Canvas
              error={error}
              status={storeStatus}
              active={active}
              graph={graph}
              selected={agentId}
              onSelect={selectAgent}
            />
          </main>
        </ResizablePanel>
        <ResizableHandle />
        <ResizablePanel
          id="console"
          panelRef={panels.refs.console}
          collapsible
          minSize={`${MIN_SIZE.console}%`}
          className="relative"
        >
          {panels.prefs.console.open && (
            <aside aria-label="Console" className="flex h-full flex-col bg-surface">
              <div className="flex h-10 flex-none items-center gap-2 border-b border-border px-3">
                <div className="flex min-w-0 flex-1 flex-col">
                  <h2 id={HEADING_ID} className="truncate text-sm font-semibold">
                    {heading}
                  </h2>
                  {/* Visible, so a sighted keyboard user reads what the node's chip only speaks. */}
                  {shown && (
                    <p className="truncate font-mono text-[11px] text-muted-foreground">
                      {shown.charge}
                    </p>
                  )}
                </div>
                <Button
                  variant="ghost"
                  size="icon"
                  aria-label="Close console"
                  onClick={closeConsole}
                  className="size-7"
                >
                  <X aria-hidden className="size-4" />
                </Button>
              </div>
              {agentId === null ? (
                <EmptyState message="Select an agent to see its transcript." />
              ) : (
                // Its own instance per agent, so each starts at its own newest entry; and gone
                // while the console is closed, so a reopen reads afresh.
                // A finished agent is not drawn, so it reads as not working.
                <AgentTranscript
                  key={agentId}
                  agent={agentId}
                  working={selectedAgent?.state === 'working'}
                  version={version}
                  labelledBy={HEADING_ID}
                />
              )}
            </aside>
          )}
        </ResizablePanel>
      </ResizablePanelGroup>
    </Tabs>
  )
}
