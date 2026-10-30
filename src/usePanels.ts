import { usePanelRef, type PanelImperativeHandle } from '@crawfordyoung/ui'
import { useEffect, useState, type RefObject } from 'react'
import {
  columnsLayout,
  loadPrefs,
  savePrefs,
  withLayout,
  type Layout,
  type PanelId,
} from './layout'

/**
 * The console panel: open state and size, saved to localStorage on every change and restored on
 * the next launch. It opens with `resize()` to its last size: `expand()` on a panel that mounted
 * collapsed only goes to its `minSize`.
 */
export function usePanels() {
  const [prefs, setPrefs] = useState(() => loadPrefs(localStorage))
  const [initial] = useState(prefs)
  const refs: Record<PanelId, RefObject<PanelImperativeHandle | null>> = {
    console: usePanelRef(),
  }
  useEffect(() => savePrefs(localStorage, prefs), [prefs])

  const panel = (id: PanelId) => refs[id].current as PanelImperativeHandle
  return {
    prefs,
    refs,
    columns: columnsLayout(initial),
    onLayout: (layout: Layout) => setPrefs((prev) => withLayout(prev, layout)),
    open: (id: PanelId) => {
      if (!prefs[id].open) panel(id).resize(`${prefs[id].size}%`)
    },
    close: (id: PanelId) => panel(id).collapse(),
  }
}
