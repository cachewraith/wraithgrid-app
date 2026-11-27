// Voice typing's pure parts (src/renderer/lib/voice.ts): where the spoken words land in the box.
import { describe, expect, it } from 'vitest'
import {
  clock,
  insertSpoken,
  megabytes,
  percent,
  typeAhead,
  TYPE_CATCH_UP_MS,
  TYPE_MIN_CPS
} from '../../src/renderer/lib/voice'

describe('insertSpoken', () => {
  it('fills an empty box', () => {
    expect(insertSpoken('', 0, 0, ' Fix the login bug. ')).toEqual({
      text: 'Fix the login bug.',
      caret: 18
    })
  })

  it('adds a space after typed words, none after one already there', () => {
    expect(insertSpoken('Please', 6, 6, 'fix it')).toEqual({ text: 'Please fix it', caret: 13 })
    expect(insertSpoken('Please ', 7, 7, 'fix it').text).toBe('Please fix it')
  })

  it('goes in at the caret, spaced from the words after it but not from punctuation', () => {
    expect(insertSpoken('Fix bug', 4, 4, 'the login')).toEqual({
      text: 'Fix the login bug',
      caret: 14
    })
    expect(insertSpoken('Fix it.', 6, 6, 'now').text).toBe('Fix it now.')
  })

  it('replaces a selection', () => {
    expect(insertSpoken('Fix the bug', 4, 7, 'that').text).toBe('Fix that bug')
  })

  it('leaves the box alone for silence and clamps a stale caret', () => {
    expect(insertSpoken('hello', 2, 2, '  ')).toEqual({ text: 'hello', caret: 2 })
    expect(insertSpoken('hi', 50, 60, 'there').text).toBe('hi there')
  })
})

describe('labels', () => {
  it('formats time, size and progress', () => {
    expect(clock(7_400)).toBe('0:07')
    expect(clock(90_000)).toBe('1:30')
    expect(megabytes(190_085_487)).toBe('190 MB')
    expect(percent(50, 200)).toBe(25)
    expect(percent(5, 0)).toBe(0)
    expect(percent(300, 200)).toBe(100)
  })
})

describe('typeAhead', () => {
  it('types at the steady pace when only a little is left', () => {
    expect(typeAhead(0, 2, 1000 / TYPE_MIN_CPS)).toBeCloseTo(1)
  })

  it('catches up faster the further behind it is, never past the end', () => {
    const behind = 100
    const step = typeAhead(0, behind, 16)
    expect(step).toBeCloseTo((behind * 16) / TYPE_CATCH_UP_MS)
    expect(step).toBeGreaterThan((TYPE_MIN_CPS * 16) / 1000)
    expect(typeAhead(99.9, 100, 1000)).toBe(100)
  })

  it('shows a shorter reading at once and stands still for a bad frame', () => {
    expect(typeAhead(40, 10, 16)).toBe(10)
    expect(typeAhead(5, 10, -50)).toBe(5)
  })
})
