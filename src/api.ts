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
  /** The agent's resolved context nudge, in percent (gear, default 80). */
  nudgePct: number
  /** The agent's resolved context hard stop, in percent (gear, default 95). */
  hardStopPct: number
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

/** How the Audit tree's top level groups the cells. */
export type GroupBy = 'hierarchy' | 'day' | 'pr' | 'agent_type' | 'model'

/** What auditRollup sums: the cells on local days `from..=to`. */
export interface AuditFilter {
  /** Local day, `YYYY-MM-DD`. */
  from: string
  /** Local day, `YYYY-MM-DD`, inclusive. */
  to: string
  /** Repo slugs; empty means every window, Unlinked included. */
  repos: string[]
  groupBy: GroupBy
}

export type NodeKind =
  | 'root'
  /** A value of the group-by: a day, `<repo>#<pr>`, an agent type or a model. */
  | 'group'
  | 'repo'
  | 'unlinked'
  | 'epic'
  | 'issue'
  | 'pr'
  /** One `/rename` window. */
  | 'session'
  | 'agent'

export interface Tokens {
  input: number
  output: number
  cacheRead: number
  cacheWrite: number
}

/** An epic's closed and total sub-issues, from GitHub. */
export interface Progress {
  completed: number
  total: number
}

export interface Metrics {
  /** Priced requests only; `unpriced` counts the rest. */
  costUsd: number
  unpriced: number
  tokens: Tokens
  /** Agent-hours: subagent wall time. */
  agentMs: number
  /** Some of `agentMs` is a background subagent's, timed approximately (#42). */
  approximate: boolean
  activeMs: number
  /** Waiting on you. */
  waitMs: number
  /** Windows. */
  sessions: number
  /** Subagents. */
  agents: number
  /** Windows named `-sN`. */
  rotations: number
  /** Null when the node has no start. */
  elapsedMs: number | null
  /** Elapsed runs to the last activity: the issue, epic or PR is open. */
  open: boolean
  /** On an epic, or an issue with sub-issues. */
  progress: Progress | null
}

export interface AuditNode {
  kind: NodeKind
  /** Unique among its siblings. */
  key: string
  label: string
  /** An epic, issue or PR read from the GitHub cache; false for an issue known only from a window name. */
  github: boolean
  metrics: Metrics
  children: AuditNode[]
}

/** The closed issues and merged PRs with work in the filter. */
export interface Headline {
  issuesClosed: number
  medianElapsedMs: number | null
  /** Their $ over their count. */
  usdPerClosedIssue: number | null
  usdPerMergedPr: number | null
  /** Cloud sessions are not in the store (#29), so their spend is missing. */
  cloudUncounted: boolean
}

export interface Audit {
  root: AuditNode
  headline: Headline
}

export const status = () => invoke<Status>('status')

export const workstreams = () => invoke<Workstream[]>('workstreams')

export const workstreamCosts = () => invoke<WorkstreamCost[]>('workstream_costs')

export const needsYou = () => invoke<NeedsYou[]>('needs_you')

/** Null when the workstream has no session. */
export const liveGraph = (workstream: number) =>
  invoke<LiveGraph | null>('live_graph', { workstream })

/** Rejects with the message on a bad day in the filter. */
export const auditRollup = (filter: AuditFilter) => invoke<Audit>('audit_rollup', { filter })

/**
 * Asks the engine to refresh the GitHub cache and resolves at once; refetch
 * on `store-changed` when it lands. A missing or failing `gh` is not an
 * error: it shows as unlinked issues.
 */
export const auditRefresh = (): Promise<void> => invoke('audit_refresh')

/** Calls `callback` after each (debounced) store change; resolves to the unlisten. */
export function onStoreChanged(callback: () => void): Promise<UnlistenFn> {
  return listen(STORE_CHANGED, () => callback())
}
