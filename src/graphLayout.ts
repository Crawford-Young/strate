// Positions for the live graph: ELK `layered`, top-down, with pinning. Pure,
// and handed its ELK, so tests run it on the in-process bundle while the app
// runs it in a web worker (./elk).
import type { ElkNode } from 'elkjs/lib/elk-api'
import type { LiveGraph } from './api'

/** A node card's footprint (the card is `w-48`). */
export const NODE_WIDTH = 192
export const NODE_HEIGHT = 80
/** Space between neighbouring cards in a tier. */
export const NODE_GAP = 24

export interface Point {
  x: number
  y: number
}

/** Top-left corner per agent id. */
export type Positions = ReadonlyMap<number, Point>

/** A layout, and the parents (incoming edges) each agent had when it was placed. */
export interface Layout {
  positions: Positions
  parents: ReadonlyMap<number, string>
}

/** The slice of an ELK instance the layout needs. */
export interface ElkLike {
  layout(graph: ElkNode): Promise<ElkNode>
}

const LAYOUT_OPTIONS = {
  'elk.algorithm': 'layered',
  'elk.direction': 'DOWN',
  'elk.spacing.nodeNode': String(NODE_GAP),
  'elk.layered.spacing.nodeNodeBetweenLayers': '64',
  // Centres each parent over its children, as in the mockup.
  'elk.layered.nodePlacement.bk.fixedAlignment': 'BALANCED',
}

type Structure = Pick<LiveGraph, 'agents' | 'edges'>

/** The edges between agents that are in the graph. */
export function drawnEdges(graph: Structure) {
  const ids = new Set(graph.agents.map((a) => a.id))
  return graph.edges.filter((e) => ids.has(e.parent) && ids.has(e.child))
}

/** Changes only when an agent or an edge is added or removed. */
export function structureKey(graph: Structure): string {
  const agents = graph.agents.map((a) => a.id).sort((a, b) => a - b)
  const edges = drawnEdges(graph)
    .map((e) => `${e.kind}:${e.parent}>${e.child}`)
    .sort()
  return `${agents.join(',')}|${edges.join(',')}`
}

/** Each agent's parents as a key; empty for a root. */
export function parentsOf(graph: Structure): ReadonlyMap<number, string> {
  const parents = new Map<number, number[]>(graph.agents.map((a) => [a.id, []]))
  for (const e of drawnEdges(graph)) parents.get(e.child)?.push(e.parent)
  return new Map([...parents].map(([id, ps]) => [id, ps.sort((a, b) => a - b).join(',')]))
}

const overlap = (a: Point, b: Point) =>
  Math.abs(a.x - b.x) < NODE_WIDTH && Math.abs(a.y - b.y) < NODE_HEIGHT

/**
 * Lays `graph` out. Agents `previous` placed keep their position exactly while
 * their parents are unchanged (an agent can show up before its dispatch edge,
 * as a root); the rest take ELK's, nudged right past any node they would overlap.
 */
export async function layoutGraph(
  elk: ElkLike,
  graph: Structure,
  previous?: Layout,
): Promise<Layout> {
  const laid = await elk.layout({
    id: 'root',
    layoutOptions: LAYOUT_OPTIONS,
    children: graph.agents.map((a) => ({
      id: String(a.id),
      width: NODE_WIDTH,
      height: NODE_HEIGHT,
    })),
    edges: drawnEdges(graph).map((e) => ({
      id: `${e.parent}>${e.child}`,
      sources: [String(e.parent)],
      targets: [String(e.child)],
    })),
  })
  const parents = parentsOf(graph)
  const kept = previous
    ? [...previous.positions].filter(
        ([id]) => parents.has(id) && previous.parents.get(id) === parents.get(id),
      )
    : []
  const positions = new Map(kept)
  const placed = [...positions.values()]
  for (const child of laid.children ?? []) {
    const id = Number(child.id)
    if (positions.has(id)) continue
    const point = { x: child.x ?? 0, y: child.y ?? 0 }
    for (let hit = placed.find((p) => overlap(p, point)); hit;) {
      point.x = hit.x + NODE_WIDTH + NODE_GAP
      hit = placed.find((p) => overlap(p, point))
    }
    positions.set(id, point)
    placed.push(point)
  }
  return { positions, parents }
}
