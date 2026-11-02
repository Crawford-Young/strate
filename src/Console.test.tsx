import { act, fireEvent, render, screen, within } from '@testing-library/react'
import type { ReactElement } from 'react'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { agentTranscript, type TranscriptEntry } from './api'
import { AgentTranscript } from './Console'

vi.mock('./api', () => ({ agentTranscript: vi.fn() }))

const transcriptMock = vi.mocked(agentTranscript)

/** A local wall-clock time as the ISO the store sends, so the expected clock reads the same anywhere. */
const at = (h: number, m: number, s: number) => new Date(2026, 9, 30, h, m, s).toISOString()

type ToolCall = Extract<TranscriptEntry, { kind: 'tool_call' }>

function call(over: Partial<ToolCall> & Pick<ToolCall, 'id'>): ToolCall {
  return {
    kind: 'tool_call',
    name: 'Bash',
    args: `Bash(echo ${over.id})`,
    status: 'success',
    result: null,
    resultTruncated: false,
    resultAt: null,
    todos: null,
    timestamp: null,
    ...over,
  }
}

function deferred<T>() {
  let resolve!: (value: T) => void
  const promise = new Promise<T>((r) => (resolve = r))
  return { promise, resolve }
}

beforeEach(() => {
  transcriptMock.mockReset().mockResolvedValue([])
})

const transcript = () => screen.getByRole('list', { name: 'Agent transcript' })
/** The transcript's own entries, not the todos nested in one. */
const items = () => [...transcript().children] as HTMLElement[]
const item = (i: number) => items()[i] as HTMLElement

async function show(entries: TranscriptEntry[], { agent = 7, working = false } = {}) {
  transcriptMock.mockResolvedValue(entries)
  const view = render(
    <AgentTranscript agent={agent} working={working} version={0} labelledBy="heading" />,
  )
  await screen.findByRole('list', { name: 'Agent transcript' })
  return view
}

describe('reading', () => {
  it("reads the agent's own transcript", async () => {
    await show([{ kind: 'text', text: 'lorem', timestamp: null }], { agent: 12 })
    expect(transcriptMock).toHaveBeenCalledWith(12)
  })

  it('says it is reading until the transcript lands', async () => {
    const pending = deferred<TranscriptEntry[]>()
    transcriptMock.mockReturnValue(pending.promise)
    render(<AgentTranscript agent={7} working={false} version={0} labelledBy="heading" />)
    expect(screen.getByText('Reading the transcript…')).toBeInTheDocument()
    await act(async () => pending.resolve([{ kind: 'text', text: 'lorem', timestamp: null }]))
    expect(screen.queryByText('Reading the transcript…')).not.toBeInTheDocument()
    expect(transcript()).toHaveTextContent('lorem')
  })

  it('says so when the agent has no records yet', async () => {
    render(<AgentTranscript agent={7} working={false} version={0} labelledBy="heading" />)
    expect(await screen.findByText('No transcript records yet.')).toBeInTheDocument()
  })

  it('scrolls in one keyboard-reachable container named once, by the list', async () => {
    await show([{ kind: 'text', text: 'lorem', timestamp: null }])
    expect(screen.queryByRole('region', { name: 'Transcript' })).not.toBeInTheDocument()
    const list = transcript()
    expect(list).toHaveAccessibleName('Agent transcript')
    const box = list.parentElement as HTMLElement
    expect(box).toHaveAttribute('tabIndex', '0')
    expect(box).not.toHaveAttribute('aria-label')
  })

  it('reports a read error', async () => {
    transcriptMock.mockRejectedValue('database is locked')
    render(<AgentTranscript agent={7} working={false} version={0} labelledBy="heading" />)
    expect(
      await screen.findByText("Couldn't read the transcript: database is locked"),
    ).toBeInTheDocument()
  })

  it('rereads on a store change and shows what was added', async () => {
    const { rerender } = await show([{ kind: 'text', text: 'lorem', timestamp: null }])
    transcriptMock.mockResolvedValue([
      { kind: 'text', text: 'lorem', timestamp: null },
      call({ id: 'toolu_2', status: 'pending' }),
    ])
    rerender(<AgentTranscript agent={7} working={false} version={1} labelledBy="heading" />)
    await screen.findByText('Bash(echo toolu_2)')
    expect(transcriptMock).toHaveBeenCalledTimes(2)
    expect(items()).toHaveLength(2)
  })
})

describe('entries', () => {
  it('shows every entry in transcript order, each at its time', async () => {
    await show([
      { kind: 'user', text: 'Fix the build', timestamp: at(14, 2, 11) },
      { kind: 'thinking', text: 'consider it', timestamp: at(14, 2, 12) },
      { kind: 'text', text: 'On it.', timestamp: at(14, 2, 13) },
      call({ id: 'toolu_1', args: 'Read(src/main.ts)', timestamp: at(14, 2, 14) }),
      call({ id: 'toolu_2', args: 'Bash(bun test)', timestamp: at(9, 5, 7) }),
    ])
    expect(items()).toHaveLength(5)
    expect(item(0)).toHaveTextContent('Fix the build')
    expect(item(1)).toHaveTextContent('Thinking…')
    expect(item(2)).toHaveTextContent('On it.')
    expect(item(3)).toHaveTextContent('Read(src/main.ts)')
    expect(item(4)).toHaveTextContent('Bash(bun test)')
    const times = items().map((li) => li.querySelector('time'))
    expect(times.map((t) => t?.textContent)).toEqual([
      '14:02:11',
      '14:02:12',
      '14:02:13',
      '14:02:14',
      '09:05:07',
    ])
    expect(times[0]).toHaveAttribute('dateTime', at(14, 2, 11))
  })

  it('offers the full time on hover', async () => {
    await show([{ kind: 'text', text: 'lorem', timestamp: '2026-10-30T14:02:11.250Z' }])
    expect(item(0).querySelector('time')).toHaveAttribute('title', '2026-10-30T14:02:11.250Z')
  })

  it('shows a time it cannot read as it came', async () => {
    await show([{ kind: 'text', text: 'lorem', timestamp: 'garbled' }])
    expect(item(0).querySelector('time')).toHaveTextContent(/^garbled$/)
  })

  it('shows an entry with no time without one', async () => {
    await show([{ kind: 'text', text: 'lorem', timestamp: null }])
    expect(item(0).querySelector('time')).toBeNull()
  })

  it('marks a human prompt as the user', async () => {
    await show([{ kind: 'user', text: 'Fix the build', timestamp: null }])
    expect(item(0)).toHaveTextContent('User: Fix the build')
  })

  it('keeps the line breaks of prompts and replies', async () => {
    await show([
      { kind: 'user', text: 'one\ntwo', timestamp: null },
      { kind: 'text', text: 'three\nfour', timestamp: null },
    ])
    expect(item(0)).toHaveClass('whitespace-pre-wrap')
    expect(item(1)).toHaveClass('whitespace-pre-wrap')
  })

  it('folds thinking away until opened', async () => {
    await show([{ kind: 'thinking', text: 'consider it', timestamp: null }])
    expect(screen.queryByText('consider it')).not.toBeInTheDocument()
    fireEvent.click(within(item(0)).getByRole('button', { name: /Thinking…/ }))
    expect(screen.getByText('consider it')).toBeInTheDocument()
  })
})

describe('tool calls', () => {
  it('shows each call with its status: done, failed, or running without a result while the agent works', async () => {
    await show(
      [
        call({ id: 'toolu_1', status: 'success', result: 'ok' }),
        call({ id: 'toolu_2', status: 'error', result: 'exit 1' }),
        call({ id: 'toolu_3', status: 'pending' }),
      ],
      { working: true },
    )
    expect(item(0)).toHaveAttribute('data-status', 'success')
    expect(item(0)).toHaveTextContent('Done: Bash(echo toolu_1)')
    expect(item(1)).toHaveAttribute('data-status', 'error')
    expect(item(1)).toHaveTextContent('Failed: Bash(echo toolu_2)')
    expect(item(2)).toHaveAttribute('data-status', 'running')
    expect(item(2)).toHaveTextContent(/^Running: Bash\(echo toolu_3\)$/)
  })

  it('cuts a long result line on whole characters, never inside a surrogate pair', async () => {
    const long = 'x'.repeat(159) + '😀' + 'y'.repeat(50)
    await show([call({ id: 'toolu_1', result: long })])
    const open = within(item(0)).getByRole('button', { name: /x+/ })
    expect(open).toHaveTextContent('x'.repeat(159) + '😀…')
    expect(open.textContent).not.toMatch(/[\uD800-\uDBFF](?![\uDC00-\uDFFF])/)
  })

  it("shows a call with no result as pending, not running, once the agent isn't working", async () => {
    await show([call({ id: 'toolu_1', status: 'pending' })], { working: false })
    expect(item(0)).toHaveAttribute('data-status', 'pending')
    expect(item(0)).toHaveTextContent(/^Pending: Bash\(echo toolu_1\)$/)
  })

  it("caps a long result's line under its call, opening onto the whole of it", async () => {
    const long = 'x'.repeat(500)
    await show([call({ id: 'toolu_1', result: long })])
    const open = within(item(0)).getByRole('button', { name: /x+…/ })
    expect(open).toHaveTextContent(`${'x'.repeat(160)}…`)
    expect(open).not.toHaveTextContent('x'.repeat(161))
    fireEvent.click(open)
    expect(item(0)).toHaveTextContent(long)
  })

  it('shows a one-line result under its call, with nothing to open', async () => {
    await show([call({ id: 'toolu_1', result: 'Found 3 files' })])
    expect(item(0)).toHaveTextContent('Found 3 files')
    expect(within(item(0)).queryByRole('button')).not.toBeInTheDocument()
  })

  it("shows a longer result's first line, opening onto the whole result", async () => {
    await show([call({ id: 'toolu_1', result: '\nline one\nline two\nline three' })])
    const open = within(item(0)).getByRole('button', { name: /line one/ })
    expect(item(0)).not.toHaveTextContent('line two')
    fireEvent.click(open)
    expect(item(0)).toHaveTextContent(/line one\s*line two\s*line three/)
  })

  it('says when a result was cut short', async () => {
    await show([call({ id: 'toolu_1', result: 'the start', resultTruncated: true })])
    fireEvent.click(within(item(0)).getByRole('button', { name: /the start/ }))
    expect(item(0)).toHaveTextContent('… (truncated)')
  })

  it('says when a finished call printed nothing', async () => {
    await show([call({ id: 'toolu_1', result: '  \n' })])
    expect(item(0)).toHaveTextContent('(no output)')
  })

  it("shows a TodoWrite's todos, each with its status, under its call", async () => {
    await show([
      call({
        id: 'toolu_1',
        name: 'TodoWrite',
        args: 'TodoWrite(3 todos)',
        timestamp: at(10, 0, 0),
        todos: [
          { content: 'Write the test', status: 'completed' },
          { content: 'Make it pass', status: 'in_progress' },
          { content: 'Refactor', status: 'pending' },
        ],
      }),
    ])
    expect(items()).toHaveLength(1)
    expect(item(0)).toHaveTextContent('TodoWrite(3 todos)')
    expect(item(0).querySelector('time')).toHaveTextContent('10:00:00')
    const todos = within(item(0)).getAllByRole('listitem')
    expect(todos.map((t) => t.dataset.status)).toEqual(['completed', 'in_progress', 'pending'])
    expect(todos[0]).toHaveTextContent('Completed: Write the test')
    expect(todos[1]).toHaveTextContent('In progress: Make it pass')
    expect(todos[2]).toHaveTextContent('Pending: Refactor')
  })

  it('shows a failed TodoWrite as a failed call', async () => {
    await show([
      call({
        id: 'toolu_1',
        name: 'TodoWrite',
        args: 'TodoWrite(1 todo)',
        status: 'error',
        result: 'invalid input',
        todos: [{ content: 'Write the test', status: 'pending' }],
      }),
    ])
    expect(item(0)).toHaveAttribute('data-status', 'error')
    expect(item(0)).toHaveTextContent('invalid input')
  })
})

describe('following the tail', () => {
  // happy-dom has no layout: every scroll metric reads 0. These stand in for a box `height`
  // tall holding `content` of transcript, scrolled to wherever it was last set.
  const metrics = { content: 0, height: 200 }
  const scrolled = new WeakMap<Element, number>()
  const KEYS = ['scrollHeight', 'clientHeight', 'scrollTop'] as const
  const proto = HTMLElement.prototype
  const original = KEYS.map((key) => [key, Object.getOwnPropertyDescriptor(proto, key)] as const)

  beforeEach(() => {
    metrics.content = 1000
    Object.defineProperty(HTMLElement.prototype, 'scrollHeight', {
      configurable: true,
      get: () => metrics.content,
    })
    Object.defineProperty(HTMLElement.prototype, 'clientHeight', {
      configurable: true,
      get: () => metrics.height,
    })
    Object.defineProperty(HTMLElement.prototype, 'scrollTop', {
      configurable: true,
      get(this: Element) {
        return scrolled.get(this) ?? 0
      },
      set(this: Element, top: number) {
        scrolled.set(this, top)
      },
    })
  })

  afterEach(() => {
    for (const [key, descriptor] of original) {
      if (descriptor) Object.defineProperty(proto, key, descriptor)
      else Reflect.deleteProperty(proto, key)
    }
  })

  const box = () => transcript().parentElement as HTMLElement
  const entry = (n: number): TranscriptEntry => ({
    kind: 'text',
    text: `entry ${n}`,
    timestamp: null,
  })

  async function grow(rerender: (ui: ReactElement) => void, version: number) {
    metrics.content += 300
    transcriptMock.mockResolvedValue(Array.from({ length: version + 1 }, (_, i) => entry(i)))
    rerender(<AgentTranscript agent={7} working={false} version={version} labelledBy="heading" />)
    await screen.findByText(`entry ${version}`)
  }

  it('opens at the newest entry and stays there as entries arrive', async () => {
    const { rerender } = await show([entry(0)])
    expect(box().scrollTop).toBe(1000)
    await grow(rerender, 1)
    expect(box().scrollTop).toBe(1300)
  })

  it('stays put while the reader is scrolled up, and follows again back at the bottom', async () => {
    const { rerender } = await show([entry(0)])
    box().scrollTop = 100
    fireEvent.scroll(box())
    await grow(rerender, 1)
    expect(box().scrollTop).toBe(100)
    // Back at the bottom, within a few pixels.
    box().scrollTop = metrics.content - metrics.height - 4
    fireEvent.scroll(box())
    await grow(rerender, 2)
    expect(box().scrollTop).toBe(metrics.content)
  })
})
