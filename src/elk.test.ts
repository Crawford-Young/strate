import { afterEach, beforeEach, expect, it, vi } from 'vitest'

class FakeWorker {
  static all: FakeWorker[] = []
  onmessage: unknown = null
  listeners: ((e: { message: string }) => void)[] = []
  terminated = false
  constructor(readonly url: string) {
    FakeWorker.all.push(this)
  }
  postMessage() {}
  addEventListener(type: string, listener: (e: { message: string }) => void) {
    if (type === 'error') this.listeners.push(listener)
  }
  terminate() {
    this.terminated = true
  }
  fail(message: string) {
    this.listeners.forEach((l) => l({ message }))
  }
}

beforeEach(() => {
  FakeWorker.all = []
  vi.resetModules()
  vi.stubGlobal('Worker', FakeWorker)
})

afterEach(() => {
  vi.unstubAllGlobals()
})

it('runs one ELK, in a web worker loaded from the bundled elk-worker script', async () => {
  const { elk } = await import('./elk')
  const instance = elk()
  expect(elk()).toBe(instance)
  expect(FakeWorker.all).toHaveLength(1)
  expect(FakeWorker.all[0]?.url).toMatch(/elk-worker\.min\.js$/)
})

it('rejects pending layouts when the worker fails, and builds a new one next time', async () => {
  const { elk } = await import('./elk')
  const first = elk()
  const pending = first.layout({ id: 'root' })
  FakeWorker.all[0]?.fail('Failed to load worker script')
  await expect(pending).rejects.toThrow('Failed to load worker script')
  expect(FakeWorker.all[0]?.terminated).toBe(true)
  await expect(first.layout({ id: 'root' })).rejects.toThrow('Failed to load worker script')
  expect(elk()).not.toBe(first)
  expect(FakeWorker.all).toHaveLength(2)
})

it('names a worker failure that carries no message', async () => {
  const { elk } = await import('./elk')
  const pending = elk().layout({ id: 'root' })
  FakeWorker.all[0]?.fail('')
  await expect(pending).rejects.toThrow('The layout worker failed')
})
