// Checks for the Wraithgrid account screens (AuthGate, Settings), before anything reaches the
// server. The server checks again; these only give quick, specific messages.

/** Same rules as the server (wraithgrid-protocol `MIN_PASSWORD_LENGTH`, `OTP_LENGTH`). */
export const MIN_PASSWORD = 12
export const MAX_PASSWORD = 128
export const OTP_LENGTH = 6
/** Seconds before another code may be asked for (the server allows 3 a minute). */
export const RESEND_WAIT = 30

export function emailProblem(email: string): string | null {
  const e = email.trim()
  if (!e) return 'Enter your email.'
  return /^[^\s@]+@[^\s@]+\.[^\s@]+$/.test(e) ? null : 'That email address looks incomplete.'
}

/** For a new password: long enough, and the same twice when a confirmation is given. */
export function newPasswordProblem(password: string, confirm?: string): string | null {
  if (password.length < MIN_PASSWORD) return `Use at least ${MIN_PASSWORD} characters.`
  if (password.length > MAX_PASSWORD) return `Use at most ${MAX_PASSWORD} characters.`
  if (confirm !== undefined && confirm !== password) return "The passwords don't match."
  return null
}

export function nameProblem(name: string): string | null {
  const n = name.trim()
  if (!n) return 'Enter your name.'
  return n.length > 100 ? 'Use at most 100 characters.' : null
}

/** What may be typed or pasted into the code field: digits only, at most 6 ("123 456" → "123456"). */
export function cleanCode(input: string): string {
  return input.replace(/\D/g, '').slice(0, OTP_LENGTH)
}

export const codeComplete = (code: string): boolean => new RegExp(`^\\d{${OTP_LENGTH}}$`).test(code)

/** The resend button's text while it waits. */
export function resendLabel(secondsLeft: number): string {
  return secondsLeft > 0 ? `Resend code in ${secondsLeft}s` : 'Resend code'
}

/** Two letters for an avatar: "Mara Okafor" → "MO", "mara" → "M". */
export function initials(name: string): string {
  const parts = name.trim().split(/\s+/).filter(Boolean)
  const letters = parts.length > 1 ? [parts[0]!, parts[parts.length - 1]!] : parts.slice(0, 1)
  return letters.map((p) => [...p][0]!.toUpperCase()).join('') || '?'
}

/** The code as six boxes; '' = an empty box. */
export type Digits = string[]

export const emptyDigits = (): Digits => Array<string>(OTP_LENGTH).fill('')

/**
 * What a box's input now holds, minus the digit it already had: typing into a filled box
 * gives "34" when it held "3", and only the new "4" counts.
 */
export function typedInto(old: string, value: string): string {
  if (old && value.length === 2) {
    if (value[0] === old) return value.slice(1)
    if (value[1] === old) return value.slice(0, 1)
  }
  return value
}

/**
 * Puts typed or pasted text into the boxes, starting at box `at`; returns the boxes and the
 * box to focus next. A whole code (six digits, e.g. pasted or filled in from the email) always
 * fills every box from the first.
 */
export function fillDigits(
  digits: Digits,
  at: number,
  text: string
): { digits: Digits; focus: number } {
  const chars = text.replace(/\D/g, '')
  const next = [...digits]
  if (!chars) {
    next[at] = ''
    return { digits: next, focus: at }
  }
  if (chars.length >= OTP_LENGTH) {
    return { digits: [...chars.slice(0, OTP_LENGTH)], focus: OTP_LENGTH - 1 }
  }
  let i = at
  for (const ch of chars) {
    if (i >= OTP_LENGTH) break
    next[i++] = ch
  }
  return { digits: next, focus: Math.min(i, OTP_LENGTH - 1) }
}

/** Backspace: clears this box, or, when it's already empty, the one before (and goes there). */
export function eraseDigit(digits: Digits, at: number): { digits: Digits; focus: number } {
  const next = [...digits]
  if (next[at]) {
    next[at] = ''
    return { digits: next, focus: at }
  }
  const back = Math.max(0, at - 1)
  next[back] = ''
  return { digits: next, focus: back }
}
