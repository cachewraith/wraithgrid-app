import { describe, expect, it } from 'vitest'
import {
  cleanCode,
  codeComplete,
  emailProblem,
  emptyDigits,
  eraseDigit,
  fillDigits,
  typedInto,
  initials,
  nameProblem,
  newPasswordProblem,
  resendLabel
} from '../../src/renderer/lib/auth-forms'

describe('account screen checks', () => {
  it('emails', () => {
    expect(emailProblem('')).toBe('Enter your email.')
    expect(emailProblem('mara@example')).toBe('That email address looks incomplete.')
    expect(emailProblem(' mara@example.com ')).toBeNull()
  })

  it('new passwords: length and confirmation', () => {
    expect(newPasswordProblem('short')).toBe('Use at least 12 characters.')
    expect(newPasswordProblem('x'.repeat(129))).toBe('Use at most 128 characters.')
    expect(newPasswordProblem('correct horse battery', 'correct horse')).toBe(
      "The passwords don't match."
    )
    expect(newPasswordProblem('correct horse battery', 'correct horse battery')).toBeNull()
    expect(newPasswordProblem('correct horse battery')).toBeNull()
  })

  it('names', () => {
    expect(nameProblem('  ')).toBe('Enter your name.')
    expect(nameProblem('x'.repeat(101))).toBe('Use at most 100 characters.')
    expect(nameProblem('Mara')).toBeNull()
  })

  it('codes: digits only, six of them, pasted with spaces or dashes', () => {
    expect(cleanCode('123 456')).toBe('123456')
    expect(cleanCode('12-34-56-78')).toBe('123456')
    expect(cleanCode('ab1')).toBe('1')
    expect(codeComplete('012345')).toBe(true)
    expect(codeComplete('12345')).toBe(false)
  })

  it('resend countdown and initials', () => {
    expect(resendLabel(12)).toBe('Resend code in 12s')
    expect(resendLabel(0)).toBe('Resend code')
    expect(initials('Mara Okafor')).toBe('MO')
    expect(initials('mara')).toBe('M')
    expect(initials('Ana María de la Cruz')).toBe('AC')
    expect(initials('  ')).toBe('?')
  })
})

describe('code boxes', () => {
  const blank = () => emptyDigits()

  it('typing fills a box and moves on; the last box stays', () => {
    let s = fillDigits(blank(), 0, '1')
    expect(s).toEqual({ digits: ['1', '', '', '', '', ''], focus: 1 })
    s = fillDigits(['1', '2', '3', '4', '5', ''], 5, '6')
    expect(s).toEqual({ digits: ['1', '2', '3', '4', '5', '6'], focus: 5 })
    expect(fillDigits(blank(), 2, 'x')).toEqual({ digits: blank(), focus: 2 })
  })

  it('typing over a filled box keeps only the new digit', () => {
    expect(typedInto('3', '34')).toBe('4')
    expect(typedInto('3', '43')).toBe('4')
    expect(typedInto('', '7')).toBe('7')
    expect(typedInto('3', '123456')).toBe('123456')
  })

  it('a pasted or autofilled code fills every box from the first', () => {
    expect(fillDigits(blank(), 3, '123 456')).toEqual({
      digits: ['1', '2', '3', '4', '5', '6'],
      focus: 5
    })
    expect(fillDigits(blank(), 0, 'Your code: 987654.')).toEqual({
      digits: ['9', '8', '7', '6', '5', '4'],
      focus: 5
    })
    // A partial paste goes in from where the cursor is.
    expect(fillDigits(blank(), 2, '12')).toEqual({ digits: ['', '', '1', '2', '', ''], focus: 4 })
  })

  it('backspace clears this box, or steps back when it is empty', () => {
    expect(eraseDigit(['1', '2', '', '', '', ''], 1)).toEqual({
      digits: ['1', '', '', '', '', ''],
      focus: 1
    })
    expect(eraseDigit(['1', '2', '', '', '', ''], 2)).toEqual({
      digits: ['1', '', '', '', '', ''],
      focus: 1
    })
    expect(eraseDigit(blank(), 0)).toEqual({ digits: blank(), focus: 0 })
  })
})
