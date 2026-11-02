// The Audit and performance reads' shapes and their Tauri command wrappers (strate#110): the
// charge tree (no session level), splits by role, and root-agent rows. They mirror
// strate_core::store::audit (serialized camelCase); the shapes this does not change come from
// ./api and are re-exported, so the Audit UI imports from here alone.
import { invoke } from '@tauri-apps/api/core'
import type {
  AuditFilter,
  Bottleneck,
  Headline,
  GithubError,
  Outcome,
  Peak,
  Progress,
  Slice,
  TimelineDay,
  Tokens,
} from './api'

export type {
  AuditFilter,
  Bottleneck,
  BottleneckDay,
  GithubError,
  Headline,
  Outcome,
  OutcomeDay,
  Peak,
  PermissionWaits,
  Progress,
  Slice,
  SpanKind,
  TimelineDay,
  TimelineMark,
  TimelineSpan,
  Tokens,
} from './api'

export type NodeKind = 'root' | 'repo' | 'topic' | 'unlinked' | 'epic' | 'issue' | 'pr' | 'agent'

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
   * when none is recorded); else `root` for a root agent; else `''` (unknown).
   */
  byRole: Slice[]
  /** By model (`''`: a cell with no requests). */
  byModel: Slice[]
}

export interface AuditNode {
  /**
   * `topic` is a top-level node per topic charge. `agent` is a root agent (the main agent of one
   * `/clear` chain's sessions within one charge, keyed `<process root>/<charge key>`) or a
   * subagent (keyed `<session id>/<agent id>`), each root agent followed by its chain's subagents.
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

/** One role on one model, root agents apart. */
export interface AgentRow {
  /** As `Metrics.byRole`: `root` on a root agent with no parsed role; `''` when unknown. */
  role: string
  /** `''` when unknown. */
  model: string
  /** A root-agent row (a session's main agent): no dispatches, so every count is 0. */
  root: boolean
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

export interface LaneCount {
  /** As `AgentRow.role`. */
  role: string
  model: string
  count: number
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
  /** Agents, root agents too, whose deepest request reached 80% of the window. */
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
  /** Root-agent rows first, then by $ (most first). */
  agents: AgentRow[]
  bottleneck: Bottleneck
  waste: Waste
}

/** Rejects with the message on a bad day in the filter. */
export const auditRollup = (filter: AuditFilter) => invoke<Audit>('audit_rollup', { filter })

/** The node's days in the filter; empty for an unknown path, rejects on a bad day. */
export const auditTimeline = (filter: AuditFilter, path: AuditPath) =>
  invoke<TimelineDay[]>('audit_timeline', { filter, path })

/** The performance read under `filter`; rejects on a bad day. */
export const auditPerformance = (filter: AuditFilter) =>
  invoke<Performance>('audit_performance', { filter })
