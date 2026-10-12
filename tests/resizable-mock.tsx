// A stand-in for the library's resizable panels. happy-dom has no layout, so
// the real group never measures and never reports a layout. This one keeps a
// layout per group, applies `resize`/`collapse` to it and reports every change
// through `onLayoutChanged`, as the real group does after an imperative call.
import type { PanelImperativeHandle, ResizablePanelGroupProps } from '@crawfordyoung/ui'
import {
  createContext,
  useContext,
  useImperativeHandle,
  useLayoutEffect,
  type ReactNode,
  type Ref,
} from 'react'

type Layout = Record<string, number>

interface Group {
  defaultLayout: Layout
  layout: Layout
  report?: (layout: Layout) => void
}

const groups = new Map<string, Group>()
const GroupId = createContext('')

function change(groupId: string, panelId: string, size: number) {
  const group = groups.get(groupId) as Group
  group.layout = { ...group.layout, [panelId]: size }
  group.report?.(group.layout)
}

/** Drags a group to `layout`, as a user would. */
export function dragTo(groupId: string, layout: Layout) {
  const group = groups.get(groupId) as Group
  group.layout = layout
  group.report?.(layout)
}

/** The `defaultLayout` a group mounted with. */
export function defaultLayoutOf(groupId: string): Layout {
  return (groups.get(groupId) as Group).defaultLayout
}

export function resetGroups() {
  groups.clear()
}

export function ResizablePanelGroup({
  id,
  orientation,
  defaultLayout,
  onLayoutChanged,
  children,
}: ResizablePanelGroupProps) {
  const groupId = String(id)
  useLayoutEffect(() => {
    const group = groups.get(groupId) ?? {
      defaultLayout: { ...defaultLayout },
      layout: { ...defaultLayout },
    }
    group.report = (layout) => onLayoutChanged?.(layout, { isUserInteraction: false })
    groups.set(groupId, group)
    group.report(group.layout)
  })
  return (
    <div data-testid={`group-${groupId}`} data-orientation={orientation}>
      <GroupId.Provider value={groupId}>{children}</GroupId.Provider>
    </div>
  )
}

export function ResizablePanel({
  id,
  panelRef,
  collapsible,
  children,
}: {
  id?: string | number
  collapsible?: boolean
  panelRef?: Ref<PanelImperativeHandle | null>
  children?: ReactNode
}) {
  const groupId = useContext(GroupId)
  const panelId = String(id)
  useImperativeHandle(panelRef, () => ({
    // Like the real panel, collapse does nothing unless the panel is collapsible.
    collapse: () => {
      if (collapsible) change(groupId, panelId, 0)
    },
    expand: () => {
      throw new Error('strate opens panels with resize(), not expand()')
    },
    resize: (size) => change(groupId, panelId, Number.parseFloat(String(size))),
    isCollapsed: () => groups.get(groupId)?.layout[panelId] === 0,
    getSize: () => ({ asPercentage: groups.get(groupId)?.layout[panelId] ?? 0, inPixels: 0 }),
  }))
  return <section data-testid={`panel-${panelId}`}>{children}</section>
}

export function ResizableHandle() {
  return <div role="separator" />
}
