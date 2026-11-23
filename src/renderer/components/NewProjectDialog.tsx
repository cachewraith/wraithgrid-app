// New project (#26): name, folder, default account, machine and a first chat in one step,
// opened from the sidebar's + and the palette. The switcher only switches/manages. The folder
// is a local one, or a Git repository cloned into a folder the user picks (Tauri app only;
// `git` fills it in from a `wraithgrid://clone` link, which never clones without Create).
import { useEffect, useState } from 'react'
import { contractHome } from '@shared/paths'
import { AGENT_CLI_LABEL } from '@shared/agent-clis'
import { Initial } from './AccountIcon'
import { useActions, useApp, useServices } from '../app/services'
import type { NewProjectGit } from '../app/store'
import { Dialog } from './Dialog'
import { FolderPicker } from './FolderPicker'
import { IconBranch, IconCheck, IconClose, IconFolder, IconGlobe } from './icons'
import { IS_MAC } from '../app/shortcuts'
import { middleTruncate, splitFolder } from '../lib/folder-path'
import { BRANCH_MAX, normalizeCloneUrl, repoDirName, repoLabel, validBranch } from '@shared/git-url'

type Source = 'folder' | 'git'

export function NewProjectDialog({ git }: { git?: NewProjectGit }) {
  const { api } = useServices()
  const actions = useActions()
  const accounts = useApp((s) => s.config.accounts)
  const machines = useApp((s) => s.config.machines)
  const settings = useApp((s) => s.config.settings)
  const recent = useApp((s) => s.config.recentFolders)
  const home = useApp((s) => s.info.homeDir)
  const plans = useApp((s) => s.accountInfo)
  const workspaces = useApp((s) => s.config.workspaces)
  const canClone = typeof api.git.clone === 'function'

  // A link's repository names the project, as a folder pick does.
  const [name, setName] = useState(git ? repoDirName(git.repo) : '')
  // The last name we filled in from a folder pick: typing your own name stops the suggesting.
  const [suggested, setSuggested] = useState(name)
  const [folder, setFolder] = useState('')
  const [accountId, setAccountId] = useState<string | null>(
    accounts.find((a) => a.id === settings.defaultAccountId)?.id ?? accounts[0]?.id ?? null
  )
  const [machineId, setMachineId] = useState<string | null>(null)
  const [startChat, setStartChat] = useState(accounts.length > 0)
  const [error, setError] = useState<string | null>(null)
  /** Which folder the picker is choosing: the project's, or where a clone goes. */
  const [picking, setPicking] = useState<Source | null>(null)

  const [source, setSource] = useState<Source>(git && canClone ? 'git' : 'folder')
  const [repo, setRepo] = useState(git?.repo ?? '')
  const [branch, setBranch] = useState(git?.branch ?? '')
  const [parent, setParent] = useState(settings.defaultCwd || '~')
  const [cloning, setCloning] = useState(false)
  const prompt = git?.prompt ?? ''
  const repoUrl = normalizeCloneUrl(repo)
  const repoBad = repo.trim() !== '' && !repoUrl
  const branchBad = branch.trim() !== '' && !validBranch(branch.trim())
  const isGit = source === 'git'

  // The chosen folder, checked in main: exists, and its git branch.
  const [checked, setChecked] = useState<{
    folder: string
    ok: boolean
    branch: string | null
    error: string | null
  } | null>(null)
  useEffect(() => {
    let live = true
    const f = folder.trim()
    if (!f) return
    void api.fs.listFolder(f).then((r) => {
      if (live)
        setChecked({
          folder: f,
          ok: r.ok,
          branch: r.ok ? r.branch : null,
          error: r.ok ? null : r.error
        })
    })
    return () => {
      live = false
    }
  }, [api, folder])
  const folderState = checked?.folder === folder.trim() ? checked : null

  const pickFolder = (p: string): void => {
    if (picking === 'git') {
      setParent(p)
      setPicking(null)
      return
    }
    setFolder(p)
    setPicking(null)
    setError(null)
    const base = splitFolder(p).name
    if (base && (!name.trim() || name === suggested)) {
      setName(base)
      setSuggested(base)
    }
  }
  const typeRepo = (v: string): void => {
    setRepo(v)
    setError(null)
    const url = normalizeCloneUrl(v)
    const base = url ? repoDirName(url) : ''
    if (base && (!name.trim() || name === suggested)) {
      setName(base)
      setSuggested(base)
    }
  }
  const browse = async (): Promise<void> => {
    const start = picking === 'git' ? parent : folder
    const picked = await api.dialog.pickFolder(start || settings.defaultCwd)
    if (picked) pickFolder(contractHome(picked, home))
  }
  const togglePin = (p: string): void =>
    actions.updateSettings({
      pinnedFolders: settings.pinnedFolders.includes(p)
        ? settings.pinnedFolders.filter((x) => x !== p)
        : [p, ...settings.pinnedFolders].slice(0, 20)
    })

  const canCreate =
    !cloning &&
    name.trim() !== '' &&
    (isGit
      ? !!repoUrl && !branchBad && parent.trim() !== ''
      : folder.trim() !== '' && folderState?.ok !== false)
  const submit = async (e?: React.FormEvent): Promise<void> => {
    e?.preventDefault()
    if (cloning) return
    if (!canCreate) {
      setError(
        !name.trim()
          ? 'Give the project a name.'
          : isGit
            ? 'Paste the address of a Git repository.'
            : 'Pick the project’s folder.'
      )
      return
    }
    // Checked before a clone that could take minutes, not after it.
    if (workspaces.some((w) => w.name.toLowerCase() === name.trim().toLowerCase())) {
      setError(`A project named ${name.trim()} exists.`)
      return
    }
    let dir = folder.trim()
    if (isGit && repoUrl && api.git.clone) {
      setCloning(true)
      setError(null)
      const r = await api.git.clone(repoUrl, parent.trim(), branch.trim() || null)
      setCloning(false)
      if (!r.ok) {
        setError(r.error)
        return
      }
      dir = r.dir
    }
    const err = actions.createProject({
      name,
      folder: dir,
      accountId,
      machineId: isGit ? null : machineId,
      startChat: startChat && !!accountId,
      prompt: isGit ? prompt : undefined
    })
    if (err) setError(err)
  }
  const { name: folderName, parent: folderParent } = splitFolder(folder.trim())
  // A clone runs on: closing the dialog would only hide it, and the folder would still appear.
  const close = (): void => {
    if (!cloning) actions.closeModal()
  }

  if (picking)
    return (
      <Dialog onClose={actions.closeModal} labelledBy="npj-t" className="pinned np">
        <div className="dlg-hd">
          <h2 id="npj-t">{picking === 'git' ? 'Clone into…' : 'Choose a folder'}</h2>
          <button
            type="button"
            className="icon-btn"
            aria-label="Back to New project"
            title="Back (Esc)"
            onClick={() => setPicking(null)}
          >
            <IconClose />
          </button>
        </div>
        <FolderPicker
          start={(picking === 'git' ? parent : folder).trim() || settings.defaultCwd}
          recent={recent}
          pinned={settings.pinnedFolders}
          mac={IS_MAC}
          onPick={pickFolder}
          onClose={() => setPicking(null)}
          onTogglePin={togglePin}
          onSystem={() => void browse()}
        />
      </Dialog>
    )

  return (
    <Dialog onClose={close} labelledBy="npj-t" className="pinned np">
      <form onSubmit={(e) => void submit(e)} aria-busy={cloning}>
        <div className="dlg-hd">
          <h2 id="npj-t">New project</h2>
          <button
            type="button"
            className="icon-btn"
            aria-label="Close dialog (Esc)"
            title="Close (Esc)"
            disabled={cloning}
            onClick={close}
          >
            <IconClose />
          </button>
        </div>
        <div className="dlg-bd">
          {canClone ? (
            <div className="fld">
              <span className="lbl" id="npj-src">
                Start from
              </span>
              <div className="seg" role="radiogroup" aria-labelledby="npj-src">
                {(
                  [
                    ['folder', 'A folder on this computer'],
                    ['git', 'A Git repository']
                  ] as const
                ).map(([id, label]) => (
                  <button
                    type="button"
                    key={id}
                    className={`seg-btn${source === id ? ' on' : ''}`}
                    role="radio"
                    aria-checked={source === id}
                    disabled={cloning}
                    onClick={() => {
                      setSource(id)
                      setError(null)
                    }}
                  >
                    {label}
                  </button>
                ))}
              </div>
            </div>
          ) : null}

          {isGit ? (
            <>
              <div className="fld">
                <label className="lbl" htmlFor="npj-repo">
                  Repository
                </label>
                <input
                  id="npj-repo"
                  className="inpt"
                  value={repo}
                  onChange={(e) => typeRepo(e.target.value)}
                  placeholder="https://github.com/owner/repo.git or owner/repo"
                  spellCheck={false}
                  autoComplete="off"
                  maxLength={2000}
                  disabled={cloning}
                  aria-invalid={repoBad}
                  aria-describedby="npj-repo-h"
                  data-autofocus={git ? undefined : true}
                />
                <span id="npj-repo-h" className={`hint${repoBad ? ' err' : ''}`}>
                  {repoBad
                    ? 'Use an https:// or SSH address (git@host:owner/repo), or GitHub’s owner/repo.'
                    : 'Private repositories use your git credential helper or SSH key.'}
                </span>
              </div>
              <div className="fld">
                <label className="lbl" htmlFor="npj-branch">
                  Branch <span className="np-opt">(optional)</span>
                </label>
                <input
                  id="npj-branch"
                  className="inpt"
                  value={branch}
                  onChange={(e) => {
                    setBranch(e.target.value)
                    setError(null)
                  }}
                  placeholder="The repository’s default branch"
                  spellCheck={false}
                  autoComplete="off"
                  maxLength={BRANCH_MAX}
                  disabled={cloning}
                  aria-invalid={branchBad}
                />
                {branchBad ? (
                  <span className="hint err" role="alert">
                    Not a valid branch name.
                  </span>
                ) : null}
              </div>
              <div className="fld">
                <span className="lbl" id="npj-into-l">
                  Clone into
                </span>
                <button
                  type="button"
                  className="np-folder"
                  aria-labelledby="npj-into-l npj-into-v"
                  disabled={cloning}
                  onClick={() => setPicking('git')}
                >
                  <IconFolder />
                  <span id="npj-into-v" className="np-fv" title={parent}>
                    <span className="np-fn">{repoUrl ? repoDirName(repoUrl) : 'repository'}</span>
                    <span className="np-fp">in {middleTruncate(parent, 48)}</span>
                  </span>
                  <span className="np-change">Change</span>
                </button>
              </div>
              {prompt ? (
                <div className="note np-ask">
                  <span>
                    The first chat opens with this question in its message box, ready for you to
                    send:
                    <q>{prompt.length > 240 ? `${prompt.slice(0, 240)}…` : prompt}</q>
                  </span>
                </div>
              ) : null}
            </>
          ) : (
            <div className="fld">
              <span className="lbl" id="npj-dir-l">
                Folder
              </span>
              {folder.trim() ? (
                <button
                  type="button"
                  className={`np-folder${folderState?.ok === false ? ' bad' : ''}`}
                  aria-labelledby="npj-dir-l npj-dir-v"
                  onClick={() => setPicking('folder')}
                >
                  <IconFolder />
                  <span id="npj-dir-v" className="np-fv" title={folder}>
                    <span className="np-fn">{folderName}</span>
                    {folderParent ? (
                      <span className="np-fp">{middleTruncate(folderParent, 48)}</span>
                    ) : null}
                  </span>
                  {folderState?.branch ? (
                    <span className="np-br">
                      <IconBranch small />
                      {folderState.branch}
                    </span>
                  ) : null}
                  <span className="np-change">Change</span>
                </button>
              ) : (
                <button type="button" className="np-folder" onClick={() => setPicking('folder')}>
                  <IconFolder />
                  <span className="np-fv">
                    <span className="np-fn">Choose a folder…</span>
                    <span className="np-fp">New chats in the project start there</span>
                  </span>
                </button>
              )}
              {folderState?.ok === false ? (
                <span className="hint err" role="alert">
                  {folderState.error}
                </span>
              ) : null}
            </div>
          )}

          <div className="fld">
            <label className="lbl" htmlFor="npj-name">
              Name
            </label>
            <input
              id="npj-name"
              className="inpt sans"
              value={name}
              onChange={(e) => {
                setName(e.target.value)
                setError(null)
              }}
              placeholder={suggested || 'e.g. wraithgrid'}
              maxLength={64}
              disabled={cloning}
              data-autofocus={!isGit || git ? true : undefined}
            />
          </div>

          {accounts.length ? (
            <div className="fld">
              <span className="lbl" id="npj-acc">
                Account
              </span>
              <div className="np-accts" role="radiogroup" aria-labelledby="npj-acc">
                {accounts.map((a) => {
                  const on = a.id === accountId
                  const plan = plans[a.id]?.info?.plan
                  return (
                    <button
                      type="button"
                      key={a.id}
                      className={`np-acct${on ? ' on' : ''}`}
                      role="radio"
                      aria-checked={on}
                      onClick={() => setAccountId(a.id)}
                    >
                      <Initial name={a.name} size={20} />
                      <span className="nm">{a.name}</span>
                      <span className="sb" title={AGENT_CLI_LABEL[a.cli]}>
                        {[a.cli, plan].filter(Boolean).join(' · ')}
                      </span>
                      {a.signedIn ? null : <span className="sb warn">not signed in</span>}
                      {on ? <IconCheck className="ck" /> : null}
                    </button>
                  )
                })}
              </div>
              <span className="hint">The default account for the project’s chats.</span>
            </div>
          ) : (
            <div className="note">
              <span>
                No accounts yet; the project can still be made.{' '}
                <button
                  type="button"
                  className="btn gh"
                  onClick={() => actions.setAccountsMode('add')}
                >
                  Add account
                </button>
              </span>
            </div>
          )}

          {machines.length && !isGit ? (
            <div className="fld">
              <span className="lbl" id="npj-run">
                Runs on
              </span>
              <div className="seg" role="radiogroup" aria-labelledby="npj-run">
                {[{ id: null as string | null, name: 'This computer' }, ...machines].map((m) => (
                  <button
                    type="button"
                    key={m.id ?? 'local'}
                    className={`seg-btn${machineId === m.id ? ' on' : ''}`}
                    role="radio"
                    aria-checked={machineId === m.id}
                    onClick={() => setMachineId(m.id)}
                  >
                    {m.name}
                  </button>
                ))}
              </div>
            </div>
          ) : null}

          <div className="fld">
            <span className="lbl" id="npj-start">
              Start with
            </span>
            <div className="seg" role="radiogroup" aria-labelledby="npj-start">
              <button
                type="button"
                className={`seg-btn${startChat && accountId ? ' on' : ''}`}
                role="radio"
                aria-checked={startChat && !!accountId}
                disabled={!accountId}
                title={accountId ? undefined : 'Needs an account'}
                onClick={() => setStartChat(true)}
              >
                The first chat open
              </button>
              <button
                type="button"
                className={`seg-btn${!startChat || !accountId ? ' on' : ''}`}
                role="radio"
                aria-checked={!startChat || !accountId}
                onClick={() => setStartChat(false)}
              >
                An empty project
              </button>
            </div>
          </div>

          {error ? (
            <div className="note" role="alert" style={{ color: 'var(--err)' }}>
              {error}
            </div>
          ) : null}
          {cloning ? (
            <div className="note" role="status">
              <IconGlobe /> Cloning {repoUrl ? repoLabel(repoUrl) : 'the repository'}… a big
              repository can take a few minutes.
            </div>
          ) : null}
        </div>
        <div className="dlg-ft">
          <span className="hint">Enter to create · Esc to cancel</span>
          <button type="button" className="btn gh" disabled={cloning} onClick={close}>
            Cancel
          </button>
          <button type="submit" className="btn pri" disabled={!canCreate}>
            {cloning ? 'Cloning…' : isGit ? 'Clone and create' : 'Create project'}
          </button>
        </div>
      </form>
    </Dialog>
  )
}
