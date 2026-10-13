// The app root. Today it is the live view alone; the Audit view (#27) becomes a sibling of
// `LiveShell` here, which stays mounted so panel prefs and the picked tab survive a visit.
import { LiveShell } from './LiveShell'

export function App() {
  return <LiveShell />
}
