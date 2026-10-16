// The Audit view's only bar: `title` at its left start, then date range, the repo chips and
// the $ / hours toggle (the chart and breakdown columns), with `actions` at its right end
// (Refresh GitHub, Export CSV and "‹ Live", #27 U2). Its box matches the live header's, so
// "‹ Live" lands where Audit was.
import { DateRangePicker, Toggle, ToggleGroup, ToggleGroupItem } from '@crawfordyoung/ui'
import type { ReactNode } from 'react'
import { localDay, parseDay, UNLINKED, type Measure } from './audit'

/** The range presets, each the last that many local days. */
export const PRESET_DAYS = { '7d': 7, '14d': 14, '30d': 30 } as const
type Preset = keyof typeof PRESET_DAYS
const PRESETS = Object.keys(PRESET_DAYS) as Preset[]
export type Range = Preset | 'custom'

export interface Controls {
  range: Range
  /** The custom range's local days, used while `range` is custom. */
  custom: { from: string; to: string }
  /** The repos picked, `UNLINKED` for the windows in none; empty for every repo. */
  repos: string[]
  measure: Measure
}

interface AuditControlsProps {
  value: Controls
  /** The current range's local days, which a switch to custom starts from. */
  days: { from: string; to: string }
  onChange: (patch: Partial<Controls>) => void
  /** The repo chips, in order (see `repoChips`). */
  repos: string[]
  /** The view's heading, first in the bar. */
  title?: ReactNode
  actions?: ReactNode
}

export function AuditControls({
  value,
  days,
  onChange,
  repos,
  title,
  actions,
}: AuditControlsProps) {
  return (
    <div className="flex min-h-12 flex-none flex-wrap items-center gap-3 border-b border-border bg-surface px-3 py-2">
      {title}
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
      <div role="group" aria-label="Repos" className="flex flex-wrap gap-1.5">
        <Chip
          label="All repos"
          pressed={value.repos.length === 0}
          // Pressed again it stays: every repo is already in.
          onPressedChange={(on) => on && onChange({ repos: [] })}
        />
        {repos.map((r) => (
          <Chip
            key={r}
            label={r === UNLINKED ? 'Unlinked' : r}
            pressed={value.repos.includes(r)}
            onPressedChange={(on) => {
              const picked = new Set(value.repos)
              if (on) picked.add(r)
              else picked.delete(r)
              onChange({ repos: repos.filter((x) => picked.has(x)) })
            }}
          />
        ))}
      </div>
      <ToggleGroup
        type="single"
        variant="outline"
        size="sm"
        aria-label="Measure"
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

/**
 * A repo filter chip, the shape of the library's FilterChip, which is not exported yet
 * (component-library#139): a pill that is pressed while its repo is in the filter.
 */
function Chip({
  label,
  pressed,
  onPressedChange,
}: {
  label: string
  pressed: boolean
  onPressedChange: (pressed: boolean) => void
}) {
  return (
    <Toggle
      variant="outline"
      size="sm"
      pressed={pressed}
      onPressedChange={onPressedChange}
      className="h-7 rounded-full px-3 text-xs"
    >
      {label}
    </Toggle>
  )
}
