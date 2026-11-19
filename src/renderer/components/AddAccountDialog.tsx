// Add account in three steps: provider, sign-in (a new login, or one already on this machine),
// then a name filled in from that login. The account is only kept once the last step is
// confirmed: leaving earlier removes it, and deletes its config dir only if this dialog made it
// (an imported folder is never touched).
import { useEffect, useRef, useState } from 'react'
import { contractHome, expandHome } from '@shared/paths'
import {
  AGENT_CLIS,
  AGENT_CLI_SPECS,
  AGENT_PROVIDER_LABEL,
  DIALOG_LOGIN_CLIS,
  type AgentCli
} from '@shared/agent-clis'
import type { Account } from '@shared/types'
import { useActions, useApp, useServices } from '../app/services'
import { Dialog } from './Dialog'
import { LoginBody, loginService, useLogin } from './LoginDialog'
import { ProviderLogo } from './ProviderLogo'

/** The folder each CLI uses when started without Wraithgrid, offered as "reuse this login". */
const HOME_DIRS: Partial<Record<AgentCli, string>> = { claude: '.claude', codex: '.codex' }

type Step = 'provider' | 'method' | 'signin' | 'name'
/** An account this dialog already added; `madeDir`: its config dir is ours to delete. */
interface Draft {
  id: string
  madeDir: boolean
}

const signInNote = (cli: AgentCli): string => {
  if (AGENT_CLI_SPECS[cli].signIn === 'shared') return 'One login shared by all its accounts'
  if (DIALOG_LOGIN_CLIS.includes(cli)) return 'Sign in here, in your browser'
  return 'Signs in when its first pane starts'
}

const sameName = (a: string, b: string): boolean =>
  a.trim().toLowerCase() === b.trim().toLowerCase()

/** A name not used by another account: `base`, else `base-2`, `base-3`… */
function freeName(base: string, accounts: Account[], self: string | null): string {
  const taken = (n: string): boolean => accounts.some((a) => a.id !== self && sameName(a.name, n))
  if (!taken(base)) return base
  for (let i = 2; ; i++) if (!taken(`${base}-${i}`)) return `${base}-${i}`
}

/** From the login's email: its local part, with the domain added when that name is taken. */
function nameFromEmail(email: string, accounts: Account[], self: string | null): string {
  const [local = '', domain = ''] = email.toLowerCase().split('@')
  const base = local.slice(0, 48) || 'account'
  const taken = accounts.some((a) => a.id !== self && sameName(a.name, base))
  const org = domain.split('.')[0]
  return freeName(taken && org ? `${base}-${org}` : base, accounts, self)
}

export function AddAccountDialog({
  startAt,
  onAdded
}: {
  /** 'import': open on the sign-in choice for claude with the existing login picked. */
  startAt: 'add' | 'import'
  onAdded(id: string): void
}) {
  const { api, store } = useServices()
  const actions = useActions()
  const accounts = useApp((s) => s.config.accounts)
  const folders = useApp((s) => s.config.accountFolders)
  const home = useApp((s) => s.info.homeDir)
  const plans = useApp((s) => s.accountInfo)

  const [step, setStep] = useState<Step>(startAt === 'import' ? 'method' : 'provider')
  const [cli, setCli] = useState<AgentCli>('claude')
  const [method, setMethod] = useState<'new' | 'reuse'>(startAt === 'import' ? 'reuse' : 'new')
  const [reuseDir, setReuseDir] = useState<string | null>(null)
  const [homeDirs, setHomeDirs] = useState<Set<string>>(new Set())
  const [draft, setDraftState] = useState<Draft | null>(null)
  const [who, setWho] = useState<{ email: string | null; plan: string | null } | null>(null)
  const [name, setName] = useState('')
  const [folderId, setFolderId] = useState<string | null>(null)
  const [error, setError] = useState<string | null>(null)
  const [busy, setBusy] = useState(false)

  // Which CLI homes exist (`~/.claude`, `~/.codex`), for "reuse the login in …".
  useEffect(() => {
    let live = true
    void api.fs.listFolder(home || '~').then((r) => {
      if (live && r.ok) setHomeDirs(new Set(r.entries.filter((e) => e.hidden).map((e) => e.name)))
    })
    return () => {
      live = false
    }
  }, [api, home])

  // Leaving before the last step takes the half-made account back out.
  const pending = useRef<Draft | null>(null)
  const setDraft = (d: Draft | null): void => {
    pending.current = d
    setDraftState(d)
  }
  const { removeAccount } = actions
  useEffect(
    () => () => {
      const d = pending.current
      if (!d) return
      // A sign-in still running there stops before its folder goes.
      if (d.madeDir) api.login.cancel(d.id)
      void removeAccount(d.id, d.madeDir)
    },
    [api, removeAccount]
  )
  const discard = async (): Promise<void> => {
    const d = draft
    setDraft(null)
    setWho(null)
    if (!d) return
    if (d.madeDir) api.login.cancel(d.id)
    // If main won't delete the folder, the account still goes (an empty folder stays).
    if (await removeAccount(d.id, d.madeDir)) await removeAccount(d.id, false)
  }
  const close = (): void => actions.setAccountsMode('list')

  const isAccountDir = (dir: string): boolean =>
    accounts.some((a) => expandHome(a.configDir, home) === expandHome(dir, home))
  const homeDir = HOME_DIRS[cli]
  const homeLogin =
    homeDir && homeDirs.has(homeDir) && !isAccountDir(`~/${homeDir}`) ? `~/${homeDir}` : null
  const canDialog = DIALOG_LOGIN_CLIS.includes(cli)
  const chosenReuse = reuseDir ?? homeLogin
  // "Import" picks reuse up front; with no login to reuse, a new one is the only choice.
  const pick = method === 'reuse' && !chosenReuse ? 'new' : method

  const goName = (n: string): void => {
    setName(n)
    setError(null)
    setStep('name')
  }

  const fromProvider = (): void => {
    setError(null)
    setReuseDir(null)
    setMethod('new')
    // Nothing to choose: CLIs without the sign-in dialog and no login to reuse sign in later.
    if (canDialog || homeLogin) setStep('method')
    else goName('')
  }

  const fromMethod = async (): Promise<void> => {
    setError(null)
    if (pick === 'reuse') {
      if (!chosenReuse) return
      const res = actions.importAccount(chosenReuse, cli)
      if (!res.ok) return setError(res.error)
      setDraft({ id: res.id, madeDir: false })
      return goName(nameOf(res.id))
    }
    if (!canDialog) return goName('')
    setBusy(true)
    const provisional = freeName(AGENT_PROVIDER_LABEL[cli], accounts, null)
    const res = await actions.createAccount(provisional, cli)
    setBusy(false)
    if (!res.ok) return setError(res.error)
    setDraft({ id: res.id, madeDir: true })
    setStep('signin')
  }
  // The name the store gave (importAccount: from the folder's name), read back.
  const nameOf = (id: string): string =>
    store.getState().config.accounts.find((a) => a.id === id)?.name ?? ''

  const pickFolder = async (): Promise<void> => {
    const picked = await api.dialog.pickFolder(chosenReuse ?? '~')
    if (!picked) return
    setReuseDir(contractHome(picked, home))
    setMethod('reuse')
    setError(null)
  }

  const signedIn = (email: string | null, plan: string | null): void => {
    setWho({ email, plan })
    const self = draft?.id ?? null
    goName(
      email ? nameFromEmail(email, store.getState().config.accounts, self) : nameOf(self ?? '')
    )
  }

  // Someone else already added on this login (as far as their chats have said).
  const twin =
    who?.email && draft
      ? accounts.find(
          (a) =>
            a.id !== draft.id &&
            a.cli === cli &&
            plans[a.id]?.info?.email?.toLowerCase() === who.email!.toLowerCase()
        )
      : undefined

  const finish = async (open: boolean): Promise<void> => {
    const trimmed = name.trim()
    if (!trimmed) return setError('Enter a name.')
    const self = draft?.id ?? null
    if (accounts.some((a) => a.id !== self && sameName(a.name, trimmed)))
      return setError('An account with that name already exists.')
    let id = self
    if (!id) {
      setBusy(true)
      const res = await actions.createAccount(trimmed, cli)
      setBusy(false)
      if (!res.ok) return setError(res.error)
      id = res.id
    }
    actions.updateAccount(id, { name: trimmed, folderId })
    setDraft(null)
    close()
    onAdded(id)
    if (open) actions.openModal({ kind: 'newPane', slotId: null, accountId: id })
  }

  const steps: { key: Step[]; label: string }[] = [
    { key: ['provider'], label: 'Provider' },
    { key: ['method', 'signin'], label: 'Sign in' },
    { key: ['name'], label: 'Name' }
  ]
  const at = steps.findIndex((s) => s.key.includes(step))
  const title =
    step === 'provider'
      ? 'Add account'
      : step === 'method'
        ? `${AGENT_CLI_SPECS[cli].label} login`
        : step === 'signin'
          ? `Sign in to ${loginService(cli)}`
          : who?.email
            ? `Signed in as ${who.email}`
            : 'Name this account'

  return (
    <Dialog onClose={close} className="w4 add-acc" labelledBy="add-acc-t">
      <div className="dlg-hd">
        <ol className="add-steps" aria-label="Steps">
          {steps.map((s, i) => (
            <li
              key={s.label}
              className={i === at ? 'on' : i < at ? 'done' : undefined}
              aria-current={i === at ? 'step' : undefined}
            >
              <span>{i + 1}</span>
              {s.label}
            </li>
          ))}
        </ol>
        <h2 id="add-acc-t">{title}</h2>
      </div>

      {step === 'provider' ? (
        <>
          <div className="dlg-bd">
            <p className="hint add-lead">
              Which CLI will this account run? You can add several accounts of each.
            </p>
            <div className="add-opts" role="radiogroup" aria-label="Provider">
              {AGENT_CLIS.map((c) => {
                const n = accounts.filter((a) => a.cli === c).length
                return (
                  <button
                    key={c}
                    type="button"
                    role="radio"
                    aria-checked={cli === c}
                    className={`add-opt${cli === c ? ' on' : ''}`}
                    onClick={() => setCli(c)}
                  >
                    <span className="add-radio" aria-hidden="true" />
                    <span className="add-logo">
                      <ProviderLogo cli={c} size={14} />
                    </span>
                    <span className="add-tx">
                      <span className="t">{AGENT_CLI_SPECS[c].label}</span>
                      <span className="d">{signInNote(c)}</span>
                    </span>
                    {n ? <span className="add-n">{n} added</span> : null}
                  </button>
                )
              })}
            </div>
          </div>
          <div className="dlg-ft">
            <button className="btn gh" onClick={close}>
              Cancel
            </button>
            <button className="btn pri" onClick={fromProvider} data-autofocus>
              Continue
            </button>
          </div>
        </>
      ) : null}

      {step === 'method' ? (
        <>
          <div className="dlg-bd">
            <div className="add-opts" role="radiogroup" aria-label="Login">
              <button
                type="button"
                role="radio"
                aria-checked={pick === 'new'}
                className={`add-opt${pick === 'new' ? ' on' : ''}`}
                onClick={() => setMethod('new')}
              >
                <span className="add-radio" aria-hidden="true" />
                <span className="add-tx">
                  <span className="t">Sign in with a new login</span>
                  <span className="d">
                    {canDialog ? 'Opens your browser' : 'It asks when its first pane starts'}
                  </span>
                </span>
              </button>
              {chosenReuse ? (
                <button
                  type="button"
                  role="radio"
                  aria-checked={pick === 'reuse'}
                  className={`add-opt${pick === 'reuse' ? ' on' : ''}`}
                  onClick={() => setMethod('reuse')}
                >
                  <span className="add-radio" aria-hidden="true" />
                  <span className="add-tx">
                    <span className="t">
                      Reuse the login in <span className="mono">{chosenReuse}</span>
                    </span>
                    <span className="d">
                      {reuseDir
                        ? 'Used where it is; nothing is copied'
                        : 'The one your terminal uses now; nothing is copied'}
                    </span>
                  </span>
                </button>
              ) : null}
            </div>
            <button type="button" className="add-link" onClick={() => void pickFolder()}>
              Use another existing folder…
            </button>
            {error ? <div className="hint err">{error}</div> : null}
          </div>
          <div className="dlg-ft">
            <button
              className="btn gh"
              onClick={() => {
                setError(null)
                setStep('provider')
              }}
            >
              Back
            </button>
            <button
              className="btn pri"
              onClick={() => void fromMethod()}
              disabled={busy}
              data-autofocus
            >
              Continue
            </button>
          </div>
        </>
      ) : null}

      {step === 'signin' && draft ? (
        <SignInStep
          accountId={draft.id}
          onBack={() => {
            void discard()
            setStep('method')
          }}
          onSignedIn={signedIn}
        />
      ) : null}

      {step === 'name' ? (
        <>
          <div className="dlg-bd">
            <p className="hint add-lead">
              {[AGENT_CLI_SPECS[cli].label, who?.plan, pick === 'reuse' ? chosenReuse : null]
                .filter(Boolean)
                .join(' · ')}
            </p>
            {twin ? (
              <div className="add-msg warn" role="status">
                {who?.email} is already added as “{twin.name}”. Two accounts on one login share the
                same usage limits.
              </div>
            ) : null}
            <div className={folders.length ? 'add-row2' : undefined}>
              <div className="fld">
                <label className="lbl" htmlFor="add-acc-name">
                  Name
                </label>
                <input
                  id="add-acc-name"
                  className="inpt sans"
                  value={name}
                  maxLength={64}
                  placeholder="e.g. work"
                  autoFocus
                  onChange={(e) => {
                    setName(e.target.value)
                    setError(null)
                  }}
                  onKeyDown={(e) => {
                    if (e.key === 'Enter') void finish(false)
                  }}
                />
                <div className="hint">
                  {who?.email
                    ? 'From the email. Change it if you like.'
                    : 'Shown in pickers and pane headers.'}
                </div>
              </div>
              {folders.length ? (
                <div className="fld">
                  <label className="lbl" htmlFor="add-acc-folder">
                    Folder
                  </label>
                  <select
                    id="add-acc-folder"
                    className="inpt sans"
                    value={folderId ?? ''}
                    onChange={(e) => setFolderId(e.target.value || null)}
                  >
                    <option value="">No folder</option>
                    {folders.map((f) => (
                      <option key={f.id} value={f.id}>
                        {f.name}
                      </option>
                    ))}
                  </select>
                </div>
              ) : null}
            </div>
            {error ? <div className="hint err">{error}</div> : null}
          </div>
          <div className="dlg-ft">
            <button
              className="btn gh"
              onClick={() => {
                void discard()
                close()
              }}
            >
              Cancel
            </button>
            <span className="add-ft-r">
              <button className="btn" onClick={() => void finish(true)} disabled={busy}>
                Add and open a pane
              </button>
              <button className="btn pri" onClick={() => void finish(false)} disabled={busy}>
                Add account
              </button>
            </span>
          </div>
        </>
      ) : null}
    </Dialog>
  )
}

/** Step 2 for claude/codex: the CLI's own login, mounted only while it runs. */
function SignInStep({
  accountId,
  onBack,
  onSignedIn
}: {
  accountId: string
  onBack(): void
  onSignedIn(email: string | null, plan: string | null): void
}) {
  const login = useLogin(accountId)
  const { step } = login
  const done = useRef(onSignedIn)
  useEffect(() => {
    done.current = onSignedIn
  })
  useEffect(() => {
    if (step.s === 'done') done.current(step.email, step.plan)
  }, [step])

  return (
    <>
      <div className="dlg-bd login-bd">
        <LoginBody login={login} />
      </div>
      <div className="dlg-ft">
        <button className="btn gh" onClick={onBack}>
          Back
        </button>
        {step.s === 'error' ? (
          <button className="btn pri" onClick={login.retry}>
            Try again
          </button>
        ) : step.s === 'url' && !step.deviceCode ? (
          <button className="btn pri" onClick={login.submit} disabled={!login.canSubmit}>
            Continue
          </button>
        ) : (
          <span className="hint">Moves on by itself</span>
        )}
      </div>
    </>
  )
}
