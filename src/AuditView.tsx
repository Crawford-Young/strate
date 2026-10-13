// The full-screen Audit view (#27): its own header with "‹ Live", then the audit body. It stays
// mounted while hidden, like LiveShell, and takes focus each time it is shown.
import { Button, EmptyState } from '@crawfordyoung/ui'
import { ChevronLeft } from 'lucide-react'
import { useEffect, useRef } from 'react'

interface AuditViewProps {
  hidden: boolean
  onBack: () => void
}

export function AuditView({ hidden, onBack }: AuditViewProps) {
  const headingRef = useRef<HTMLHeadingElement>(null)
  useEffect(() => {
    if (!hidden) (headingRef.current as HTMLHeadingElement).focus()
  }, [hidden])

  return (
    <div hidden={hidden} className="flex h-screen flex-col bg-background text-foreground">
      <header className="flex h-12 flex-none items-center gap-3 border-b border-border bg-surface px-3">
        <Button variant="ghost" size="sm" onClick={onBack} className="gap-1">
          <ChevronLeft aria-hidden className="size-4" />
          Live
        </Button>
        <h1
          ref={headingRef}
          tabIndex={-1}
          className="text-sm font-bold tracking-tight outline-none"
        >
          Audit
        </h1>
      </header>
      <main className="min-h-0 flex-1 overflow-auto">
        <EmptyState message="Audit arrives with the rest of #27." />
      </main>
    </div>
  )
}
