import { describe, expect, it } from 'vitest'
import {
  DEFAULT_PREFS,
  MIN_SIZE,
  PREFS_KEY,
  columnsLayout,
  loadPrefs,
  rowsLayout,
  savePrefs,
  withLayout,
  type LayoutPrefs,
} from './layout'

function memory(initial?: string) {
  const data = new Map<string, string>()
  if (initial !== undefined) data.set(PREFS_KEY, initial)
  return {
    getItem: (key: string) => data.get(key) ?? null,
    setItem: (key: string, value: string) => void data.set(key, value),
    data,
  }
}

const open: LayoutPrefs = {
  harness: { open: true, size: 25 },
  console: { open: true, size: 40 },
  terminal: { open: true, size: 20 },
}

describe('defaults', () => {
  it('are Harness 30%, console 32%, terminal 35%, all closed', () => {
    expect(DEFAULT_PREFS).toEqual({
      harness: { open: false, size: 30 },
      console: { open: false, size: 32 },
      terminal: { open: false, size: 35 },
    })
  })
})

describe('loadPrefs', () => {
  it('falls back to the defaults with nothing stored', () => {
    expect(loadPrefs(memory())).toEqual(DEFAULT_PREFS)
  })

  it('keeps working when storage refuses to save', () => {
    const full = {
      setItem: () => {
        throw new DOMException('quota', 'QuotaExceededError')
      },
    }
    expect(() => savePrefs(full, open)).not.toThrow()
  })

  it('restores what savePrefs stored', () => {
    const storage = memory()
    savePrefs(storage, open)
    expect(loadPrefs(storage)).toEqual(open)
  })

  it('ignores stored JSON that does not parse', () => {
    expect(loadPrefs(memory('{not json'))).toEqual(DEFAULT_PREFS)
  })

  it('keeps each valid panel and defaults the rest', () => {
    const stored = JSON.stringify({
      harness: { open: true, size: 22 },
      console: { open: 'yes', size: 40 },
      terminal: { open: true, size: 140 },
    })
    expect(loadPrefs(memory(stored))).toEqual({
      ...DEFAULT_PREFS,
      harness: { open: true, size: 22 },
    })
  })

  it('defaults a panel saved below its minimum or past its room', () => {
    const stored = JSON.stringify({
      harness: { open: true, size: MIN_SIZE.harness - 1 },
      console: { open: true, size: 30 },
      terminal: { open: true, size: 100 - MIN_SIZE.main + 1 },
    })
    expect(loadPrefs(memory(stored))).toEqual({
      ...DEFAULT_PREFS,
      console: { open: true, size: 30 },
    })
  })

  it('defaults everything when Harness and console would squeeze the canvas', () => {
    const stored = JSON.stringify({
      harness: { open: false, size: 45 },
      console: { open: true, size: 100 - MIN_SIZE.canvas - 44 },
      terminal: { open: true, size: 20 },
    })
    expect(loadPrefs(memory(stored))).toEqual(DEFAULT_PREFS)
  })

  it('accepts Harness and console filling all but the canvas minimum', () => {
    const fit = {
      harness: { open: true, size: 40 },
      console: { open: true, size: 100 - MIN_SIZE.canvas - 40 },
      terminal: { open: false, size: 35 },
    }
    expect(loadPrefs(memory(JSON.stringify(fit)))).toEqual(fit)
  })

  it('defaults panels stored as anything but an object', () => {
    const stored = JSON.stringify({ harness: null, console: 5, terminal: 'open' })
    expect(loadPrefs(memory(stored))).toEqual(DEFAULT_PREFS)
  })

  it('defaults everything when the stored value is not an object', () => {
    expect(loadPrefs(memory('null'))).toEqual(DEFAULT_PREFS)
    expect(loadPrefs(memory('[1]'))).toEqual(DEFAULT_PREFS)
  })
})

describe('default layouts', () => {
  it('give closed panels no space', () => {
    expect(columnsLayout(DEFAULT_PREFS)).toEqual({ harness: 0, canvas: 100, console: 0 })
    expect(rowsLayout(DEFAULT_PREFS)).toEqual({ main: 100, terminal: 0 })
  })

  it('give open panels their saved size and the canvas the rest', () => {
    expect(columnsLayout(open)).toEqual({ harness: 25, canvas: 35, console: 40 })
    expect(rowsLayout(open)).toEqual({ main: 80, terminal: 20 })
  })
})

describe('withLayout', () => {
  it('records the size of open panels and the open state', () => {
    const next = withLayout(DEFAULT_PREFS, { harness: 28, canvas: 72, console: 0 })
    expect(next.harness).toEqual({ open: true, size: 28 })
    expect(next.console).toEqual({ open: false, size: 32 })
    expect(next.terminal).toBe(DEFAULT_PREFS.terminal)
  })

  it('keeps the last open size when a panel collapses', () => {
    const next = withLayout(open, { main: 100, terminal: 0 })
    expect(next.terminal).toEqual({ open: false, size: 20 })
  })

  it('returns the same prefs when nothing changed', () => {
    expect(withLayout(open, { harness: 25, canvas: 35, console: 40 })).toBe(open)
  })
})
