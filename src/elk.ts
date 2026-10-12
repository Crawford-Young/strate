// The app's one ELK, running in a web worker. `?url` makes Vite emit the
// worker script as a same-origin asset (the Tauri CSP allows 'self' only).
// elkjs only listens for the worker's messages, so a worker that fails to load
// or crashes would leave every layout pending: here its `error` rejects them,
// and the next call builds a fresh worker.
import ELK from 'elkjs/lib/elk-api'
import workerUrl from 'elkjs/lib/elk-worker.min.js?url'
import type { ElkLike } from './graphLayout'

let instance: ElkLike | undefined

function create(): ElkLike {
  let fail!: (reason: Error) => void
  const failed = new Promise<never>((_, reject) => (fail = reject))
  // Nothing may be waiting when the worker fails.
  failed.catch(() => {})
  const engine = new ELK({
    workerUrl,
    algorithms: ['layered'],
    workerFactory: (url) => {
      const worker = new Worker(url as string)
      worker.addEventListener('error', (e) => {
        instance = undefined
        worker.terminate()
        fail(new Error(e.message || 'The layout worker failed'))
      })
      return worker
    },
  })
  return { layout: (graph) => Promise.race([engine.layout(graph), failed]) }
}

export function elk(): ElkLike {
  return (instance ??= create())
}
