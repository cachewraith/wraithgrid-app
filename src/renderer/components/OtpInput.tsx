// The emailed code as one box per digit. Typing moves to the next box, Backspace steps back,
// arrows move, and a pasted or autofilled code fills every box at once. `onComplete` fires when
// the last empty box is filled, so the screen can submit without a button press.
import { useEffect, useRef, type ClipboardEvent, type KeyboardEvent } from 'react'
import { eraseDigit, fillDigits, typedInto, type Digits } from '../lib/auth-forms'

export function OtpInput({
  digits,
  onChange,
  onComplete,
  disabled,
  invalid,
  describedBy
}: {
  digits: Digits
  onChange: (digits: Digits) => void
  onComplete: (code: string) => void
  disabled?: boolean
  /** Draws the boxes as wrong (after a refused code). */
  invalid?: boolean
  describedBy?: string
}) {
  const boxes = useRef<(HTMLInputElement | null)[]>([])
  const focus = (i: number): void => {
    const el = boxes.current[i]
    if (el) {
      el.focus()
      el.select()
    }
  }
  useEffect(() => {
    // Start at the first empty box (the first one for a fresh code).
    const first = digits.findIndex((d) => !d)
    focus(first === -1 ? digits.length - 1 : first)
    // Only when shown: later focus follows typing.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [])

  const apply = (next: { digits: Digits; focus: number }): void => {
    const wasFull = digits.every(Boolean)
    onChange(next.digits)
    focus(next.focus)
    if (!wasFull && next.digits.every(Boolean)) onComplete(next.digits.join(''))
  }

  const onKey = (i: number, e: KeyboardEvent<HTMLInputElement>): void => {
    if (e.key === 'Backspace') {
      e.preventDefault()
      apply(eraseDigit(digits, i))
    } else if (e.key === 'ArrowLeft' && i > 0) {
      e.preventDefault()
      focus(i - 1)
    } else if (e.key === 'ArrowRight' && i < digits.length - 1) {
      e.preventDefault()
      focus(i + 1)
    }
  }

  const onPaste = (i: number, e: ClipboardEvent<HTMLInputElement>): void => {
    e.preventDefault()
    apply(fillDigits(digits, i, e.clipboardData.getData('text')))
  }

  return (
    <div
      className={`otp${invalid ? ' bad' : ''}`}
      role="group"
      aria-label={`${digits.length}-digit code`}
      aria-describedby={describedBy}
    >
      {digits.map((d, i) => (
        <input
          key={i}
          ref={(el) => {
            boxes.current[i] = el
          }}
          className={d ? 'on' : ''}
          value={d}
          onChange={(e) => apply(fillDigits(digits, i, typedInto(d, e.target.value)))}
          onKeyDown={(e) => onKey(i, e)}
          onPaste={(e) => onPaste(i, e)}
          onFocus={(e) => e.target.select()}
          inputMode="numeric"
          pattern="[0-9]*"
          // Lets the OS / browser offer the code from the email in the first box.
          autoComplete={i === 0 ? 'one-time-code' : 'off'}
          maxLength={digits.length}
          aria-label={`Digit ${i + 1} of ${digits.length}`}
          aria-invalid={invalid || undefined}
          disabled={disabled}
        />
      ))}
    </div>
  )
}
