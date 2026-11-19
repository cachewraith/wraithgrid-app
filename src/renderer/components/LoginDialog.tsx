// Signs a claude or codex account in without a terminal: main runs the CLI's own login in
// the account's folder; this shows its link, takes the code the page may show (claude) or
// shows the one-time code to type there (codex), and says who is signed in at the end.
// Tokens never pass through Wraithgrid. `useLogin` + `LoginBody` are shared with Add account.
import { useEffect, useState } from 'react'
import type { LoginEvent } from '@shared/ipc-contract'
import { useActions, useApp, useServices } from '../app/services'
import { accountById } from '../app/store'
import { CopyButton } from './chat/Markdown'
import { Dialog } from './Dialog'

export type LoginStep =
  | { s: 'starting' }
  | { s: 'url'; url: string; sent: boolean; deviceCode: string | null }
  | { s: 'done'; email: string | null; plan: string | null }
  | { s: 'error'; error: string }

export interface Login {
  step: LoginStep
  code: string
  setCode(code: string): void
  /** Sends the pasted code (claude); a no-op until there is a link and a plausible code. */
  submit(): void
  retry(): void
  canSubmit: boolean
}

/** Runs one account's sign-in while mounted; unmounting before it ends cancels it in main. */
export function useLogin(accountId: string): Login {
  const { api } = useServices()
  const { markSignedIn } = useActions()
  const [step, setStep] = useState<LoginStep>({ s: 'starting' })
  const [code, setCode] = useState('')
  const [attempt, setAttempt] = useState(0)

  useEffect(() => {
    let ended = false
    const off = api.login.onEvent((e: LoginEvent) => {
      if (e.accountId !== accountId) return
      if (e.state === 'url')
        setStep({ s: 'url', url: e.url, sent: false, deviceCode: e.deviceCode ?? null })
      else {
        ended = true
        if (e.ok) {
          markSignedIn(accountId)
          setStep({ s: 'done', email: e.email, plan: e.plan })
        } else setStep({ s: 'error', error: e.error ?? 'Sign-in did not finish.' })
      }
    })
    void api.login.start(accountId).then((r) => {
      if (!r.ok) {
        ended = true
        setStep({ s: 'error', error: r.error })
      }
    })
    return () => {
      off()
      if (!ended) api.login.cancel(accountId)
    }
  }, [api, accountId, attempt, markSignedIn])

  const clean = code.trim()
  const canSubmit = step.s === 'url' && !step.sent && /^[\x21-\x7e]{1,2048}$/.test(clean)
  return {
    step,
    code,
    setCode,
    canSubmit,
    submit: () => {
      if (step.s !== 'url' || !canSubmit) return
      api.login.code(accountId, clean)
      setStep({ ...step, sent: true })
    },
    retry: () => {
      setCode('')
      setStep({ s: 'starting' })
      setAttempt((n) => n + 1)
    }
  }
}

/** The sign-in's link, code and result, without a frame or buttons. */
export function LoginBody({ login }: { login: Login }) {
  const { api } = useServices()
  const { step, code, setCode, submit } = login
  return (
    <>
      {step.s === 'starting' ? <p className="hint">Starting the sign-in…</p> : null}
      {step.s === 'url' && step.deviceCode ? (
        <>
          <p className="hint">Open the sign-in page and enter this one-time code there.</p>
          <div className="login-device">
            <span className="mono" aria-label="One-time code">
              {step.deviceCode}
            </span>
            <CopyButton text={step.deviceCode} label="Copy code" />
          </div>
          <div className="login-link">
            <button
              className="btn"
              onClick={() => void api.shell.openExternal(step.url)}
              data-autofocus
            >
              Open sign-in page
            </button>
            <CopyButton text={step.url} label="Copy link" />
          </div>
          <p className="hint">This moves on by itself once you finish in the browser.</p>
        </>
      ) : null}
      {step.s === 'url' && !step.deviceCode ? (
        <>
          <p className="hint">
            Your browser opens Claude’s sign-in page. If it didn’t, open it here.
          </p>
          <div className="login-link">
            <button
              className="btn"
              onClick={() => void api.shell.openExternal(step.url)}
              data-autofocus
            >
              Open sign-in page
            </button>
            <CopyButton text={step.url} label="Copy link" />
          </div>
          <label className="login-code">
            <span className="hint">If the page shows a code, paste it here.</span>
            <input
              className="inpt mono"
              value={code}
              placeholder="Code"
              aria-label="Sign-in code"
              spellCheck={false}
              disabled={step.sent}
              onChange={(e) => setCode(e.target.value)}
              onKeyDown={(e) => {
                if (e.key === 'Enter') submit()
              }}
            />
          </label>
          {step.sent ? <p className="hint">Checking the code…</p> : null}
        </>
      ) : null}
      {step.s === 'done' ? (
        <p className="hint">
          {[step.email ? `Signed in as ${step.email}` : 'Signed in', step.plan]
            .filter(Boolean)
            .join(' · ')}
          .
        </p>
      ) : null}
      {step.s === 'error' ? <div className="hint err">{step.error}</div> : null}
    </>
  )
}

/** The CLI's service as its sign-in page names it. */
export const loginService = (cli: string | undefined): string =>
  cli === 'codex' ? 'ChatGPT (Codex)' : 'Claude'

export function LoginDialog({ accountId }: { accountId: string }) {
  const actions = useActions()
  const { closeModal } = actions
  const account = useApp((s) => accountById(s.config, accountId))
  const login = useLogin(accountId)
  const { step } = login

  const name = account?.name ?? 'this account'
  return (
    <Dialog onClose={closeModal} className="w4" labelledBy="login-t">
      <div className="dlg-hd">
        <h2 id="login-t">
          {step.s === 'done'
            ? `${name} is signed in`
            : `Sign in to ${loginService(account?.cli)} as ${name}`}
        </h2>
      </div>
      <div className="dlg-bd login-bd">
        <LoginBody login={login} />
      </div>
      <div className="dlg-ft">
        {step.s === 'done' ? (
          <button className="btn pri" onClick={closeModal} data-autofocus>
            Done
          </button>
        ) : step.s === 'error' ? (
          <>
            <button
              className="btn gh"
              onClick={() => {
                closeModal()
                actions.loginInTerminal(accountId)
              }}
            >
              Use a terminal instead
            </button>
            <button className="btn pri" onClick={login.retry}>
              Try again
            </button>
          </>
        ) : (
          <>
            <button className="btn gh" onClick={closeModal}>
              Cancel
            </button>
            {step.s === 'url' && !step.deviceCode ? (
              <button className="btn pri" onClick={login.submit} disabled={!login.canSubmit}>
                Continue
              </button>
            ) : null}
          </>
        )}
      </div>
    </Dialog>
  )
}
