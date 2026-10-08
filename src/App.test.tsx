import { render, screen } from '@testing-library/react'
import { describe, expect, it } from 'vitest'
import { App } from './App'

describe('App', () => {
  it('renders the empty shell', () => {
    render(<App />)
    expect(screen.getByRole('heading', { name: 'Strate' })).toBeInTheDocument()
    expect(screen.getByText('No workstreams yet.')).toBeInTheDocument()
  })
})
