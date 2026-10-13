import { fireEvent, render, screen } from '@testing-library/react'
import { describe, expect, it } from 'vitest'
import { node } from '../tests/audit-fixtures'
import { AuditTable } from './AuditTable'

const web = node('repo', 'acme/web', {}, [node('issue', 'acme/web#1', {}, [], { label: '#1 Fix' })])
const api = node('repo', 'acme/api', {}, [node('issue', 'acme/api#2', {}, [], { label: '#2 Add' })])

const shown = () => screen.getAllByRole('button', { name: /^(acme|#)/ }).map((b) => b.textContent)

describe('AuditTable', () => {
  it('keeps a row expanded when a refresh inserts a sibling before it', () => {
    const { rerender } = render(<AuditTable nodes={[web]} selected={null} onSelect={() => {}} />)
    fireEvent.click(screen.getByRole('button', { name: 'Expand repo acme/web' }))
    expect(shown()).toEqual(['acme/web', '#1 Fix'])
    rerender(<AuditTable nodes={[api, web]} selected={null} onSelect={() => {}} />)
    expect(shown()).toEqual(['acme/api', 'acme/web', '#1 Fix'])
  })

  it('passes the path and label of the row picked', () => {
    const picked: unknown[] = []
    render(<AuditTable nodes={[web]} selected={null} onSelect={(...args) => picked.push(args)} />)
    fireEvent.click(screen.getByRole('button', { name: 'acme/web' }))
    expect(picked).toEqual([[[['repo', 'acme/web']], 'acme/web']])
  })
})
