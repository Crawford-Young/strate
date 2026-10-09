// Typed wrappers over the Tauri commands in src-tauri/src/commands.rs.
// Shapes mirror strate_core::store::views (serialized camelCase). The UI
// refetches on `store-changed` instead of polling.
import { invoke } from '@tauri-apps/api/core'
import { listen, type UnlistenFn } from '@tauri-apps/api/event'

export const STORE_CHANGED = 'store-changed'

/** `indexing` until the initial scan is stored, then `live`. */
export type Status = 'indexing' | 'live'

export type AgentState = 'working' | 'needs_you' | 'idle' | 'done'

export type AgentKind = 'orchestrator' | 'subagent' | 'teammate'

/** One orchestrator tab: a merge-resolved workstream. */
export interface Workstream {
  id: number
  name: string
  /** The name is the cwd/branch fallback, not a user-set name. */
  fallback: boolean
  /** Sum of priced requests; null with none. */
  costUsd: number | null
  unpriced: number
  state: AgentState
  needsYou: number
  /** The session liveGraph draws. */
  sessionId: string | null
  live: boolean
  /** The latest segment start (ISO). */
  startedAt: string | null
}

export interface WorkstreamCost {
  workstream: number
  costUsd: number | null
  unpriced: number
}

export interface NeedsYou {
  source: 'registry' | 'hook'
  sessionId: string | null
  agentId: string | null
  waitingFor: string | null
  sinceMs: number | null
  workstream: number | null
}

export interface GraphAgent {
  /** Row id; edges and `parent` refer to it. */
  id: number
  /** Null on the orchestrator. */
  agentId: string | null
  kind: AgentKind
  name: string | null
  agentType: string | null
  description: string | null
  model: string | null
  state: AgentState
  costUsd: number | null
  contextTokens: number | null
  /** Latest request depth over the model's window; null for an unknown window. */
  contextPct: number | null
  parent: number | null
}

export interface GraphEdge {
  kind: 'dispatch' | 'teammate'
  parent: number
  child: number
}

/** The live session of a workstream only, never earlier sessions. */
export interface LiveGraph {
  sessionId: string
  live: boolean
  agents: GraphAgent[]
  edges: GraphEdge[]
}

export const status = () => invoke<Status>('status')

export const workstreams = () => invoke<Workstream[]>('workstreams')

export const workstreamCosts = () => invoke<WorkstreamCost[]>('workstream_costs')

export const needsYou = () => invoke<NeedsYou[]>('needs_you')

/** Null when the workstream has no session. */
export const liveGraph = (workstream: number) =>
  invoke<LiveGraph | null>('live_graph', { workstream })

/** Calls `callback` after each (debounced) store change; resolves to the unlisten. */
export function onStoreChanged(callback: () => void): Promise<UnlistenFn> {
  return listen(STORE_CHANGED, () => callback())
}
