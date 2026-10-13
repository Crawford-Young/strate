// Builders for Audit performance reads in tests: every count zero unless a test sets it.
import type { AgentRow, BottleneckDay, Outcome, OutcomeWeek, Performance, Waste } from '../src/api'

export const week = (over: Partial<OutcomeWeek> = {}): OutcomeWeek => ({
  week: '2026-10-12',
  days: 7,
  issuesClosed: 0,
  usdPerClosedIssue: null,
  activeMsPerClosedIssue: null,
  prsMerged: 0,
  usdPerMergedPr: null,
  spendUsd: 0,
  ...over,
})

export const outcome = (over: Partial<Outcome> = {}): Outcome => ({
  weeks: [],
  unlinkedUsd: 0,
  unlinkedWorkstreams: 0,
  openUsd: 0,
  openIssues: 0,
  ...over,
})

export const agentRow = (over: Partial<AgentRow> = {}): AgentRow => ({
  agentType: 'implementer',
  model: 'sonnet-5-5',
  orchestrator: false,
  dispatches: 0,
  completed: 0,
  blocked: 0,
  failed: 0,
  noReport: 0,
  running: 0,
  followups: 0,
  costUsd: 0,
  usdPerDispatch: null,
  tokens: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0 },
  cacheHitPct: null,
  ...over,
})

export const bottleneckDay = (over: Partial<BottleneckDay> = {}): BottleneckDay => ({
  day: '2026-10-12',
  repo: null,
  askMs: 0,
  promptWaitMs: 0,
  blockedMs: 0,
  activeMs: 0,
  blockedReports: 0,
  ...over,
})

export const waste = (over: Partial<Waste> = {}): Waste => ({
  followups: 0,
  sendMessages: 0,
  blocked: 0,
  dispatches: 0,
  noReport: 0,
  noReportTop: null,
  compactions: 0,
  clears: 0,
  contextPeaksOver80: 0,
  deepestPeak: null,
  apiRetries: 0,
  apiErrors: 0,
  promptTooLong: 0,
  reopenedIssues: 0,
  revertedPrs: 0,
  repos: 0,
  ...over,
})

export const performance = (
  over: Partial<Omit<Performance, 'bottleneck'>> & { days?: BottleneckDay[] } = {},
): Performance => ({
  outcome: over.outcome ?? outcome(),
  agents: over.agents ?? [],
  bottleneck: { days: over.days ?? [], permissionWaits: 'not_recorded' },
  waste: over.waste ?? waste(),
})
