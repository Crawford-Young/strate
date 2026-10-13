// The app root: the live view and the full-screen Audit view (#27). LiveShell stays mounted while
// Audit shows, so panel prefs and the picked tab survive a visit. Hiding it is safe for the saved
// sizes: react-resizable-panels skips a resize to a 0-px group instead of reporting a layout.
import { useRef, useState } from 'react'
import { flushSync } from 'react-dom'
import { AuditView } from './AuditView'
import { LiveShell } from './LiveShell'

type View = 'live' | 'audit'

export function App() {
  const [view, setView] = useState<View>('live')
  const auditButtonRef = useRef<HTMLButtonElement>(null)
  const backToLive = () => {
    // Render the live view first: a hidden button can't take focus.
    flushSync(() => setView('live'))
    ;(auditButtonRef.current as HTMLButtonElement).focus()
  }
  return (
    <>
      <LiveShell
        hidden={view !== 'live'}
        auditButtonRef={auditButtonRef}
        onOpenAudit={() => setView('audit')}
      />
      <AuditView hidden={view !== 'audit'} onBack={backToLive} />
    </>
  )
}
