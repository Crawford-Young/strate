// The Audit control bar: date range, repo, group by and the chart's $ / hours toggle. `actions`
// sits at its right end (Export CSV, #27 U2).
import {
  DateRangePicker,
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
  ToggleGroup,
  ToggleGroupItem,
} from '@crawfordyoung/ui'
import type { ReactNode } from 'react'
import type { GroupBy } from './api'
import { localDay, parseDay, type Measure } from './audit'

export const PRESETS = ['7d', '14d', '30d'] as const
export type Range = (typeof PRESETS)[number] | 'custom'

export interface Controls {
  range: Range
  /** The custom range's local days, used while `range` is custom. */
  custom: { from: string; to: string }
  /** One repo slug, or null for every repo (Unlinked included). */
  repo: string | null
  groupBy: GroupBy
  measure: Measure
}

const GROUP_BY: [GroupBy, string][] = [
  ['hierarchy', 'Repo › Epic › Issue › PR'],
  ['day', 'Day'],
  ['pr', 'PR'],
  ['agent_type', 'Agent type'],
  ['model', 'Model'],
]

// Radix Select items can't have an empty value.
const ALL_REPOS = '*'

interface AuditControlsProps {
  value: Controls
  /** The current range's local days, which a switch to custom starts from. */
  days: { from: string; to: string }
  onChange: (patch: Partial<Controls>) => void
  repos: string[]
  actions?: ReactNode
}

export function AuditControls({ value, days, onChange, repos, actions }: AuditControlsProps) {
  return (
    <div className="flex flex-wrap items-center gap-3 border-b border-border px-4 py-2">
      <ToggleGroup
        type="single"
        variant="outline"
        size="sm"
        aria-label="Range"
        value={value.range}
        onValueChange={(range: Range | '') => {
          // A single group reports '' when its pressed item is clicked again; keep the range.
          if (range === 'custom') onChange({ range, custom: days })
          else if (range) onChange({ range })
        }}
      >
        {PRESETS.map((p) => (
          <ToggleGroupItem key={p} value={p}>
            {p}
          </ToggleGroupItem>
        ))}
        <ToggleGroupItem value="custom">Custom</ToggleGroupItem>
      </ToggleGroup>
      {value.range === 'custom' && (
        <DateRangePicker
          value={{ from: parseDay(value.custom.from), to: parseDay(value.custom.to) }}
          onValueChange={(range) => {
            if (range?.from)
              onChange({
                custom: { from: localDay(range.from), to: localDay(range.to ?? range.from) },
              })
          }}
        />
      )}
      <Select
        value={value.repo ?? ALL_REPOS}
        onValueChange={(repo) => onChange({ repo: repo === ALL_REPOS ? null : repo })}
      >
        <SelectTrigger aria-label="Repo" className="h-8 w-48">
          <SelectValue />
        </SelectTrigger>
        <SelectContent>
          <SelectItem value={ALL_REPOS}>All repos</SelectItem>
          {repos.map((r) => (
            <SelectItem key={r} value={r}>
              {r}
            </SelectItem>
          ))}
        </SelectContent>
      </Select>
      <Select value={value.groupBy} onValueChange={(groupBy: GroupBy) => onChange({ groupBy })}>
        <SelectTrigger aria-label="Group by" className="h-8 w-56">
          <SelectValue />
        </SelectTrigger>
        <SelectContent>
          {GROUP_BY.map(([g, label]) => (
            <SelectItem key={g} value={g}>
              {label}
            </SelectItem>
          ))}
        </SelectContent>
      </Select>
      <ToggleGroup
        type="single"
        variant="outline"
        size="sm"
        aria-label="Chart measure"
        value={value.measure}
        onValueChange={(measure: Measure | '') => {
          if (measure) onChange({ measure })
        }}
      >
        <ToggleGroupItem value="usd">$</ToggleGroupItem>
        <ToggleGroupItem value="hours">Hours</ToggleGroupItem>
      </ToggleGroup>
      <div className="flex-1" />
      {actions}
    </div>
  )
}
