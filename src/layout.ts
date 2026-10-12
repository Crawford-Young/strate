// Panel sizes and open state, saved locally and restored on open. Sizes are
// percentages of the panel's group; a closed panel keeps its last open size.

export type PanelId = 'harness' | 'console' | 'terminal'

export interface PanelPref {
  open: boolean
  size: number
}

export type LayoutPrefs = Record<PanelId, PanelPref>

/** Panel id to percentage, as the resizable group reports it. */
export type Layout = Record<string, number>

export const PREFS_KEY = 'strate.layout'

const PANELS: readonly PanelId[] = ['harness', 'console', 'terminal']

export const DEFAULT_PREFS: LayoutPrefs = {
  harness: { open: false, size: 30 },
  console: { open: false, size: 32 },
  terminal: { open: false, size: 35 },
}

/** Smallest size per panel, in percent; the panels and the tests share these. */
export const MIN_SIZE = { harness: 15, canvas: 20, console: 15, main: 20, terminal: 10 } as const

/** The most a panel can take while its neighbour keeps its minimum. */
const MAX_SIZE: Record<PanelId, number> = {
  harness: 100 - MIN_SIZE.canvas,
  console: 100 - MIN_SIZE.canvas,
  terminal: 100 - MIN_SIZE.main,
}

function isPref(id: PanelId, value: unknown): value is PanelPref {
  if (typeof value !== 'object' || value === null) return false
  const { open, size } = value as Record<string, unknown>
  return (
    typeof open === 'boolean' &&
    typeof size === 'number' &&
    size >= MIN_SIZE[id] &&
    size <= MAX_SIZE[id]
  )
}

export function loadPrefs(storage: Pick<Storage, 'getItem'>): LayoutPrefs {
  let stored: unknown
  try {
    stored = JSON.parse(storage.getItem(PREFS_KEY) ?? 'null')
  } catch {
    return DEFAULT_PREFS
  }
  if (typeof stored !== 'object' || stored === null || Array.isArray(stored)) return DEFAULT_PREFS
  const record = stored as Record<string, unknown>
  const prefs = { ...DEFAULT_PREFS }
  for (const id of PANELS) {
    const pref = record[id]
    if (isPref(id, pref)) prefs[id] = { open: pref.open, size: pref.size }
  }
  // Harness and console share a row with the canvas: both at once must leave it room.
  const fits = prefs.harness.size + prefs.console.size <= MAX_SIZE.harness
  return fits ? prefs : DEFAULT_PREFS
}

export function savePrefs(storage: Pick<Storage, 'setItem'>, prefs: LayoutPrefs): void {
  try {
    storage.setItem(PREFS_KEY, JSON.stringify(prefs))
  } catch {
    // Storage full or blocked: the layout still works, it just isn't remembered.
  }
}

const shown = (pref: PanelPref) => (pref.open ? pref.size : 0)

/** Harness | canvas | console. */
export function columnsLayout(prefs: LayoutPrefs): Layout {
  const harness = shown(prefs.harness)
  const right = shown(prefs.console)
  return { harness, canvas: 100 - harness - right, console: right }
}

/** Main area over the terminal. */
export function rowsLayout(prefs: LayoutPrefs): Layout {
  const terminal = shown(prefs.terminal)
  return { main: 100 - terminal, terminal }
}

/** Prefs updated from a group's reported layout; the same object when unchanged. */
export function withLayout(prefs: LayoutPrefs, layout: Layout): LayoutPrefs {
  let next = prefs
  for (const id of PANELS) {
    const size = layout[id]
    if (size === undefined) continue
    const pref = size > 0 ? { open: true, size } : { open: false, size: prefs[id].size }
    if (pref.open !== prefs[id].open || pref.size !== prefs[id].size) {
      next = { ...next, [id]: pref }
    }
  }
  return next
}
