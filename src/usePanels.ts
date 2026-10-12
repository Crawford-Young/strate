import { usePanelRef, type PanelImperativeHandle } from '@crawfordyoung/ui'
import { useEffect, useState, type RefObject } from 'react'
import {
  columnsLayout,
  loadPrefs,
  rowsLayout,
  savePrefs,
  withLayout,
  type Layout,
  type PanelId,
} from './layout'

/**
 * Harness, console and terminal panels: open state and sizes, saved to
 * localStorage on every change and restored on the next launch. Panels open
 * with `resize()` to their last size: `expand()` on a panel that mounted
 * collapsed only goes to its `minSize`.
 */
export function usePanels() {
  const [prefs, setPrefs] = useState(() => loadPrefs(localStorage))
  const [initial] = useState(prefs)
  const refs: Record<PanelId, RefObject<PanelImperativeHandle | null>> = {
    harness: usePanelRef(),
    console: usePanelRef(),
    terminal: usePanelRef(),
  }
  useEffect(() => savePrefs(localStorage, prefs), [prefs])

  const panel = (id: PanelId) => refs[id].current as PanelImperativeHandle
  const open = (id: PanelId) => {
    if (!prefs[id].open) panel(id).resize(`${prefs[id].size}%`)
  }
  const close = (id: PanelId) => panel(id).collapse()
  return {
    prefs,
    refs,
    columns: columnsLayout(initial),
    rows: rowsLayout(initial),
    onLayout: (layout: Layout) => setPrefs((prev) => withLayout(prev, layout)),
    open,
    close,
    toggle: (id: PanelId) => (prefs[id].open ? close(id) : open(id)),
  }
}
