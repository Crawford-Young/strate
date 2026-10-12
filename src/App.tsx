// The app shell: orchestrator tabs, the session line, and resizable panels
// (Harness left, canvas centre, console right, terminal below). The console
// body is #19, Harness content #23, terminals Epic E.
import {
  Button,
  EmptyState,
  ResizableHandle,
  ResizablePanel,
  ResizablePanelGroup,
  Tabs,
  TabsList,
  Toggle,
} from '@crawfordyoung/ui'
import { SlidersVertical, Terminal, X } from 'lucide-react'
import { useEffect, useRef, useState, type ReactNode } from 'react'
import { liveGraph, status, workstreams, type Workstream } from './api'
import { Canvas, agentName } from './Canvas'
import { useLoad, useStoreVersion } from './hooks'
import { OrchestratorTab } from './OrchestratorTab'
import type { Layout } from './layout'
import { usePanels } from './usePanels'

/** The picked tab while it exists, else the live orchestrator, else the first. */
function activeTab(list: Workstream[], picked: number | null): Workstream | null {
  return list.find((w) => w.id === picked) ?? list.find((w) => w.live) ?? list[0] ?? null
}

export function App() {
  const version = useStoreVersion()
  const shell = useLoad('shell', version, () => Promise.all([status(), workstreams()]))
  const [picked, setPicked] = useState<number | null>(null)
  const [agent, setAgent] = useState<number | null>(null)
  const panels = usePanels()
  const canvasRef = useRef<HTMLElement>(null)
  const stripRef = useRef<HTMLDivElement>(null)

  const [current, list] = shell && 'value' in shell ? shell.value : [null, []]
  // The picked tab went away: fall back, and forget the pick and its node so
  // neither comes back if the tab reappears.
  if (picked !== null && !list.some((w) => w.id === picked)) {
    setPicked(null)
    setAgent(null)
  }
  const active = activeTab(list, picked)
  const target = active && String(active.id)
  const loaded = useLoad(target, version, () => liveGraph(Number(target)))
  const graphState = loaded?.target === target ? loaded : null
  const graph = graphState && 'value' in graphState ? graphState.value : undefined
  const error =
    (shell && 'error' in shell ? shell.error : null) ??
    (graphState && 'error' in graphState ? graphState.error : null)
  // Keep the selected tab visible in the (scrollbar-less) strip.
  useEffect(() => {
    stripRef.current
      ?.querySelector('[role="tab"][aria-selected="true"]')
      ?.scrollIntoView({ block: 'nearest', inline: 'nearest' })
  }, [target, list.length])
  const selectedAgent = graph?.agents.find((a) => a.id === agent)

  const selectAgent = (id: number) => {
    setAgent(id)
    panels.open('console')
  }
  const closeConsole = () => {
    // The console unmounts once collapsed; keep focus on the node it showed.
    const canvas = canvasRef.current as HTMLElement
    const card = canvas.querySelector<HTMLElement>(`[data-agent="${agent}"]`)
    ;(card ?? canvas).focus()
    panels.close('console')
  }
  // However the console closes (×, drag), its node selection goes with it.
  const onLayout = (layout: Layout) => {
    if (layout.console === 0) setAgent(null)
    panels.onLayout(layout)
  }
  const panelToggle = (id: 'harness' | 'terminal', label: string, icon: ReactNode) => (
    <Toggle
      variant="outline"
      size="sm"
      pressed={panels.prefs[id].open}
      onPressedChange={() => panels.toggle(id)}
      className="gap-1.5"
    >
      {icon}
      {label}
    </Toggle>
  )

  return (
    <Tabs
      value={target ?? ''}
      onValueChange={(value) => {
        setPicked(Number(value))
        setAgent(null)
      }}
      className="flex h-screen flex-col bg-background text-foreground"
    >
      <header className="flex h-12 flex-none items-stretch gap-1 border-b border-border bg-surface px-3">
        <h1 className="flex items-center pl-1 pr-3 text-sm font-bold tracking-tight">strate</h1>
        {list.length > 0 && (
          <TabsList
            ref={stripRef}
            aria-label="Orchestrators"
            // Wheel and trackpad still scroll it; only the scrollbar is hidden.
            onWheel={(e) => {
              if (Math.abs(e.deltaY) > Math.abs(e.deltaX)) e.currentTarget.scrollLeft += e.deltaY
            }}
            className="h-full w-auto min-w-0 overflow-x-auto border-b-0 [scrollbar-width:none] [&::-webkit-scrollbar]:hidden"
          >
            {list.map((ws) => (
              <OrchestratorTab key={ws.id} ws={ws} />
            ))}
          </TabsList>
        )}
        <div className="flex-1" />
        <div className="flex items-center gap-1.5">
          {panelToggle('harness', 'Harness', <SlidersVertical aria-hidden className="size-3.5" />)}
          {panelToggle('terminal', 'Terminal', <Terminal aria-hidden className="size-3.5" />)}
        </div>
      </header>

      <ResizablePanelGroup
        id="strate-rows"
        orientation="vertical"
        defaultLayout={panels.rows}
        onLayoutChanged={onLayout}
        className="min-h-0 flex-1"
      >
        <ResizablePanel id="main" minSize="20%">
          <ResizablePanelGroup
            id="strate-columns"
            orientation="horizontal"
            defaultLayout={panels.columns}
            onLayoutChanged={onLayout}
          >
            <ResizablePanel id="harness" panelRef={panels.refs.harness} collapsible minSize="15%">
              {panels.prefs.harness.open && (
                <aside aria-label="Harness" className="h-full overflow-auto">
                  <EmptyState message="Harness arrives in #23." />
                </aside>
              )}
            </ResizablePanel>
            <ResizableHandle />
            <ResizablePanel id="canvas" minSize="20%">
              <main
                ref={canvasRef}
                tabIndex={-1}
                className="relative h-full overflow-hidden bg-background bg-[radial-gradient(rgb(63_63_70/0.5)_1px,transparent_1px)] bg-[size:20px_20px] outline-none"
              >
                <Canvas
                  error={error}
                  status={current}
                  active={active}
                  graph={graph}
                  selected={agent}
                  onSelect={selectAgent}
                />
              </main>
            </ResizablePanel>
            <ResizableHandle />
            <ResizablePanel id="console" panelRef={panels.refs.console} collapsible minSize="15%">
              {panels.prefs.console.open && (
                <aside aria-label="Console" className="flex h-full flex-col bg-surface">
                  <div className="flex h-10 flex-none items-center gap-2 border-b border-border px-3">
                    <h2 className="min-w-0 flex-1 truncate text-sm font-semibold">
                      {selectedAgent ? agentName(selectedAgent) : 'Console'}
                    </h2>
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
                  <EmptyState message="The console arrives in #19." />
                </aside>
              )}
            </ResizablePanel>
          </ResizablePanelGroup>
        </ResizablePanel>
        <ResizableHandle />
        <ResizablePanel id="terminal" panelRef={panels.refs.terminal} collapsible minSize="10%">
          {panels.prefs.terminal.open && (
            <section aria-label="Terminal" className="h-full overflow-auto bg-surface">
              <EmptyState message="Terminal arrives in Epic E." />
            </section>
          )}
        </ResizablePanel>
      </ResizablePanelGroup>
    </Tabs>
  )
}
