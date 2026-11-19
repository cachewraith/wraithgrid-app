import { describe, expect, it } from 'vitest'
import {
  fenceCode,
  makeQuote,
  quoteSnippet,
  quotedCode,
  quoteBlock,
  splitQuotes,
  withQuotes
} from '../../src/renderer/lib/quotes'

describe('reply quotes (#164)', () => {
  it('makes a quote of a selection, or nothing of a blank one', () => {
    expect(makeQuote('\n\n  keep the indent\nsecond\n\n')).toEqual({
      text: '  keep the indent\nsecond',
      trimmed: false
    })
    expect(makeQuote('a\r\nb')).toEqual({ text: 'a\nb', trimmed: false })
    expect(makeQuote('  \n\t ')).toBeNull()
    expect(makeQuote('')).toBeNull()
  })

  it('cuts a long selection at the limit and says so', () => {
    const q = makeQuote('abcdefgh', 5)
    expect(q).toEqual({ text: 'abcde', trimmed: true })
    expect(quoteBlock(q!)).toBe('> abcde\n> […trimmed]')
  })

  it('quotes every line, an empty one with a bare >', () => {
    expect(
      quoteBlock({ text: 'Remember to turn it off\n\n(or keep the filter)', trimmed: false })
    ).toBe('> Remember to turn it off\n>\n> (or keep the filter)')
  })

  it('puts the quotes before the typed text', () => {
    const a = { text: 'first', trimmed: false }
    const b = { text: 'second\nline', trimmed: false }
    expect(withQuotes([a, b], 'why this?')).toBe('> first\n\n> second\n> line\n\nwhy this?')
    expect(withQuotes([a], '')).toBe('> first')
    expect(withQuotes([], 'just text')).toBe('just text')
  })

  it('keeps code fenced inside the quote', () => {
    const code = fenceCode('const a = 1\n', 'ts')
    expect(code).toBe('```ts\nconst a = 1\n```')
    expect(quoteBlock({ text: code, trimmed: false })).toBe('> ```ts\n> const a = 1\n> ```')
    // Backticks in the code get a longer fence.
    expect(fenceCode('a ``` b')).toBe('````\na ``` b\n````')
  })

  it('splits a sent message back into its quotes and text', () => {
    const quotes = [
      { text: 'first', trimmed: false },
      { text: '> nested\n\nwith a gap', trimmed: false }
    ]
    expect(splitQuotes(withQuotes(quotes, 'why?\n\n> later quote'))).toEqual({
      quotes: ['first', '> nested\n\nwith a gap'],
      rest: 'why?\n\n> later quote'
    })
    expect(splitQuotes('no quote > here')).toEqual({ quotes: [], rest: 'no quote > here' })
    expect(splitQuotes('> only')).toEqual({ quotes: ['only'], rest: '' })
  })
  it('finds a quote by its first line with words', () => {
    expect(quoteSnippet('```js\nconst   a = 1\n```')).toBe('const a = 1')
    expect(quoteSnippet('\n  Version: 0.5.0 → 0.6.0, a minor bump', 20)).toBe(
      'Version: 0.5.0 → 0.6'
    )
    expect(quoteSnippet('```\n```')).toBe('')
  })
  it('shows a quoted code block as code', () => {
    expect(quotedCode('```js\nconst a = 1\n```')).toEqual({ code: 'const a = 1', lang: 'js' })
    expect(quotedCode('````\na ``` b\n````')).toEqual({ code: 'a ``` b', lang: '' })
    expect(quotedCode('see\n```js\nx\n```')).toBeNull()
  })
})
