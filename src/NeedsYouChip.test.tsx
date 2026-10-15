import { act, fireEvent, render, screen, within } from '@testing-library/react'
import { afterEach, describe, expect, it, vi } from 'vitest'
import type { NeedsYou, Workstream } from './api'
import { NeedsYouChip } from './NeedsYouChip'

function ws(over: Partial<Workstream> & Pick<Workstream, 'id' | 'name'>): Workstream {
  return {
    fallback: false,
    costUsd: null,
    unpriced: 0,
    state: 'idle',
    needsYou: 0,
    sessionId: `s-${over.id}`,
    live: false,
    startedAt: null,
    director: null,
    ...over,
  }
}

function wait(over: Partial<NeedsYou>): NeedsYou {
  return {
    source: 'registry',
    sessionId: null,
    agentId: null,
    waitingFor: null,
    sinceMs: null,
    workstream: null,
    ...over,
  }
}

const NOW = Date.parse('2026-10-15T12:00:00Z')
const minutesAgo = (m: number) => NOW - m * 60_000

const director = ws({ id: 1, name: 'director-strate', live: true })
const ticket = ws({ id: 2, name: 'ticket-strate-68', live: true })
const unit = ws({ id: 3, name: 'ticket-strate-67', live: true, director: 1 })
const blocked = ws({ id: 4, name: 'dog-eat-dog-18' })
const sessions = [director, ticket, unit, blocked]

const tabbed = wait({
  sessionId: 's-2',
  waitingFor: 'permission: Bash',
  sinceMs: minutesAgo(4),
  workstream: 2,
})
const hook = wait({
  source: 'hook',
  sessionId: 's-3',
  agentId: 'a-9',
  waitingFor: 'permission: Edit',
  sinceMs: minutesAgo(2),
  workstream: 3,
})
const tabless = wait({ sessionId: '7f3a9c1e-0000', sinceMs: minutesAgo(66), workstream: 4 })

afterEach(() => {
  vi.useRealTimers()
  vi.restoreAllMocks()
})

function show(waits: NeedsYou[], onGoToTab = vi.fn()) {
  render(
    <NeedsYouChip
      waits={waits}
      sessions={sessions}
      tabs={[director, ticket]}
      onGoToTab={onGoToTab}
    />,
  )
  return onGoToTab
}

const chip = () => screen.getByRole('button', { name: /needs? you/ })
const open = () => fireEvent.click(chip())
const popover = () => screen.getByRole('dialog', { name: 'Waiting on you' })
const rows = () => within(popover()).getAllByRole('listitem')
const row = (i: number) => rows()[i] as HTMLElement

describe('the chip', () => {
  it('counts one wait in the singular', () => {
    show([tabbed])
    expect(chip()).toHaveTextContent('1 needs you')
  })

  it('counts several in the plural, closed until clicked', () => {
    show([tabbed, hook, tabless])
    expect(chip()).toHaveTextContent('3 need you')
    expect(chip()).toHaveAttribute('aria-expanded', 'false')
    expect(screen.queryByRole('dialog')).not.toBeInTheDocument()
    open()
    expect(chip()).toHaveAttribute('aria-expanded', 'true')
    expect(within(popover()).getByText('Waiting on you')).toBeInTheDocument()
  })
})

describe('the list', () => {
  it('lists every wait longest first, one with no start last', () => {
    vi.useFakeTimers({ toFake: ['Date', 'setTimeout', 'clearTimeout'] })
    vi.setSystemTime(NOW)
    const unknown = wait({ source: 'hook', sessionId: 's-2', agentId: 'a-1', workstream: 2 })
    show([unknown, hook, tabbed, tabless])
    open()
    expect(rows().map((r) => r.textContent)).toEqual([
      'dog-eat-dog-18blocked1h 6mcopy resume',
      'ticket-strate-68permission: Bash4mgo to tab',
      'ticket-strate-67permission: Edit2mgo to tab',
      'ticket-strate-68waitinggo to tab',
    ])
  })

  it('names a wait with no known workstream by its session, or as unknown with none', () => {
    show([wait({ sessionId: 'abcdef0123456789' }), wait({ source: 'hook' })])
    open()
    expect(within(row(0)).getByText('session abcdef01')).toBeInTheDocument()
    expect(within(row(1)).getByText('unknown session')).toBeInTheDocument()
  })

  it('offers no action on a wait with no session to resume', () => {
    show([wait({ source: 'hook' })])
    open()
    expect(within(row(0)).queryByRole('button')).not.toBeInTheDocument()
  })

  it('ticks each wait time on its own minute', async () => {
    vi.useFakeTimers({ toFake: ['Date', 'setTimeout', 'clearTimeout'] })
    vi.setSystemTime(NOW + 30_000)
    show([tabbed])
    open()
    expect(within(row(0)).getByText('4m')).toBeInTheDocument()
    await act(() => vi.advanceTimersByTimeAsync(29_999))
    expect(within(row(0)).getByText('4m')).toBeInTheDocument()
    await act(() => vi.advanceTimersByTimeAsync(1))
    expect(within(row(0)).getByText('5m')).toBeInTheDocument()
  })
})

describe('row actions', () => {
  it('goes to the tab of a tabbed wait and closes', () => {
    const onGoToTab = show([tabbed])
    open()
    fireEvent.click(screen.getByRole('button', { name: 'go to tab for ticket-strate-68' }))
    expect(onGoToTab).toHaveBeenCalledWith(2)
    expect(screen.queryByRole('dialog')).not.toBeInTheDocument()
  })

  it("goes to the director's tab for a unit nested under it", () => {
    const onGoToTab = show([hook])
    open()
    fireEvent.click(screen.getByRole('button', { name: 'go to tab for ticket-strate-67' }))
    expect(onGoToTab).toHaveBeenCalledWith(1)
  })

  it('copies the resume command for a wait with no tab and says so', async () => {
    const writeText = vi.spyOn(navigator.clipboard, 'writeText').mockResolvedValue()
    show([tabless])
    open()
    const copy = screen.getByRole('button', { name: 'copy resume for dog-eat-dog-18' })
    await act(async () => fireEvent.click(copy))
    expect(writeText).toHaveBeenCalledWith('claude --resume 7f3a9c1e-0000')
    expect(copy).toHaveTextContent('copied')
    expect(popover()).toBeInTheDocument()
  })

  it('says when the copy fails', async () => {
    vi.spyOn(navigator.clipboard, 'writeText').mockRejectedValue(new Error('denied'))
    show([tabless])
    open()
    const copy = screen.getByRole('button', { name: 'copy resume for dog-eat-dog-18' })
    await act(async () => fireEvent.click(copy))
    expect(copy).toHaveTextContent('copy failed')
  })

  it('copies for a unit whose director has no tab', async () => {
    const writeText = vi.spyOn(navigator.clipboard, 'writeText').mockResolvedValue()
    render(<NeedsYouChip waits={[hook]} sessions={sessions} tabs={[ticket]} onGoToTab={vi.fn()} />)
    open()
    await act(async () =>
      fireEvent.click(screen.getByRole('button', { name: 'copy resume for ticket-strate-67' })),
    )
    expect(writeText).toHaveBeenCalledWith('claude --resume s-3')
  })
})
