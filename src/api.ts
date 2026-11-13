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
  /** A parked background job's id in `claude agents --json`, for `claude attach <id>`; else null. */
  bgId: string | null
}

/** How an agent runs, kebab-case. Lanes added later (e.g. `cloud`) arrive as their own string. */
export type Lane = 'subagent' | 'background-session' | 'interactive' | (string & {})

export type ChargeKind = 'repo' | 'epic' | 'issue' | 'pr' | 'topic'

/** Where an agent's charge came from; `unlinked` with no charge. */
export type ChargeSource = 'branch' | 'name' | 'parent' | 'unlinked'

/** What an agent works on: `repo` and `number` as the kind needs, `topic` only for a topic. */
export interface Charge {
  kind: ChargeKind
  repo: string | null
  number: number | null
  topic: string | null
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
  /** The agent row's session. */
  sessionId: string
  /** The role its name gives; null when it names none. */
  role: string | null
  lane: Lane
  /** Its effective charge; null when Unlinked. */
  charge: Charge | null
  /** The charge's join key, e.g. `issue_strate-109`; null when Unlinked. */
  chargeKey: string | null
  chargeSource: ChargeSource
  /**
   * On a drawn chain's orchestrator, every link's orchestrator of its `/clear` chain, itself
   * included, in start order: an earlier link's id names this node. Empty on a worker.
   */
  links: number[]
}

/**
 * `directs` is a spawned session's orchestrator under its spawner, across sessions. Kinds added
 * later arrive as their own string.
 */
export interface GraphEdge {
  kind: 'dispatch' | 'teammate' | 'directs' | (string & {})
  parent: number
  child: number
}

/** The live session of a workstream and its directed units, with their running workers. */
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

export type TranscriptToolStatus = 'pending' | 'success' | 'error'

export type TranscriptTodoStatus = 'pending' | 'in_progress' | 'completed'

export interface TranscriptTodo {
  content: string
  status: TranscriptTodoStatus
}

/**
 * One record of an agent's transcript, at its record's time (ISO; null when it has none). `key`
 * names it for good: its event id and its place among that record's entries.
 */
export type TranscriptEntry = { key: string; timestamp: string | null } & (
  | /** A human prompt; `sender` names who sent a teammate or cross-session one. */ {
      kind: 'user'
      text: string
      sender: string | null
    }
  | /** An assistant text block. */ { kind: 'text'; text: string }
  | /** Non-empty thinking; redacted thinking is skipped. */ { kind: 'thinking'; text: string }
  | {
      kind: 'tool_call'
      id: string
      name: string
      /** The one-line `Tool(args)` summary. */
      args: string
      /** `pending` until its result arrives. */
      status: TranscriptToolStatus
      /** The result text, capped at 4000 characters. */
      result: string | null
      resultTruncated: boolean
      /** When the result arrived (ISO). */
      resultAt: string | null
      /** TodoWrite's todos; null for every other tool. */
      todos: TranscriptTodo[] | null
    }
)

/** A tool result whose call is not in its page: an earlier page's call, or none (an orphan). */
export interface TranscriptResult {
  /** The tool_use id it answers. */
  id: string
  status: Exclude<TranscriptToolStatus, 'pending'>
  /** Capped at 4000 characters. */
  result: string
  resultTruncated: boolean
  /** When the result arrived (ISO). */
  resultAt: string | null
}

/** What an agent's transcript gained after a cursor. */
export interface TranscriptPage {
  /** The new records' entries, their calls completed by the new results. */
  entries: TranscriptEntry[]
  /** The new results for calls not in `entries`; each completes the last call with its id. */
  updates: TranscriptResult[]
  /** The last event id read: the next read's cursor. The given one when nothing was read. */
  cursor: number | null
}

/** What auditRollup sums: the cells on local days `from..=to`. */
export interface AuditFilter {
  /** Local day, `YYYY-MM-DD`. */
  from: string
  /** Local day, `YYYY-MM-DD`, inclusive. */
  to: string
  /** Repo slugs, `''` for Unlinked windows; empty means every window. */
  repos: string[]
}

export type NodeKind = 'root' | 'repo' | 'topic' | 'unlinked' | 'epic' | 'issue' | 'pr' | 'agent'

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
  /** Sessions, once per charge. */
  sessions: number
  /** Subagents. */
  agents: number
  /** Null when the node has no start. */
  elapsedMs: number | null
  /** Elapsed runs to the last activity: the issue, epic or PR is open. */
  open: boolean
  /** On an epic, or an issue with sub-issues. */
  progress: Progress | null
  /** By local day. */
  byDay: Slice[]
  /**
   * By role: the agent's parsed role if set; else a subagent's or teammate's type (`teammate`
   * when none is recorded); else `main` for a main agent; else `''` (unknown).
   */
  byRole: Slice[]
  /** By model (`''`: a cell with no requests). */
  byModel: Slice[]
}

/** A node's $ and agent-hours under one key of a split; a split's slices are in key order. */
export interface Slice {
  key: string
  costUsd: number
  agentMs: number
}

export interface AuditNode {
  /**
   * `topic` is a top-level node per topic charge. `agent` is a main agent (the main agent of one
   * `/clear` chain's sessions within one charge, keyed `<process root>/<charge key>`) or a
   * subagent (keyed `<session id>/<agent id>`), each main agent followed by its chain's subagents.
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
  /** The repos the range's windows are in, sorted, whatever the repo filter. */
  repos: string[]
  /** Some window in the range is in no repo. */
  unlinked: boolean
}

/** An Audit node's `[kind, key]` steps from the root (the root's own may lead). */
export type AuditPath = [NodeKind, string][]

/**
 * `active`: the main agent at work. `wait`: it waits on you. `agent`: a
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
 * One local day of the range: the issues closed as completed and the PRs merged on it,
 * each at its whole-life cost (all its `<repo>-<issue>` work, in the range or not).
 */
export interface OutcomeDay {
  /** `YYYY-MM-DD`. */
  day: string
  issuesClosed: number
  usdPerClosedIssue: number | null
  /** Main-agent active time, not subagent time. */
  activeMsPerClosedIssue: number | null
  /** With linked work; a PR closing several issues counts once. */
  prsMerged: number
  usdPerMergedPr: number | null
  /** Everything spent on it. */
  spendUsd: number
}

export interface Outcome {
  /** Every local day of the range, in order, the quiet ones too. */
  days: OutcomeDay[]
  /** The range's $ on no issue or epic charge. */
  unlinkedUsd: number
  /** Charges whose $ is on no issue or epic (Unlinked counting as one). */
  unlinkedCharges: number
  /** The range's $ on issues not closed (or not in the GitHub cache). */
  openUsd: number
  openIssues: number
}

/** One role on one model, main agents apart. */
export interface AgentRow {
  /** As `Metrics.byRole`: `main` on a main agent with no parsed role; `''` when unknown. */
  role: string
  /** `''` when unknown. */
  model: string
  /** A main-agent row (a session's main agent): no dispatches, so every count is 0. */
  main: boolean
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
  /** The main agent's active time. */
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
  /** As `AgentRow.role`. */
  role: string
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
  /** The role and model with the most no-report dispatches. */
  noReportTop: LaneCount | null
  compactions: number
  /** `/clear` successors started in the range. */
  clears: number
  /** Agents, main agents too, whose deepest request reached 80% of the window. */
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
  /** Main-agent rows first, then by $ (most first). */
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

/**
 * What the agent row's own transcript records gained after event `cursor` (null: from the start),
 * every tool call included; empty when unknown.
 */
export const agentTranscript = (agent: number, cursor: number | null) =>
  invoke<TranscriptPage>('agent_transcript', { agent, cursor })

/** Rejects with the message on a bad day in the filter. */
export const auditRollup = (filter: AuditFilter) => invoke<Audit>('audit_rollup', { filter })

/** The node's days in the filter; empty for an unknown path, rejects on a bad day. */
export const auditTimeline = (filter: AuditFilter, path: AuditPath) =>
  invoke<TimelineDay[]>('audit_timeline', { filter, path })

/** The performance read under `filter`; rejects on a bad day. */
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
