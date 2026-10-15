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
  /** The live workstream whose tab this one nests under; null for a top-level tab. */
  director: number | null
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
  /** Of an earlier `/clear` link of the drawn session's chain (or a unit's); absent when not. */
  earlier?: boolean
}

export interface GraphEdge {
  kind: 'dispatch' | 'teammate' | 'directs'
  parent: number
  child: number
}

/** The live session of a workstream, with its `/clear` chain's earlier links' agents. */
export interface LiveGraph {
  sessionId: string
  live: boolean
  /** The priced requests of the drawn chain's links in this workstream that count toward it; null with none. */
  sessionCostUsd: number | null
  /** Those links' wall times summed, each first record to last, as of this read. */
  runMs: number | null
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
  'root' | 'group' | 'repo' | 'unlinked' | 'epic' | 'issue' | 'pr' | 'session' | 'agent'

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
  /**
   * `group` is a value of the group-by: a day, `<repo>#<pr>` (the PR's own
   * repo), an agent type or a model. `session` is a `/clear` chain's windows in one
   * workstream; a director's unit nests under the director's row.
   */
  kind: NodeKind
  /** Unique among its siblings. */
  key: string
  label: string
  /** An epic, issue or PR read from the GitHub cache; false for an issue known only from a window name. */
  github: boolean
  metrics: Metrics
  children: AuditNode[]
}

/** The issues closed as completed and the merged PRs with work in the filter. */
export interface Headline {
  issuesClosed: number
  /** Over each issue's whole life, first window ever to close. */
  medianElapsedMs: number | null
  /** Their $ in the filter over their count. */
  usdPerClosedIssue: number | null
  /** Their $ in the filter over their count; a PR closing several issues counts once. */
  usdPerMergedPr: number | null
  /** Cloud sessions are not in the store (#29), so their spend is missing. */
  cloudUncounted: boolean
}

/** A repo (slug) whose latest GitHub refresh failed, and why. */
export interface GithubError {
  repo: string
  error: string
}

export interface Audit {
  root: AuditNode
  headline: Headline
  /** The repos whose GitHub data is missing or stale. */
  githubErrors: GithubError[]
}

/** An Audit node's `[kind, key]` steps from the root (the root's own may lead). */
export type AuditPath = [NodeKind, string][]

/**
 * `active`: the orchestrator at work. `wait`: it waits on you. `agent`: a
 * subagent at work; parallel ones overlap.
 */
export type SpanKind = 'active' | 'wait' | 'agent'

/** `[startMs, endMs)` on the day's wall clock (ms from 00:00 to the clock time), even on a DST day. */
export interface TimelineSpan {
  startMs: number
  endMs: number
  kind: SpanKind
}

/** `#N closed` or `PR #N merged`, on the day's wall clock. */
export interface TimelineMark {
  atMs: number
  label: string
}

/** One local day of a node's timeline: spans by start, marks by time. */
export interface TimelineDay {
  /** Local day, `YYYY-MM-DD`. */
  day: string
  spans: TimelineSpan[]
  marks: TimelineMark[]
}

/**
 * One ISO week of the range: the issues closed as completed and the PRs merged in it,
 * each at its whole-life cost (all its `<repo>-<issue>` work, in the range or not).
 */
export interface OutcomeWeek {
  /** Its Monday, `YYYY-MM-DD`. */
  week: string
  /** Its days inside the range: fewer than 7 at either end. */
  days: number
  issuesClosed: number
  usdPerClosedIssue: number | null
  /** Orchestrator active time, not subagent time. */
  activeMsPerClosedIssue: number | null
  /** With linked work; a PR closing several issues counts once. */
  prsMerged: number
  usdPerMergedPr: number | null
  /** Everything spent on its days in the range. */
  spendUsd: number
}

export interface Outcome {
  /** Every ISO week the range touches, in order. */
  weeks: OutcomeWeek[]
  /** The range's $ in windows with no `<repo>-<issue>` name. */
  unlinkedUsd: number
  unlinkedWorkstreams: number
  /** The range's $ on issues not closed (or not in the GitHub cache). */
  openUsd: number
  openIssues: number
}

/** One agent type on one model, or the orchestrator on one model. */
export interface AgentRow {
  /** `orchestrator` on an orchestrator row; `""` when unknown. */
  agentType: string
  /** `""` when unknown. */
  model: string
  /** An orchestrator row: no dispatches, so every count is 0. */
  orchestrator: boolean
  /** Each dispatch with a cell in the range, once. */
  dispatches: number
  /** Completed with no `NEEDS_CONTEXT:` line in the final report. */
  completed: number
  blocked: number
  /** Failed or killed. */
  failed: number
  /** Never notified, and quiet for over 30 minutes before the store's newest record. */
  noReport: number
  /** Never notified, last heard within those 30 minutes: no outcome yet. */
  running: number
  /** SendMessage calls to these dispatches. */
  followups: number
  costUsd: number
  usdPerDispatch: number | null
  tokens: Tokens
  /** Cache reads over all input: fresh, read and written. */
  cacheHitPct: number | null
}

/** Your time on one local day in one repo's windows (`null`: Unlinked). */
export interface BottleneckDay {
  day: string
  repo: string | null
  /** Answering questions and plan approvals. */
  askMs: number
  /** From the last reply to your next prompt. */
  promptWaitMs: number
  /** Proxy: a blocked dispatch's last record to its parent's next prompt or dispatch. */
  blockedMs: number
  /** The orchestrator's active time. */
  activeMs: number
  blockedReports: number
  /** Permission prompts that started this day: the answered ones' waits. */
  permissionMs: number
  /** Those prompts, open ones included. */
  permissionPrompts: number
}

/** What the PermissionRequest hook has recorded (#52). */
export interface PermissionWaits {
  /** The local day of the store's first recorded prompt, whatever the filter; null: none ever. */
  recordedSince: string | null
  /** Prompts in the range still unanswered: counted in `permissionPrompts`, not `permissionMs`. */
  open: number
}

export interface Bottleneck {
  /** By day, then repo; Unlinked last. */
  days: BottleneckDay[]
  permissionWaits: PermissionWaits
}

export interface LaneCount {
  agentType: string
  model: string
  count: number
}

export interface Peak {
  pct: number
  model: string
}

export interface Waste {
  /** SendMessage calls to a dispatch, of `sendMessages` in all. */
  followups: number
  sendMessages: number
  /** Of `dispatches`. */
  blocked: number
  dispatches: number
  noReport: number
  /** The agent type and model with the most no-report dispatches. */
  noReportTop: LaneCount | null
  compactions: number
  /** `/clear` successors started in the range. */
  clears: number
  /** Agents, orchestrators too, whose deepest request reached 80% of the window. */
  contextPeaksOver80: number
  deepestPeak: Peak | null
  /** Requests retried after an API error. */
  apiRetries: number
  /** API error messages, prompt-too-long ones included. */
  apiErrors: number
  promptTooLong: number
  /** Issues with work in the range that GitHub saw reopened. */
  reopenedIssues: number
  /** Merged revert PRs, merged in the range. */
  revertedPrs: number
  /** The repos with GitHub data the two counts above cover. */
  repos: number
}

/** The Audit performance read: every section under the same filter (days, repos). */
export interface Performance {
  outcome: Outcome
  /** Orchestrator rows first, then by $ (most first). */
  agents: AgentRow[]
  bottleneck: Bottleneck
  waste: Waste
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

/** The node's days in the filter; empty for an unknown path, rejects on a bad day. */
export const auditTimeline = (filter: AuditFilter, path: AuditPath) =>
  invoke<TimelineDay[]>('audit_timeline', { filter, path })

/** The performance read under `filter` (its `groupBy` is ignored); rejects on a bad day. */
export const auditPerformance = (filter: AuditFilter) =>
  invoke<Performance>('audit_performance', { filter })

/**
 * The Audit CSV export: the app asks where to save it in a native dialog, offering
 * `suggestedName`, and writes `text` there. The webview never names a path. False when the
 * user cancels; rejects with the I/O message.
 */
export const writeExport = (text: string, suggestedName: string) =>
  invoke<boolean>('write_export', { text, suggestedName })

/**
 * Asks the engine to refresh the GitHub cache and resolves once the fetch is
 * stored, so a read after it sees the new rows. A missing or failing `gh` is not an
 * error: its repos show in `Audit.githubErrors`, their issues with no GitHub data.
 */
export const auditRefresh = (): Promise<void> => invoke('audit_refresh')

/** Calls `callback` after each (debounced) store change; resolves to the unlisten. */
export function onStoreChanged(callback: () => void): Promise<UnlistenFn> {
  return listen(STORE_CHANGED, () => callback())
}
