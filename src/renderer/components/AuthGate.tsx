// The Wraithgrid account gate: until this computer is signed in, the whole window is a sign-in
// screen. One task per screen: sign in; create an account → confirm the email (6 digit boxes,
// submits by itself); forgot password → email → code → new password. Agents keep running
// underneath; the app returns as it was once signed in. Google/GitHub open the browser (OAuth
// needs it); everything else happens here.
import { useEffect, useState, type FormEvent, type ReactNode } from 'react'
import type { CloudResult, CloudSignInResult } from '@shared/ipc-contract'
import { useCloud, type CloudApi } from '../app/cloud'
import {
  codeComplete,
  emailProblem,
  emptyDigits,
  MIN_PASSWORD,
  nameProblem,
  newPasswordProblem,
  OTP_LENGTH,
  RESEND_WAIT,
  resendLabel,
  type Digits
} from '../lib/auth-forms'
import { Logo } from './icons'
import { OtpInput } from './OtpInput'
import { WindowControls } from './TitleBar'

type Screen = 'signIn' | 'signUp' | 'verify' | 'forgot' | 'resetCode' | 'resetPassword'

/** Server answers that mean the code was wrong or gone: the boxes clear for another try. */
const CODE_ERRORS = new Set(['invalid_code', 'code_expired', 'too_many_attempts'])

/** Where "Back" goes from each screen. */
const BACK: Partial<Record<Screen, Screen>> = {
  signUp: 'signIn',
  verify: 'signIn',
  forgot: 'signIn',
  resetCode: 'forgot',
  resetPassword: 'resetCode'
}

/** The forgot-password steps, for "Step N of 3". */
const RESET_STEP: Partial<Record<Screen, number>> = { forgot: 1, resetCode: 2, resetPassword: 3 }

export function AuthGate() {
  const account = useCloud()
  if (!account) return null
  const { cloud, status } = account
  if (status?.signedIn) return null
  return (
    <div className="auth" role="dialog" aria-modal="true" aria-labelledby="auth-title">
      <div className="auth-top">
        <WindowControls />
      </div>
      <div className="auth-body">
        {status ? <AuthCard cloud={cloud} pending={status.pending} /> : null}
      </div>
    </div>
  )
}

function AuthCard({ cloud, pending }: { cloud: CloudApi; pending: boolean }) {
  const [screen, setScreen] = useState<Screen>('signIn')
  const [name, setName] = useState('')
  const [email, setEmail] = useState('')
  const [password, setPassword] = useState('')
  const [confirm, setConfirm] = useState('')
  const [showPassword, setShowPassword] = useState(false)
  const [digits, setDigits] = useState<Digits>(emptyDigits)
  // Bumped to show fresh, focused boxes (new screen, new code, refused code).
  const [codeKey, setCodeKey] = useState(0)
  const [codeBad, setCodeBad] = useState(false)
  // The reset code that passed step 2, used again by step 3.
  const [resetCode, setResetCode] = useState('')
  const [error, setError] = useState<string | null>(null)
  const [notice, setNotice] = useState<string | null>(null)
  const [busy, setBusy] = useState(false)
  const [resendAt, setResendAt] = useState(0)
  const [now, setNow] = useState(() => Date.now())
  // Google/GitHub show only when the server has them set up (none if it can't be reached).
  const [providers, setProviders] = useState<('google' | 'github')[]>([])
  useEffect(() => {
    let live = true
    void cloud.config().then((c) => live && setProviders(c?.providers ?? []))
    return () => {
      live = false
    }
  }, [cloud])

  const mail = email.trim()
  const waiting = Math.max(0, Math.ceil((resendAt - now) / 1000))
  useEffect(() => {
    if (waiting <= 0) return
    const t = setInterval(() => setNow(Date.now()), 1000)
    return () => clearInterval(t)
  }, [waiting])

  const clearCode = (bad = false): void => {
    setDigits(emptyDigits())
    setCodeKey((k) => k + 1)
    setCodeBad(bad)
  }
  const go = (next: Screen, opts: { notice?: string; error?: string } = {}): void => {
    setScreen(next)
    setError(opts.error ?? null)
    setNotice(opts.notice ?? null)
    setPassword('')
    setConfirm('')
    setShowPassword(false)
    clearCode(Boolean(opts.error))
  }
  const startWait = (): void => {
    setNow(Date.now())
    setResendAt(Date.now() + RESEND_WAIT * 1000)
  }

  /** Runs one server call; a local problem (string) stops it before it's sent. */
  const run = async (
    problem: string | null,
    call: () => Promise<CloudResult | CloudSignInResult>,
    done?: () => void
  ): Promise<void> => {
    setError(problem)
    if (problem) return
    setBusy(true)
    setNotice(null)
    setCodeBad(false)
    try {
      const r = await call()
      if (r.ok) done?.()
      else if (r.code === 'email_not_verified') {
        // The server just emailed a code: confirming it signs in.
        go('verify', { notice: "Your email isn't confirmed yet, so we sent you a new code." })
        startWait()
      } else if (screen === 'resetPassword' && r.code && CODE_ERRORS.has(r.code)) {
        // The code went stale while the password was chosen: ask for it again.
        go('resetCode', { error: `${r.error}. We can send you a new one.` })
      } else {
        setError(r.error)
        if (r.code && CODE_ERRORS.has(r.code)) clearCode(true)
      }
    } finally {
      setBusy(false)
    }
  }

  // Code screens: the boxes submit by themselves once all six digits are in.
  const confirmEmail = (otp: string): void => {
    void run(codeComplete(otp) ? null : `Enter the ${OTP_LENGTH}-digit code.`, () =>
      cloud.emailVerify(mail, otp)
    )
  }
  const checkResetCode = (otp: string): void => {
    void run(
      codeComplete(otp) ? null : `Enter the ${OTP_LENGTH}-digit code.`,
      () => cloud.emailResetCheck(mail, otp),
      () => {
        setResetCode(otp)
        go('resetPassword')
      }
    )
  }

  const submit = (e: FormEvent): void => {
    e.preventDefault()
    const code = digits.join('')
    switch (screen) {
      case 'signIn':
        void run(emailProblem(mail) ?? (password ? null : 'Enter your password.'), () =>
          cloud.emailSignIn(mail, password)
        )
        break
      case 'signUp':
        void run(
          nameProblem(name) ?? emailProblem(mail) ?? newPasswordProblem(password, confirm),
          () => cloud.emailSignUp(name.trim(), mail, password),
          () => {
            go('verify')
            startWait()
          }
        )
        break
      case 'verify':
        confirmEmail(code)
        break
      case 'forgot':
        void run(
          emailProblem(mail),
          () => cloud.emailReset(mail),
          () => {
            go('resetCode')
            startWait()
          }
        )
        break
      case 'resetCode':
        checkResetCode(code)
        break
      case 'resetPassword':
        void run(newPasswordProblem(password, confirm), () =>
          cloud.emailResetConfirm(mail, resetCode, password)
        )
    }
  }

  const resend = (): void => {
    void run(
      null,
      () => (screen === 'resetCode' ? cloud.emailReset(mail) : cloud.emailResend(mail)),
      () => {
        startWait()
        clearCode()
        setNotice(`We sent a new code to ${mail}.`)
      }
    )
  }

  const browser = (provider: 'google' | 'github'): void => {
    void run(null, () => cloud.signIn(provider)).then(() => {
      setError((e) => (e === 'Sign-in canceled.' ? null : e))
    })
  }

  if (pending) {
    return (
      <Card title="Finish signing in in your browser" sub="Wraithgrid opens when you're done.">
        <button className="btn auth-wide" type="button" onClick={() => void cloud.cancel()}>
          Cancel
        </button>
      </Card>
    )
  }

  const copy: Record<Screen, { title: string; sub: ReactNode; submit: string; busy: string }> = {
    signIn: {
      title: 'Sign in to Wraithgrid',
      sub: 'Your plan and Wraithgrid features, on every computer.',
      submit: 'Sign in',
      busy: 'Signing in…'
    },
    signUp: {
      title: 'Create your account',
      sub: 'One account for Wraithgrid on all your computers.',
      submit: 'Create account',
      busy: 'Creating account…'
    },
    verify: {
      title: 'Check your email',
      sub: (
        <>
          Enter the {OTP_LENGTH}-digit code we sent to <strong>{mail}</strong>.
        </>
      ),
      submit: 'Confirm email',
      busy: 'Checking code…'
    },
    forgot: {
      title: 'Reset your password',
      sub: `Enter your account's email and we'll send you a ${OTP_LENGTH}-digit code.`,
      submit: 'Send code',
      busy: 'Sending code…'
    },
    resetCode: {
      title: 'Enter the code',
      sub: (
        <>
          We sent a {OTP_LENGTH}-digit code to <strong>{mail}</strong>. It works for 10 minutes.
        </>
      ),
      submit: 'Continue',
      busy: 'Checking code…'
    },
    resetPassword: {
      title: 'Choose a new password',
      sub: `At least ${MIN_PASSWORD} characters. Your other computers will be signed out.`,
      submit: 'Save password and sign in',
      busy: 'Saving…'
    }
  }
  const c = copy[screen]
  const step = RESET_STEP[screen]
  const back = BACK[screen]
  const codeScreen = screen === 'verify' || screen === 'resetCode'

  const showToggle = (
    <button
      className="auth-link"
      type="button"
      onClick={() => setShowPassword((v) => !v)}
      aria-pressed={showPassword}
    >
      {showPassword ? 'Hide' : 'Show'}
    </button>
  )
  const passwordType = showPassword ? 'text' : 'password'

  return (
    <Card
      title={c.title}
      sub={c.sub}
      eyebrow={step ? `Step ${step} of 3` : undefined}
      back={back ? () => go(back) : undefined}
    >
      <form onSubmit={submit} noValidate aria-labelledby="auth-title" className="auth-form">
        {screen === 'signUp' ? (
          <Field label="Name">
            <input
              className="inpt sans"
              value={name}
              onChange={(e) => setName(e.target.value)}
              autoComplete="name"
              maxLength={100}
              autoFocus
              disabled={busy}
            />
          </Field>
        ) : null}

        {screen === 'signIn' || screen === 'signUp' || screen === 'forgot' ? (
          <Field label="Email">
            <input
              className="inpt sans"
              type="email"
              value={email}
              onChange={(e) => setEmail(e.target.value)}
              autoComplete="email"
              spellCheck={false}
              autoFocus={screen !== 'signUp'}
              disabled={busy}
            />
          </Field>
        ) : null}

        {codeScreen ? (
          <>
            <OtpInput
              key={codeKey}
              digits={digits}
              onChange={(next) => {
                setDigits(next)
                setCodeBad(false)
              }}
              onComplete={screen === 'verify' ? confirmEmail : checkResetCode}
              disabled={busy}
              invalid={codeBad}
              describedBy="auth-code-help"
            />
            <div className="auth-row">
              <span className="auth-help" id="auth-code-help">
                Didn&apos;t get it? Check spam, or
              </span>
              <button
                className="auth-link"
                type="button"
                onClick={resend}
                disabled={busy || waiting > 0}
              >
                {waiting > 0 ? resendLabel(waiting).toLowerCase() : 'send a new code'}
              </button>
            </div>
          </>
        ) : null}

        {screen === 'signIn' ? (
          <Field
            label="Password"
            aside={
              <button className="auth-link" type="button" onClick={() => go('forgot')}>
                Forgot password?
              </button>
            }
          >
            <input
              className="inpt sans"
              type="password"
              value={password}
              onChange={(e) => setPassword(e.target.value)}
              autoComplete="current-password"
              maxLength={128}
              disabled={busy}
            />
          </Field>
        ) : null}

        {screen === 'signUp' || screen === 'resetPassword' ? (
          <>
            <Field
              label={screen === 'resetPassword' ? 'New password' : 'Password'}
              aside={showToggle}
            >
              <input
                className="inpt sans"
                type={passwordType}
                value={password}
                onChange={(e) => setPassword(e.target.value)}
                autoComplete="new-password"
                maxLength={128}
                aria-describedby="auth-pw-help"
                autoFocus={screen === 'resetPassword'}
                disabled={busy}
              />
              <span className="auth-help" id="auth-pw-help">
                At least {MIN_PASSWORD} characters.
              </span>
            </Field>
            <Field label="Confirm password">
              <input
                className="inpt sans"
                type={passwordType}
                value={confirm}
                onChange={(e) => setConfirm(e.target.value)}
                autoComplete="new-password"
                maxLength={128}
                disabled={busy}
              />
            </Field>
          </>
        ) : null}

        {notice ? (
          <p className="auth-msg" role="status">
            {notice}
          </p>
        ) : null}
        {error ? (
          <p className="auth-msg err" role="alert">
            {error}
          </p>
        ) : null}

        <button className="btn pri auth-wide" type="submit" disabled={busy}>
          {busy ? c.busy : c.submit}
        </button>
      </form>

      {screen === 'signIn' ? (
        <>
          {providers.length > 0 ? (
            <div className="auth-or" role="separator">
              <span>or</span>
            </div>
          ) : null}
          {(['google', 'github'] as const)
            .filter((p) => providers.includes(p))
            .map((p) => (
              <button
                key={p}
                className="btn auth-wide"
                type="button"
                onClick={() => browser(p)}
                disabled={busy}
              >
                Continue with {p === 'google' ? 'Google' : 'GitHub'}
              </button>
            ))}
          <p className="auth-foot">
            New to Wraithgrid?{' '}
            <button className="auth-link" type="button" onClick={() => go('signUp')}>
              Create an account
            </button>
          </p>
        </>
      ) : null}
      {screen === 'signUp' ? (
        <p className="auth-foot">
          Already have an account?{' '}
          <button className="auth-link" type="button" onClick={() => go('signIn')}>
            Sign in
          </button>
        </p>
      ) : null}
    </Card>
  )
}

function Card({
  title,
  sub,
  eyebrow,
  back,
  children
}: {
  title: string
  sub: ReactNode
  eyebrow?: string
  back?: () => void
  children: ReactNode
}) {
  return (
    <div className="auth-card">
      <div className="auth-brand">
        <Logo />
        <span>Wraithgrid</span>
      </div>
      {back ? (
        <button className="auth-back" type="button" onClick={back}>
          ← Back
        </button>
      ) : null}
      {eyebrow ? <p className="auth-eyebrow">{eyebrow}</p> : null}
      <h1 id="auth-title">{title}</h1>
      <p className="auth-sub">{sub}</p>
      {children}
    </div>
  )
}

function Field({
  label,
  aside,
  children
}: {
  label: string
  aside?: ReactNode
  children: ReactNode
}) {
  return (
    <div className="auth-field">
      <div className="auth-label">
        <label>
          {label}
          {children}
        </label>
        {aside ? <span className="auth-aside">{aside}</span> : null}
      </div>
    </div>
  )
}
