// The top bar: the app name, one tab per live orchestrator, and the panel toggles on the right.
// Must render inside a `Tabs` (the strip is a TabsList).
import { TabsList, Toggle } from '@crawfordyoung/ui'
import { SlidersVertical, Terminal } from 'lucide-react'
import { useEffect, useRef, type ReactNode } from 'react'
import type { Workstream } from './api'
import { OrchestratorTab } from './OrchestratorTab'
import type { LayoutPrefs } from './layout'

type TogglePanel = 'harness' | 'terminal'

interface HeaderProps {
  liveSessions: Workstream[]
  /** The selected tab's value, as the `Tabs` sees it. */
  activeKey: string | null
  prefs: LayoutPrefs
  onTogglePanel: (id: TogglePanel) => void
}

export function Header({ liveSessions, activeKey, prefs, onTogglePanel }: HeaderProps) {
  const stripRef = useRef<HTMLDivElement>(null)
  // Keep the selected tab visible in the (scrollbar-less) strip.
  useEffect(() => {
    stripRef.current
      ?.querySelector('[role="tab"][aria-selected="true"]')
      ?.scrollIntoView({ block: 'nearest', inline: 'nearest' })
  }, [activeKey, liveSessions.length])

  const panelToggle = (id: TogglePanel, label: string, icon: ReactNode) => (
    <Toggle
      variant="outline"
      size="sm"
      pressed={prefs[id].open}
      onPressedChange={() => onTogglePanel(id)}
      className="gap-1.5"
    >
      {icon}
      {label}
    </Toggle>
  )

  return (
    <header className="flex h-12 flex-none items-stretch gap-1 border-b border-border bg-surface px-3">
      <h1 className="flex items-center pl-1 pr-3 text-sm font-bold tracking-tight">strate</h1>
      {liveSessions.length > 0 && (
        <TabsList
          ref={stripRef}
          aria-label="Orchestrators"
          // Wheel and trackpad still scroll it; only the scrollbar is hidden.
          onWheel={(e) => {
            if (Math.abs(e.deltaY) > Math.abs(e.deltaX)) e.currentTarget.scrollLeft += e.deltaY
          }}
          className="h-full w-auto min-w-0 overflow-x-auto border-b-0 [scrollbar-width:none] [&::-webkit-scrollbar]:hidden"
        >
          {liveSessions.map((ws) => (
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
  )
}
