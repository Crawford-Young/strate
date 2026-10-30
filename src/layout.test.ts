import { describe, expect, it } from 'vitest'
import {
  DEFAULT_PREFS,
  MIN_SIZE,
  PREFS_KEY,
  columnsLayout,
  loadPrefs,
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

const open: LayoutPrefs = { console: { open: true, size: 40 } }

describe('defaults', () => {
  it('are the console closed at 32%', () => {
    expect(DEFAULT_PREFS).toEqual({ console: { open: false, size: 32 } })
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

  it('defaults a console that is not a valid pref', () => {
    const stored = JSON.stringify({ console: { open: 'yes', size: 40 } })
    expect(loadPrefs(memory(stored))).toEqual(DEFAULT_PREFS)
  })

  it('defaults a console saved below its minimum or past its room', () => {
    const below = JSON.stringify({ console: { open: true, size: MIN_SIZE.console - 1 } })
    const past = JSON.stringify({ console: { open: true, size: 100 - MIN_SIZE.canvas + 1 } })
    expect(loadPrefs(memory(below))).toEqual(DEFAULT_PREFS)
    expect(loadPrefs(memory(past))).toEqual(DEFAULT_PREFS)
  })

  it('accepts a console filling all but the canvas minimum', () => {
    const fit = { console: { open: true, size: 100 - MIN_SIZE.canvas } }
    expect(loadPrefs(memory(JSON.stringify(fit)))).toEqual(fit)
  })

  it('defaults a console stored as anything but an object', () => {
    expect(loadPrefs(memory(JSON.stringify({ console: null })))).toEqual(DEFAULT_PREFS)
    expect(loadPrefs(memory(JSON.stringify({ console: 5 })))).toEqual(DEFAULT_PREFS)
  })

  it('defaults everything when the stored value is not an object', () => {
    expect(loadPrefs(memory('null'))).toEqual(DEFAULT_PREFS)
    expect(loadPrefs(memory('[1]'))).toEqual(DEFAULT_PREFS)
  })

  // Builds before #106 also saved the Harness and Terminal panels, which are gone.
  it('restores the console from a layout saved with the old Harness and Terminal panels', () => {
    const stored = JSON.stringify({
      harness: { open: true, size: 45 },
      console: { open: true, size: 40 },
      terminal: { open: true, size: 20 },
    })
    expect(loadPrefs(memory(stored))).toEqual({ console: { open: true, size: 40 } })
  })

  it('ignores old Harness and Terminal entries that are garbage', () => {
    const stored = JSON.stringify({
      harness: 'open',
      console: { open: false, size: 28 },
      terminal: { open: true, size: 140 },
    })
    expect(loadPrefs(memory(stored))).toEqual({ console: { open: false, size: 28 } })
  })

  it('defaults a layout that only holds the old Harness and Terminal panels', () => {
    const stored = JSON.stringify({
      harness: { open: true, size: 30 },
      terminal: { open: true, size: 35 },
    })
    expect(loadPrefs(memory(stored))).toEqual(DEFAULT_PREFS)
  })
})

describe('columnsLayout', () => {
  it('gives a closed console no space', () => {
    expect(columnsLayout(DEFAULT_PREFS)).toEqual({ canvas: 100, console: 0 })
  })

  it('gives an open console its saved size and the canvas the rest', () => {
    expect(columnsLayout(open)).toEqual({ canvas: 60, console: 40 })
  })
})

describe('withLayout', () => {
  it('records the size of an open console and its open state', () => {
    expect(withLayout(DEFAULT_PREFS, { canvas: 72, console: 28 })).toEqual({
      console: { open: true, size: 28 },
    })
  })

  it('keeps the last open size when the console collapses', () => {
    expect(withLayout(open, { canvas: 100, console: 0 })).toEqual({
      console: { open: false, size: 40 },
    })
  })

  it('returns the same prefs when nothing changed', () => {
    expect(withLayout(open, { canvas: 60, console: 40 })).toBe(open)
  })

  it('ignores a layout that does not name the console', () => {
    expect(withLayout(open, { canvas: 100 })).toBe(open)
  })
})
