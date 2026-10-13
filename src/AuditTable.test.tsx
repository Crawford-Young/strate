import { fireEvent, render, screen } from '@testing-library/react'
import { describe, expect, it } from 'vitest'
import { node } from '../tests/audit-fixtures'
import { AuditTable } from './AuditTable'

const web = node('repo', 'acme/web', {}, [node('issue', '1', {}, [], { label: '#1 Fix' })])
const api = node('repo', 'acme/api', {}, [node('issue', '2', {}, [], { label: '#2 Add' })])

const shown = () => screen.getAllByRole('button', { name: /^(acme|#)/ }).map((b) => b.textContent)

describe('AuditTable', () => {
  it('keeps a row expanded when a refresh inserts a sibling before it', () => {
    const { rerender } = render(<AuditTable nodes={[web]} selected={null} onSelect={() => {}} />)
    fireEvent.click(screen.getByRole('button', { name: 'Expand repo acme/web' }))
    expect(shown()).toEqual(['acme/web', '#1 Fix'])
    rerender(<AuditTable nodes={[api, web]} selected={null} onSelect={() => {}} />)
    expect(shown()).toEqual(['acme/api', 'acme/web', '#1 Fix'])
  })

  it('keeps rows apart whose keys would join into the same id', () => {
    // A repo `a` with issue `b`, and a repo whose slug reads like that pair joined.
    const x = node('repo', 'a', {}, [
      node('issue', 'b', {}, [node('session', 's', {}, [], { label: 'S1' })], { label: 'b' }),
    ])
    const y = node('repo', 'a/issue:b', {}, [node('session', 't', {}, [], { label: 'T1' })])
    render(<AuditTable nodes={[x, y]} selected={null} onSelect={() => {}} />)
    fireEvent.click(screen.getByRole('button', { name: 'Expand repo a/issue:b' }))
    fireEvent.click(screen.getByRole('button', { name: 'Expand repo a' }))
    expect(screen.getByRole('button', { name: 'Expand issue b' })).toBeInTheDocument()
    expect(screen.queryByRole('button', { name: 'S1' })).toBeNull()
  })

  it('disables picking a row while the rollup on screen is being replaced', () => {
    const picked: unknown[] = []
    render(<AuditTable nodes={[web]} selected={null} onSelect={(p) => picked.push(p)} disabled />)
    const name = screen.getByRole('button', { name: 'acme/web' })
    expect(name).toBeDisabled()
    fireEvent.click(name)
    expect(picked).toEqual([])
  })

  it('passes the path and label of the row picked', () => {
    const picked: unknown[] = []
    render(<AuditTable nodes={[web]} selected={null} onSelect={(...args) => picked.push(args)} />)
    fireEvent.click(screen.getByRole('button', { name: 'acme/web' }))
    expect(picked).toEqual([[[['repo', 'acme/web']], 'acme/web']])
  })
})
