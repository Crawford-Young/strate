// The canvas centre: the selected tab's live session as a tiered graph
// (orchestrator → subagents, teammates and directed units → their dispatches), laid out by ELK
// and drawn by React Flow. Selecting a node opens it in the console.
import { AgentNodeCard, BorderTrace, Button, EmptyState, TabsContent } from '@crawfordyoung/ui'
import {
  Background,
  BackgroundVariant,
  Handle,
  Position,
  ReactFlow,
  ReactFlowProvider,
  useReactFlow,
  type Dimensions,
  type Edge,
  type Node,
  type NodeChange,
  type NodeProps,
} from '@xyflow/react'
import '@xyflow/react/dist/base.css'
import { Minus, Plus } from 'lucide-react'
import { useCallback, useState } from 'react'
import type { GraphAgent, LiveGraph, Status, Workstream } from './api'
import { nodeState, sessionLine } from './format'
import { NODE_HEIGHT, NODE_WIDTH, drawnEdges } from './graphLayout'
import { useClock } from './hooks'
import { useGraphLayout } from './useGraphLayout'

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
  // Outlives tab switches, so a session's count goes on while another tab shows.
  const [firstSeen] = useState(() => new Map<string, FirstSeen>())
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
  if (active === null) return <EmptyState message="No live sessions." />
  return (
    <TabsContent value={String(active.id)} className="mt-0 h-full">
      <SessionHeader ws={active} graph={graph ?? null} firstSeen={firstSeen} />
      {graph === null && (
        <div className="p-4 pt-16">
          <EmptyState message="No session to draw yet." />
        </div>
      )}
      {graph && (
        // One flow per tab and session: another one lays out and fits fresh.
        <ReactFlowProvider key={`${active.id}:${graph.sessionId}`}>
          <GraphView graph={graph} selected={selected} onSelect={onSelect} />
        </ReactFlowProvider>
      )}
    </TabsContent>
  )
}

type AgentNodeData = {
  agent: GraphAgent
  selected: boolean
}

type AgentFlowNode = Node<AgentNodeData, 'agent'>

/**
 * A library node card, with hidden handles for the edges to meet it. The card
 * is the tab stop; a card that takes focus off-screen is centred, since the
 * flow resets any scroll the browser does to reveal it.
 */
function AgentNode({
  data: { agent: a, selected },
  positionAbsoluteX,
  positionAbsoluteY,
}: NodeProps<AgentFlowNode>) {
  const { setCenter, getZoom, flowToScreenPosition } = useReactFlow()
  const reveal = (card: HTMLElement) => {
    // From the flow's own transform: mid-focus, the card's DOM box reflects the
    // browser's reveal scroll, which the flow is about to undo.
    const view = (card.closest('.react-flow') as HTMLElement).getBoundingClientRect()
    const { x: left, y: top } = flowToScreenPosition({ x: positionAbsoluteX, y: positionAbsoluteY })
    const end = flowToScreenPosition({
      x: positionAbsoluteX + NODE_WIDTH,
      y: positionAbsoluteY + NODE_HEIGHT,
    })
    const box = { left, top, right: end.x, bottom: end.y }
    const inView =
      box.left >= view.left &&
      box.right <= view.right &&
      box.top >= view.top &&
      box.bottom <= view.bottom
    if (inView) return
    void setCenter(positionAbsoluteX + NODE_WIDTH / 2, positionAbsoluteY + NODE_HEIGHT / 2, {
      zoom: getZoom(),
      duration: 200,
    })
  }
  return (
    <>
      <Handle type="target" position={Position.Top} isConnectable={false} className="opacity-0" />
      <AgentNodeCard
        className="w-48"
        name={agentName(a)}
        state={nodeState(a.state)}
        detail={a.description ?? a.model ?? undefined}
        context={a.contextPct ?? undefined}
        contextLabel={a.contextPct === null ? undefined : `${Math.round(a.contextPct)}%`}
        selected={selected}
        data-agent={a.id}
        onFocus={(e) => reveal(e.currentTarget)}
      />
      <Handle
        type="source"
        position={Position.Bottom}
        isConnectable={false}
        className="opacity-0"
      />
    </>
  )
}

const nodeTypes = { agent: AgentNode }

const EDGE_STYLE = { stroke: 'rgb(var(--muted-foreground) / 0.5)', strokeWidth: 2 }
const EDGE_STYLES = {
  dispatch: EDGE_STYLE,
  teammate: { ...EDGE_STYLE, strokeDasharray: '6 4' },
  // A director's unit: solid like dispatch, at the full muted colour so it reads as the stronger link.
  directs: { ...EDGE_STYLE, stroke: 'rgb(var(--muted-foreground))' },
}
const FIT_VIEW_OPTIONS = { maxZoom: 1 }
/** How long the canvas size holds still before the graph re-fits: a handle drag fits once, at its end. */
export const REFIT_MS = 150

interface GraphViewProps {
  graph: LiveGraph
  selected: number | null
  onSelect: (agent: number) => void
}

/**
 * Nothing until the first layout lands, so no node flashes at the origin.
 * Nodes are controlled, so each one carries the size the flow measured and
 * reported through `onNodesChange`; without it the flow re-hides the node
 * and drops its edges on every render.
 */
function GraphView({ graph, selected, onSelect }: GraphViewProps) {
  const { positions, error } = useGraphLayout(graph)
  const [measured, setMeasured] = useState<ReadonlyMap<string, Dimensions>>(new Map())
  const flow = useReactFlow()
  const onNodesChange = useCallback((changes: NodeChange<AgentFlowNode>[]) => {
    const sizes = changes.flatMap((c) =>
      c.type === 'dimensions' && c.dimensions ? [[c.id, c.dimensions] as const] : [],
    )
    if (sizes.length > 0) setMeasured((m) => new Map([...m, ...sizes]))
  }, [])
  // The flow fits once, on its first draw; this re-fits on every later canvas size: the console
  // opening or closing, a handle drag, a window resize, a return from Audit. A hidden canvas
  // reports 0x0 and fits nothing; its next real size fits.
  const refitOnResize = useCallback(
    (box: HTMLDivElement) => {
      let last: string | null | undefined
      let timer: ReturnType<typeof setTimeout> | undefined
      const observer = new ResizeObserver(([entry]) => {
        const { width, height } = (entry as ResizeObserverEntry).contentRect
        const size = width > 0 && height > 0 ? `${width}x${height}` : null
        if (size === last) return
        const first = last === undefined
        last = size
        clearTimeout(timer)
        if (!first && size !== null) {
          timer = setTimeout(() => void flow.fitView(FIT_VIEW_OPTIONS), REFIT_MS)
        }
      })
      observer.observe(box)
      return () => {
        observer.disconnect()
        clearTimeout(timer)
      }
    },
    [flow],
  )
  const onNodeClick = useCallback(
    (_: unknown, node: AgentFlowNode) => onSelect(Number(node.id)),
    [onSelect],
  )
  const failure = error === null ? null : `Couldn't lay out the graph: ${error}`
  // A failed first layout has nothing to keep drawing.
  if (positions === null) {
    return (
      failure && (
        <div className="p-4 pt-16">
          <EmptyState message={failure} />
        </div>
      )
    )
  }
  // While a relayout runs, nodes that joined wait for their position.
  const nodes: AgentFlowNode[] = graph.agents.flatMap((agent) => {
    const position = positions.get(agent.id)
    return position
      ? [
          {
            id: String(agent.id),
            type: 'agent' as const,
            position,
            measured: measured.get(String(agent.id)),
            data: { agent, selected: agent.id === selected },
          },
        ]
      : []
  })
  const edges: Edge[] = drawnEdges(graph)
    .filter((e) => positions.has(e.parent) && positions.has(e.child))
    .map((e) => ({
      id: `${e.kind}:${e.parent}>${e.child}`,
      source: String(e.parent),
      target: String(e.child),
      type: 'step',
      style: EDGE_STYLES[e.kind],
    }))
  return (
    <>
      <ReactFlow
        ref={refitOnResize}
        nodes={nodes}
        edges={edges}
        nodeTypes={nodeTypes}
        onNodesChange={onNodesChange}
        // A handler keeps pointer events on the otherwise inert nodes; a
        // card's click (or Enter) bubbles up to it.
        onNodeClick={onNodeClick}
        colorMode="dark"
        fitView
        fitViewOptions={FIT_VIEW_OPTIONS}
        minZoom={0.3}
        maxZoom={2}
        nodesDraggable={false}
        nodesConnectable={false}
        nodesFocusable={false}
        edgesFocusable={false}
        elementsSelectable={false}
        deleteKeyCode={null}
        // The default (Space) swallows Space app-wide.
        panActivationKeyCode={null}
        className="bg-transparent"
      >
        <Background
          variant={BackgroundVariant.Dots}
          gap={20}
          size={1}
          color="rgb(63 63 70 / 0.5)"
        />
      </ReactFlow>
      {failure && (
        // A failed relayout keeps the last drawn graph up, under this line.
        <p role="alert" className="absolute bottom-3 left-4 z-10 text-xs text-destructive">
          {failure}
        </p>
      )}
      <div className="absolute right-3 top-3 z-10 flex gap-0.5 rounded-md border border-border bg-surface p-px">
        <Button
          variant="ghost"
          size="icon"
          aria-label="Zoom out"
          onClick={() => void flow.zoomOut()}
          className="size-7"
        >
          <Minus aria-hidden className="size-4" />
        </Button>
        <Button
          variant="ghost"
          size="icon"
          aria-label="Zoom in"
          onClick={() => void flow.zoomIn()}
          className="size-7"
        >
          <Plus aria-hidden className="size-4" />
        </Button>
      </div>
    </>
  )
}

/** A session's latest run time and when this canvas first saw it. */
interface FirstSeen {
  runMs: number | null
  at: number
}

interface SessionHeaderProps {
  ws: Workstream
  graph: LiveGraph | null
  /** Per session id; one entry each, replaced when its run time moves. */
  firstSeen: Map<string, FirstSeen>
}

/** When `graph`'s session was first seen at its current run time; now for one not seen yet. */
function seenAt(firstSeen: Map<string, FirstSeen>, graph: LiveGraph | null): number {
  if (graph === null) return Date.now()
  const seen = firstSeen.get(graph.sessionId)
  if (seen?.runMs === graph.runMs) return seen.at
  const at = Date.now()
  firstSeen.set(graph.sessionId, { runMs: graph.runMs, at })
  return at
}

/** The tab's name and the one line over the canvas. */
function SessionHeader({ ws, graph, firstSeen }: SessionHeaderProps) {
  return (
    <div className="pointer-events-none absolute left-4 top-3.5 z-10 flex flex-col gap-0.5">
      <h2 className="font-mono text-sm font-semibold">{ws.name}</h2>
      {/* A reread with no new run time (a store change elsewhere) or a return from another tab
          keeps the count going; a new run time starts it again. */}
      <SessionText
        key={`${graph?.sessionId}:${graph?.runMs}`}
        ws={ws}
        graph={graph}
        firstSeen={firstSeen}
      />
    </div>
  )
}

/** The session line, its run time ticking on from when it was first seen, as its minutes change. */
function SessionText({ ws, graph, firstSeen }: SessionHeaderProps) {
  const [readAt] = useState(() => seenAt(firstSeen, graph))
  const now = useClock(readAt - (graph?.runMs ?? 0))
  return (
    <p className="text-xs tabular-nums text-muted-foreground">
      {sessionLine(ws, graph, now - readAt)}
    </p>
  )
}
