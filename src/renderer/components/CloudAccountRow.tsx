// Settings → General: the signed-in Wraithgrid account (wraithgrid-server), not the agent CLIs'
// accounts. Signing in happens on the account gate (AuthGate); this edits the name, changes the
// password (email accounts) and signs out.
import { useState, type FormEvent } from 'react'
import { useCloud } from '../app/cloud'
import { nameProblem, newPasswordProblem } from '../lib/auth-forms'
import { Initial } from './AccountIcon'

const PLAN_LABEL = { free: 'Free', pro: 'Pro', team: 'Team' } as const
const PROVIDER_LABEL = { google: 'Google', github: 'GitHub', email: 'Email' } as const

export function CloudAccountRow() {
  const account = useCloud()
  const [editing, setEditing] = useState<'name' | 'password' | null>(null)
  const [name, setName] = useState('')
  const [current, setCurrent] = useState('')
  const [next, setNext] = useState('')
  const [confirm, setConfirm] = useState('')
  const [error, setError] = useState<string | null>(null)
  const [notice, setNotice] = useState<string | null>(null)
  const [busy, setBusy] = useState(false)

  const user = account?.status?.user
  if (!account || !user) return null
  const { cloud } = account

  const close = (): void => {
    setEditing(null)
    setError(null)
    setCurrent('')
    setNext('')
    setConfirm('')
  }
  const open = (what: 'name' | 'password'): void => {
    close()
    setNotice(null)
    setEditing(what)
    if (what === 'name') setName(user.name)
  }

  const save = (e: FormEvent): void => {
    e.preventDefault()
    const problem =
      editing === 'name'
        ? nameProblem(name)
        : current
          ? newPasswordProblem(next, confirm)
          : 'Enter your current password.'
    setError(problem)
    if (problem) return
    setBusy(true)
    void (editing === 'name' ? cloud.updateName(name.trim()) : cloud.changePassword(current, next))
      .then((r) => {
        if (!r.ok) return setError(r.error)
        setNotice(
          editing === 'name'
            ? 'Name saved.'
            : 'Password changed. Your other computers were signed out.'
        )
        close()
      })
      .finally(() => setBusy(false))
  }

  const signOut = (): void => {
    setBusy(true)
    void cloud.signOut().finally(() => setBusy(false))
  }

  return (
    <div className="srow">
      <div>
        <h3 id="cloud-h">Wraithgrid account</h3>
        <p className="ex">
          Your Wraithgrid sign-in on this computer. Agent CLI accounts are separate and stay on your
          computer.
        </p>
      </div>
      <div className="ctl" aria-labelledby="cloud-h">
        <div className="acct-profile">
          <Initial name={user.name} size={36} />
          <span className="who">
            <strong>{user.name}</strong>
            <span className="hint">
              {user.email} · {PLAN_LABEL[user.plan]} plan
              {user.status === 'past_due' ? ' · payment due' : ''}
            </span>
            <span className="hint">
              Signs in with {user.providers.map((p) => PROVIDER_LABEL[p]).join(', ') || 'email'}
            </span>
          </span>
        </div>

        {editing ? (
          <form className="acct-forms" onSubmit={save} noValidate>
            {editing === 'name' ? (
              <label>
                Name
                <input
                  className="inpt sans"
                  value={name}
                  onChange={(e) => setName(e.target.value)}
                  maxLength={100}
                  autoComplete="name"
                  autoFocus
                  disabled={busy}
                />
              </label>
            ) : (
              <>
                <label>
                  Current password
                  <input
                    className="inpt sans"
                    type="password"
                    value={current}
                    onChange={(e) => setCurrent(e.target.value)}
                    autoComplete="current-password"
                    maxLength={128}
                    autoFocus
                    disabled={busy}
                  />
                </label>
                <label>
                  New password (12+ characters)
                  <input
                    className="inpt sans"
                    type="password"
                    value={next}
                    onChange={(e) => setNext(e.target.value)}
                    autoComplete="new-password"
                    maxLength={128}
                    disabled={busy}
                  />
                </label>
                <label>
                  Confirm new password
                  <input
                    className="inpt sans"
                    type="password"
                    value={confirm}
                    onChange={(e) => setConfirm(e.target.value)}
                    autoComplete="new-password"
                    maxLength={128}
                    disabled={busy}
                  />
                </label>
              </>
            )}
            {error ? (
              <span className="hint warn" role="alert">
                {error}
              </span>
            ) : null}
            <div className="inrow">
              <button className="btn pri" type="submit" disabled={busy}>
                {busy ? 'Saving…' : editing === 'name' ? 'Save name' : 'Change password'}
              </button>
              <button className="btn gh" type="button" onClick={close} disabled={busy}>
                Cancel
              </button>
            </div>
          </form>
        ) : (
          <div className="inrow" style={{ marginTop: 10 }}>
            <button className="btn" onClick={() => open('name')} disabled={busy}>
              Edit name
            </button>
            {user.providers.includes('email') ? (
              <button className="btn" onClick={() => open('password')} disabled={busy}>
                Change password
              </button>
            ) : null}
            <button className="btn" onClick={signOut} disabled={busy}>
              Sign out
            </button>
          </div>
        )}
        {notice ? (
          <span className="hint" role="status">
            {notice}
          </span>
        ) : null}
      </div>
    </div>
  )
}
