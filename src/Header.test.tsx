import { Tabs } from '@crawfordyoung/ui'
import { render, screen } from '@testing-library/react'
import { createRef } from 'react'
import { describe, expect, it, vi } from 'vitest'
import type { NeedsYou, Workstream } from './api'
import { Header } from './Header'
import { DEFAULT_PREFS } from './layout'

const tabs: Workstream[] = [1, 2, 3, 4, 5].map((id) => ({
  id,
  name: `ticket-strate-${id}`,
  fallback: false,
  costUsd: null,
  unpriced: 0,
  state: 'needs_you',
  needsYou: 1,
  sessionId: `s-${id}`,
  live: true,
  startedAt: null,
  director: null,
}))

const waits: NeedsYou[] = [1, 2, 3].map((id) => ({
  source: 'hook',
  sessionId: `s-${id}`,
  agentId: null,
  waitingFor: 'AskUserQuestion',
  sinceMs: null,
  workstream: id,
}))

// happy-dom has no layout, so this can't show the strip scrolling or the chip staying on one line.
// It pins the flex classes that decide who gives way in the real header (#84): at 1440 px with 5
// tabs the chip wrapped to two lines. The CDP check covers the real layout.
describe('a full header', () => {
  it('has the classes that let the tab strip scroll and keep the chip on one line', () => {
    render(
      <Tabs value="1">
        <Header
          sessions={tabs}
          liveSessions={tabs}
          waits={waits}
          onGoToTab={vi.fn()}
          activeKey="1"
          prefs={DEFAULT_PREFS}
          onTogglePanel={vi.fn()}
          auditButtonRef={createRef()}
          onOpenAudit={vi.fn()}
        />
      </Tabs>,
    )
    const strip = screen.getByRole('tablist', { name: 'Orchestrators' })
    expect(strip).toHaveClass('min-w-0', 'overflow-x-auto')
    const chip = screen.getByRole('button', { name: /3 need you/ })
    expect(chip).toHaveClass('whitespace-nowrap')
    expect(chip.parentElement).toHaveClass('flex-none')
  })
})
