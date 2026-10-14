// Builders for Audit rollups in tests: every metric zero unless a test sets it.
import type { Audit, AuditNode, Headline, Metrics, NodeKind } from '../src/api'

export function metrics(over: Partial<Metrics> = {}): Metrics {
  return {
    costUsd: 0,
    unpriced: 0,
    tokens: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0 },
    agentMs: 0,
    activeMs: 0,
    waitMs: 0,
    sessions: 0,
    agents: 0,
    rotations: 0,
    elapsedMs: null,
    open: false,
    progress: null,
    ...over,
  }
}

export function node(
  kind: NodeKind,
  key: string,
  over: Partial<Metrics> = {},
  children: AuditNode[] = [],
  extra: Partial<Pick<AuditNode, 'label' | 'github'>> = {},
): AuditNode {
  return { kind, key, label: key, github: false, metrics: metrics(over), children, ...extra }
}

export const headline = (over: Partial<Headline> = {}): Headline => ({
  issuesClosed: 0,
  medianElapsedMs: null,
  usdPerClosedIssue: null,
  usdPerMergedPr: null,
  cloudUncounted: false,
  ...over,
})

export const audit = (children: AuditNode[], over: Partial<Audit> = {}): Audit => ({
  root: node('root', '', {}, children),
  headline: headline(),
  githubErrors: [],
  ...over,
})
