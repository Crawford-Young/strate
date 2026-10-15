// A stand-in for React Flow's renderer. happy-dom has no layout, so the real
// canvas never measures its nodes and never draws them. This one renders each
// node through its custom node component, with its position as data
// attributes, lists the edges, and records zoom calls and the flow's props.
// Use it as `vi.mock('@xyflow/react', ...)` spread over the real module, which
// keeps the enums (Position, BackgroundVariant) real.
import type { BackgroundProps, HandleProps, ReactFlowProps } from '@xyflow/react'
import { useLayoutEffect, useRef, type ComponentType, type ReactNode, type Ref } from 'react'
import { vi } from 'vitest'

/** Every zoom button press, in order. */
export const zoomCalls: ('in' | 'out')[] = []

const renders: ReactFlowProps[] = []

/** The props the flow last rendered with. */
export const flowProps = (): ReactFlowProps => renders.at(-1) ?? {}

/** Every `setCenter` call: where the view was centred, at what zoom. */
export const centerCalls: { x: number; y: number; zoom?: number }[] = []

/** Every `fitView` call, as the flow's own re-fit would make it. */
export const fitView = vi.fn<(options?: unknown) => Promise<boolean>>(() => Promise.resolve(true))

export function resetFlow() {
  fitView.mockClear()
  zoomCalls.length = 0
  centerCalls.length = 0
  renders.length = 0
}

// One instance, as the real hook returns a stable one.
const flow = {
  zoomIn: () => Promise.resolve(zoomCalls.push('in') > 0),
  zoomOut: () => Promise.resolve(zoomCalls.push('out') > 0),
  getZoom: () => 1,
  flowToScreenPosition: (p: { x: number; y: number }) => p,
  setCenter: (x: number, y: number, options?: { zoom?: number }) =>
    Promise.resolve(centerCalls.push({ x, y, zoom: options?.zoom }) > 0),
  fitView,
}

export const useReactFlow = () => flow

export function ReactFlowProvider({ children }: { children?: ReactNode }) {
  return <>{children}</>
}

type NodeComponent = ComponentType<{
  id: string
  data: unknown
  positionAbsoluteX: number
  positionAbsoluteY: number
}>

/** A node's measured size, as the real flow reports it once per mounted node. */
export const MEASURED = { width: 192, height: 74 }

/**
 * Three contracts of the real flow are kept here:
 * - Its `ref` is the root element, so a caller can observe the canvas box.
 * - It measures each node once and reports the size through `onNodesChange`.
 *   A controlled node passed back without that `measured` stays hidden
 *   (`data-measured="false"`), and its edges are not drawn.
 * - A node that is neither selectable, draggable, nor has a click or hover
 *   handler gets `pointer-events: none`; clicks never reach its content.
 */
export function ReactFlow({ ref, ...rest }: ReactFlowProps & { ref?: Ref<HTMLDivElement> }) {
  const props: ReactFlowProps = rest
  const reported = useRef(new Set<string>())
  const { nodes = [], edges = [], nodeTypes = {}, children, onNodesChange, onNodeClick } = props
  useLayoutEffect(() => {
    renders.push(props)
    const fresh = nodes.filter((n) => !reported.current.has(n.id))
    fresh.forEach((n) => reported.current.add(n.id))
    if (fresh.length > 0) {
      onNodesChange?.(
        fresh.map((n) => ({ id: n.id, type: 'dimensions', dimensions: { ...MEASURED } })),
      )
    }
  })
  const measured = new Set(nodes.filter((n) => n.measured?.width).map((n) => n.id))
  return (
    <div ref={ref} data-testid="flow" className="react-flow">
      {nodes.map((n) => {
        const Node = nodeTypes[n.type as string] as unknown as NodeComponent
        const live =
          (n.selectable ?? props.elementsSelectable ?? true) ||
          (n.draggable ?? props.nodesDraggable ?? true) ||
          !!onNodeClick ||
          !!props.onNodeMouseEnter
        return (
          <div
            key={n.id}
            data-testid={`node-${n.id}`}
            data-x={n.position.x}
            data-y={n.position.y}
            data-measured={String(measured.has(n.id))}
            data-pointer-events={live ? 'all' : 'none'}
            onClickCapture={live ? undefined : (e) => e.stopPropagation()}
            onClick={(e) => onNodeClick?.(e, n)}
          >
            <Node
              id={n.id}
              data={n.data}
              positionAbsoluteX={n.position.x}
              positionAbsoluteY={n.position.y}
            />
          </div>
        )
      })}
      {edges
        .filter((e) => measured.has(e.source) && measured.has(e.target))
        .map((e) => (
          <div
            key={e.id}
            data-testid={`edge-${e.source}-${e.target}`}
            data-type={e.type}
            data-stroke={e.style?.stroke}
            data-dash={e.style?.strokeDasharray}
          />
        ))}
      {children}
    </div>
  )
}

export function Background({ variant, gap, size, color }: BackgroundProps) {
  return (
    <div
      data-testid="background"
      data-variant={variant}
      data-gap={String(gap)}
      data-size={size}
      data-color={color}
    />
  )
}

export function Handle({ type, position, isConnectable }: HandleProps) {
  return (
    <span
      data-testid={`handle-${type}`}
      data-position={position}
      data-connectable={String(isConnectable)}
    />
  )
}
