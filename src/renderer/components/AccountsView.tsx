// The Accounts page: one table of every account, grouped by folder (drag a row onto a folder
// heading to move it), with a provider column, a ⋯ menu per row and a details panel for the
// selected one. Adding goes through AddAccountDialog.
import { useEffect, useRef, useState, type ReactNode } from 'react'
import {
  DndContext,
  PointerSensor,
  pointerWithin,
  rectIntersection,
  useDraggable,
  useDroppable,
  useSensor,
  useSensors,
  type CollisionDetection,
  type DragEndEvent
} from '@dnd-kit/core'
import { AGENT_CLIS, AGENT_CLI_SPECS, type AgentCli } from '@shared/agent-clis'
import { ACCOUNT_COLORS, type Account, type AccountFolder } from '@shared/types'
import { useActions, useApp, useServices } from '../app/services'
import { AddAccountDialog } from './AddAccountDialog'
import { Initial } from './AccountIcon'
import { CopyButton } from './chat/Markdown'
import { ContextMenu, menuPoint, type MenuItem, type MenuPoint } from './ContextMenu'
import { Dialog } from './Dialog'
import {
  IconCheck,
  IconClose,
  IconFolder,
  IconLogin,
  IconMore,
  IconPlus,
  IconRename,
  IconSearch,
  IconTerminal,
  IconTrash,
  IconWarn
} from './icons'
import { ProviderLogo } from './ProviderLogo'

/** What "Overall" sharing gives an account of each provider (main's SHARE_SPECS). */
const SHARED_NOTE: Partial<Record<Account['cli'], string>> = {
  claude: 'Shares CLAUDE.md, skills and plugins with your other Claude Code accounts.',
  codex: 'Shares AGENTS.md, skills and prompts with your other Codex accounts.',
  gemini: 'Shares GEMINI.md, commands and skills with your other Gemini accounts.',
  copilot: 'Shares instructions, agents and skills with your other Copilot accounts.'
}

const ROOT = 'grp:root'
const groupDropId = (id: string): string => `grp:${id}`
const rowDragId = (id: string): string => `acc:${id}`
// The zone under the pointer; if none, the one the dragged row overlaps.
const pointerThenRect: CollisionDetection = (args) => {
  const hits = pointerWithin(args)
  return hits.length ? hits : rectIntersection(args)
}

/** What the account's chats last said about its login: "email · plan", or null. */
function useLoginLine(id: string): string | null {
  const info = useApp((s) => s.accountInfo[id]?.info)
  const line = [info?.email, info?.plan].filter(Boolean).join(' · ')
  return line || null
}

function usePanes(): (id: string) => number {
  const workspaces = useApp((s) => s.config.workspaces)
  return (id) =>
    workspaces.reduce((n, w) => n + w.panes.filter((p) => p.accountId === id).length, 0)
}

function Provider({ cli }: { cli: AgentCli }) {
  return (
    <span className="acc-prov">
      <span className="acc-plogo">
        <ProviderLogo cli={cli} size={12} />
      </span>
      {AGENT_CLI_SPECS[cli].label}
    </span>
  )
}

function Status({ account, compact = false }: { account: Account; compact?: boolean }) {
  const actions = useActions()
  if (account.signedIn)
    return (
      <span className="acc-st">
        <i aria-hidden="true" />
        Signed in
      </span>
    )
  return (
    <span className="acc-st off">
      <IconWarn small />
      Not signed in
      {compact ? null : (
        <button
          className="btn sm"
          onClick={(e) => {
            e.stopPropagation()
            actions.loginAccount(account.id)
          }}
        >
          Sign in
        </button>
      )}
    </span>
  )
}

function rowMenu(
  account: Account,
  folders: AccountFolder[],
  actions: ReturnType<typeof useActions>,
  reveal: () => void,
  details: () => void
): MenuItem[] {
  const moves: MenuItem[] = folders
    .filter((f) => f.id !== account.folderId)
    .map((f, i) => ({
      label: f.name,
      icon: <IconFolder small />,
      heading: i === 0 ? 'Move to' : undefined,
      divider: i === 0,
      onSelect: () => actions.moveAccountToFolder(account.id, f.id)
    }))
  if (account.folderId)
    moves.push({
      label: 'No folder',
      icon: <IconFolder small />,
      heading: moves.length ? undefined : 'Move to',
      divider: !moves.length,
      onSelect: () => actions.moveAccountToFolder(account.id, null)
    })
  return [
    {
      label: 'Open a pane',
      icon: <IconTerminal small />,
      onSelect: () => actions.openModal({ kind: 'newPane', slotId: null, accountId: account.id })
    },
    {
      label: account.signedIn ? 'Sign in again' : 'Sign in',
      icon: <IconLogin small />,
      onSelect: () => actions.loginAccount(account.id)
    },
    { label: 'Rename', icon: <IconRename small />, onSelect: details },
    {
      label: 'Show config folder',
      icon: <IconFolder small />,
      onSelect: reveal
    },
    ...moves,
    {
      label: 'Remove…',
      icon: <IconTrash small />,
      danger: true,
      divider: true,
      onSelect: () => actions.openModal({ kind: 'removeAccount', accountId: account.id })
    }
  ]
}

function AccountRow({
  account,
  panes,
  selected,
  fresh,
  onSelect,
  onRename
}: {
  account: Account
  panes: number
  selected: boolean
  fresh: boolean
  onSelect(): void
  onRename(): void
}) {
  const { api } = useServices()
  const actions = useActions()
  const folders = useApp((s) => s.config.accountFolders)
  const line = useLoginLine(account.id)
  const [menu, setMenu] = useState<MenuPoint | null>(null)
  const { setNodeRef, listeners, attributes, isDragging, transform } = useDraggable({
    id: rowDragId(account.id)
  })
  const style = transform
    ? { transform: `translate(${transform.x}px, ${transform.y}px)`, zIndex: 5 }
    : undefined

  return (
    <div
      ref={setNodeRef}
      {...attributes}
      role="row"
      aria-selected={selected}
      className={`acc-tr${selected ? ' sel' : ''}${fresh ? ' fresh' : ''}${isDragging ? ' dragging' : ''}`}
      style={style}
      onClick={onSelect}
      onContextMenu={(e) => {
        e.preventDefault()
        setMenu(menuPoint(e))
      }}
      {...listeners}
      aria-roledescription="draggable account"
      tabIndex={0}
      onKeyDown={(e) => {
        if (e.key === 'Enter' && e.target === e.currentTarget) onSelect()
      }}
    >
      <span className="acc-nm" role="cell">
        <Initial name={account.name} size={26} />
        <span className="acc-nm-tx">
          <span className="n">{account.name}</span>
          {line ? <span className="e">{line}</span> : null}
        </span>
      </span>
      <span role="cell">
        <Provider cli={account.cli} />
      </span>
      <span role="cell">
        <Status account={account} />
      </span>
      <span className="acc-num" role="cell">
        {panes}
      </span>
      <span className="acc-acts" role="cell">
        {fresh ? (
          <button
            className="btn sm"
            onClick={(e) => {
              e.stopPropagation()
              actions.openModal({ kind: 'newPane', slotId: null, accountId: account.id })
            }}
          >
            <IconTerminal small />
            Open a pane
          </button>
        ) : null}
        <button
          className="icon-btn"
          aria-label={`Actions for ${account.name}`}
          title="Actions"
          aria-haspopup="menu"
          onPointerDown={(e) => e.stopPropagation()}
          onClick={(e) => {
            e.stopPropagation()
            const r = e.currentTarget.getBoundingClientRect()
            setMenu({ x: r.right - 200, y: r.bottom + 4 })
          }}
        >
          <IconMore />
        </button>
      </span>
      {menu ? (
        <ContextMenu
          at={menu}
          width={200}
          label={`Account ${account.name}`}
          onClose={() => setMenu(null)}
          items={rowMenu(
            account,
            folders,
            actions,
            () => void api.fs.reveal(account.configDir),
            onRename
          )}
        />
      ) : null}
    </div>
  )
}

/** One folder's heading (a drop target) and its rows; `folder` null = accounts in no folder. */
function Group({
  folder,
  count,
  showHead,
  children
}: {
  folder: AccountFolder | null
  count: number
  showHead: boolean
  children: ReactNode
}) {
  const actions = useActions()
  const folderCount = useApp((s) => s.config.accountFolders.length)
  const { setNodeRef, isOver } = useDroppable({ id: folder ? groupDropId(folder.id) : ROOT })
  const [renaming, setRenaming] = useState(false)
  const [name, setName] = useState(folder?.name ?? '')
  const [menu, setMenu] = useState<MenuPoint | null>(null)
  const save = (): void => {
    if (folder) actions.renameFolder(folder.id, name)
    setRenaming(false)
  }
  const startRename = (): void => {
    setName(folder?.name ?? '')
    setRenaming(true)
  }
  const collapsed = !!folder?.collapsed

  return (
    <div
      ref={setNodeRef}
      className={`acc-grp${isOver ? ' over' : ''}`}
      role="rowgroup"
      aria-label={folder ? `Folder ${folder.name}` : 'No folder'}
    >
      {showHead ? (
        <div
          className="acc-grp-hd"
          onContextMenu={(e) => {
            if (!folder) return
            e.preventDefault()
            setMenu(menuPoint(e))
          }}
        >
          {folder ? (
            <button
              className="acc-grp-tg"
              aria-expanded={!collapsed}
              aria-label={`${collapsed ? 'Expand' : 'Collapse'} ${folder.name}`}
              onClick={() => actions.toggleFolder(folder.id)}
            >
              <span className={`chev${collapsed ? '' : ' open'}`} aria-hidden="true">
                ›
              </span>
            </button>
          ) : null}
          {renaming ? (
            <input
              className="inpt sans acc-grp-in"
              value={name}
              maxLength={64}
              aria-label="Folder name"
              autoFocus
              onChange={(e) => setName(e.target.value)}
              onBlur={save}
              onKeyDown={(e) => {
                if (e.key === 'Enter') save()
                if (e.key === 'Escape') {
                  e.stopPropagation()
                  setRenaming(false)
                }
              }}
            />
          ) : (
            <span
              className="acc-grp-nm"
              title={folder ? 'Double-click or right-click to rename' : undefined}
              onDoubleClick={folder ? startRename : undefined}
            >
              {folder ? folder.name : 'No folder'}
            </span>
          )}
          <span className="acc-grp-n">· {count}</span>
          {folder ? (
            <button
              className="icon-btn sm acc-grp-rm"
              aria-label={`Delete folder ${folder.name} (its accounts stay)`}
              title="Delete folder (its accounts stay)"
              onClick={() => actions.deleteFolder(folder.id)}
            >
              <IconClose small />
            </button>
          ) : null}
        </div>
      ) : null}
      {menu && folder ? (
        <ContextMenu
          at={menu}
          label={`Folder ${folder.name}`}
          onClose={() => setMenu(null)}
          items={[
            { label: 'Rename', icon: <IconRename small />, onSelect: startRename },
            {
              label: collapsed ? 'Expand' : 'Collapse',
              icon: <IconFolder small />,
              onSelect: () => actions.toggleFolder(folder.id)
            },
            {
              label: 'New folder',
              icon: <IconPlus small />,
              disabled: folderCount >= 32,
              onSelect: () => void actions.createFolder(`Folder ${folderCount + 1}`)
            },
            {
              label: 'Delete folder (accounts stay)',
              icon: <IconTrash small />,
              danger: true,
              onSelect: () => actions.deleteFolder(folder.id)
            }
          ]}
        />
      ) : null}
      {collapsed ? null : children}
      {!collapsed && count === 0 && showHead ? (
        <div className="acc-grp-empty">
          {folder ? 'Drop accounts here.' : 'Drop here to take an account out of its folder.'}
        </div>
      ) : null}
    </div>
  )
}

function Details({
  account,
  panes,
  nameRef,
  onClose
}: {
  account: Account
  panes: number
  nameRef: React.RefObject<HTMLInputElement | null>
  onClose(): void
}) {
  const { api } = useServices()
  const actions = useActions()
  const folders = useApp((s) => s.config.accountFolders)
  const accounts = useApp((s) => s.config.accounts)
  const info = useApp((s) => s.accountInfo[account.id]?.info)
  const sharedNote = useApp((s) =>
    s.config.settings.sharedMode === 'overall' ? (SHARED_NOTE[account.cli] ?? null) : null
  )
  const [name, setName] = useState(account.name)
  const [error, setError] = useState<string | null>(null)

  const saveName = (): void => {
    const t = name.trim()
    if (!t || t === account.name) return setName(account.name)
    if (accounts.some((a) => a.id !== account.id && a.name.toLowerCase() === t.toLowerCase()))
      return setError('An account with that name already exists.')
    actions.updateAccount(account.id, { name: t })
  }

  return (
    <aside className="acc-det" aria-label={`Account ${account.name}`}>
      <div className="acc-det-hd">
        <Initial name={account.name} size={30} />
        <span className="acc-det-tt">
          <h2>{account.name}</h2>
          <Provider cli={account.cli} />
        </span>
        <button className="icon-btn" aria-label="Close details" onClick={onClose}>
          <IconClose />
        </button>
      </div>
      <dl className="acc-kv">
        {info?.email ? (
          <>
            <dt>Signed in as</dt>
            <dd>{info.email}</dd>
          </>
        ) : null}
        {info?.plan ? (
          <>
            <dt>Plan</dt>
            <dd>{info.plan}</dd>
          </>
        ) : null}
        <dt>Status</dt>
        <dd>
          <Status account={account} compact />
        </dd>
        <dt>Open panes</dt>
        <dd>{panes}</dd>
      </dl>
      <div className="acc-det-acts">
        <button
          className="btn sm"
          onClick={() =>
            actions.openModal({ kind: 'newPane', slotId: null, accountId: account.id })
          }
        >
          <IconTerminal small />
          Open a pane
        </button>
        <button
          className={`btn sm${account.signedIn ? '' : ' pri'}`}
          onClick={() => actions.loginAccount(account.id)}
        >
          <IconLogin small />
          {account.signedIn ? 'Sign in again' : 'Sign in'}
        </button>
      </div>
      <div className="acc-sep" />
      <div className="fld">
        <label className="lbl" htmlFor="acc-det-name">
          Name
        </label>
        <input
          id="acc-det-name"
          ref={nameRef}
          className="inpt sans"
          value={name}
          maxLength={64}
          onChange={(e) => {
            setName(e.target.value)
            setError(null)
          }}
          onBlur={saveName}
          onKeyDown={(e) => {
            if (e.key === 'Enter') saveName()
            if (e.key === 'Escape') {
              e.stopPropagation()
              setName(account.name)
              setError(null)
            }
          }}
        />
        {error ? <div className="hint err">{error}</div> : null}
      </div>
      <div className="fld">
        <span className="lbl" id="acc-det-color">
          Color
        </span>
        <div className="colors" role="radiogroup" aria-labelledby="acc-det-color">
          {ACCOUNT_COLORS.map((c) => {
            const on = c.value.toLowerCase() === account.color.toLowerCase()
            return (
              <button
                type="button"
                key={c.value}
                className={`cpick${on ? ' on' : ''}`}
                role="radio"
                aria-checked={on}
                aria-label={c.name}
                onClick={() => actions.updateAccount(account.id, { color: c.value })}
              >
                <span className="c" style={{ background: c.value }}>
                  {on ? <IconCheck style={{ strokeWidth: 2.2 }} /> : null}
                </span>
              </button>
            )
          })}
        </div>
      </div>
      {folders.length ? (
        <div className="fld">
          <label className="lbl" htmlFor="acc-det-folder">
            Folder
          </label>
          <select
            id="acc-det-folder"
            className="inpt sans"
            value={account.folderId ?? ''}
            onChange={(e) => actions.moveAccountToFolder(account.id, e.target.value || null)}
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
      <div className="acc-sep" />
      <span className="lbl">Config folder</span>
      <div className="acc-dir">
        <span className="mono" title={account.configDir}>
          {account.configDir}
        </span>
        <CopyButton text={account.configDir} label="Copy path" />
        <button
          className="icon-btn sm"
          aria-label="Show in file manager"
          title="Show in file manager"
          onClick={() => void api.fs.reveal(account.configDir)}
        >
          <IconFolder small />
        </button>
      </div>
      {account.imported ? <div className="hint">An existing folder, used where it is.</div> : null}
      {sharedNote ? <div className="hint">{sharedNote}</div> : null}
      <div className="acc-sep" />
      <button
        className="btn sm acc-rm"
        onClick={() => actions.openModal({ kind: 'removeAccount', accountId: account.id })}
      >
        <IconTrash small />
        Remove account…
      </button>
    </aside>
  )
}

export function AccountsView() {
  const actions = useActions()
  const mode = useApp((s) => s.accountsMode)
  const accounts = useApp((s) => s.config.accounts)
  const folders = useApp((s) => s.config.accountFolders)
  const plans = useApp((s) => s.accountInfo)
  const panes = usePanes()
  const [query, setQuery] = useState('')
  const [provider, setProvider] = useState<AgentCli | null>(null)
  const [selected, setSelected] = useState<string | null>(null)
  const [fresh, setFresh] = useState<string | null>(null)
  const nameRef = useRef<HTMLInputElement | null>(null)
  const sensors = useSensors(useSensor(PointerSensor, { activationConstraint: { distance: 5 } }))

  // The row just added stays marked (with "Open a pane") for a few seconds.
  useEffect(() => {
    if (!fresh) return
    const t = setTimeout(() => setFresh(null), 6000)
    return () => clearTimeout(t)
  }, [fresh])

  const providers = AGENT_CLIS.filter((c) => accounts.some((a) => a.cli === c))
  const activeProvider = provider && providers.includes(provider) ? provider : null
  const q = query.trim().toLowerCase()
  const shown = accounts.filter(
    (a) =>
      (!activeProvider || a.cli === activeProvider) &&
      (!q ||
        a.name.toLowerCase().includes(q) ||
        AGENT_CLI_SPECS[a.cli].label.toLowerCase().includes(q) ||
        (plans[a.id]?.info?.email ?? '').toLowerCase().includes(q))
  )
  const filtering = !!q || !!activeProvider
  const current = accounts.find((a) => a.id === selected) ?? null
  const rename = (id: string): void => {
    setSelected(id)
    setTimeout(() => nameRef.current?.select(), 0)
  }

  const onDragEnd = (e: DragEndEvent): void => {
    const active = String(e.active.id)
    const over = e.over ? String(e.over.id) : null
    if (!over || !active.startsWith('acc:')) return
    actions.moveAccountToFolder(active.slice(4), over === ROOT ? null : over.slice(4))
  }

  const rows = (list: Account[]): ReactNode =>
    list.map((a) => (
      <AccountRow
        key={a.id}
        account={a}
        panes={panes(a.id)}
        selected={a.id === selected}
        fresh={a.id === fresh}
        onSelect={() => setSelected(a.id === selected ? null : a.id)}
        onRename={() => rename(a.id)}
      />
    ))
  const loose = shown.filter((a) => !a.folderId || !folders.some((f) => f.id === a.folderId))

  return (
    <div className="page">
      <div className={`page-in acc-page${current ? ' with-det' : ''}`}>
        <div className="acc-main">
          <div className="pg-hd">
            <div>
              <h1>Accounts</h1>
              <p>
                Logins Wraithgrid can run chats under. Add as many as you like, from any provider.
              </p>
            </div>
            <div className="acts">
              <button className="btn pri" onClick={() => actions.setAccountsMode('add')}>
                <IconPlus />
                Add account
              </button>
            </div>
          </div>

          {accounts.length === 0 && folders.length === 0 ? (
            <div className="emptyc">
              <h2>No accounts yet</h2>
              <p>
                Add your Claude Code, Codex, Gemini or other CLI login to start a chat. If you
                already use one in the terminal, you can reuse that login.
              </p>
              <button className="btn pri" onClick={() => actions.setAccountsMode('add')}>
                <IconPlus />
                Add account
              </button>
            </div>
          ) : (
            <>
              <div className="acc-bar">
                <label className="acc-search">
                  <IconSearch small />
                  <input
                    className="inpt sans"
                    value={query}
                    placeholder="Search accounts"
                    aria-label="Search accounts"
                    onChange={(e) => setQuery(e.target.value)}
                  />
                </label>
                {providers.length > 1 ? (
                  <div className="seg" role="radiogroup" aria-label="Provider filter">
                    {[null, ...providers].map((c) => {
                      const n = c ? accounts.filter((a) => a.cli === c).length : accounts.length
                      return (
                        <button
                          key={c ?? 'all'}
                          type="button"
                          role="radio"
                          aria-checked={activeProvider === c}
                          className={`seg-btn${activeProvider === c ? ' on' : ''}`}
                          onClick={() => setProvider(c)}
                        >
                          {c ? AGENT_CLI_SPECS[c].label : 'All'}{' '}
                          <span className="acc-cnt">{n}</span>
                        </button>
                      )
                    })}
                  </div>
                ) : null}
                <button
                  className="btn gh acc-newf"
                  onClick={() => actions.createFolder(`Folder ${folders.length + 1}`)}
                  disabled={folders.length >= 32}
                >
                  <IconFolder small />
                  New folder
                </button>
              </div>

              <div className="acc-tbl" role="table" aria-label="Accounts">
                <div className="acc-tr acc-hd" role="row">
                  <span role="columnheader">Account</span>
                  <span role="columnheader">Provider</span>
                  <span role="columnheader">Status</span>
                  <span role="columnheader">Open panes</span>
                  <span role="columnheader" aria-label="Actions" />
                </div>
                <DndContext
                  sensors={sensors}
                  collisionDetection={pointerThenRect}
                  onDragEnd={onDragEnd}
                >
                  {folders.map((f) => {
                    const list = shown.filter((a) => a.folderId === f.id)
                    if (filtering && list.length === 0) return null
                    return (
                      <Group key={f.id} folder={f} count={list.length} showHead>
                        {rows(list)}
                      </Group>
                    )
                  })}
                  {folders.length === 0 || loose.length > 0 || !filtering ? (
                    <Group folder={null} count={loose.length} showHead={folders.length > 0}>
                      {rows(loose)}
                    </Group>
                  ) : null}
                </DndContext>
                {filtering && shown.length === 0 ? (
                  <div className="acc-grp-empty">No accounts match.</div>
                ) : null}
              </div>
              <p className="hint acc-foot">
                {folders.length
                  ? 'Drag a row onto a folder heading to move it.'
                  : 'Use New folder to group accounts, e.g. Personal and Work.'}
              </p>
            </>
          )}
        </div>
        {current ? (
          <Details
            key={`${current.id}:${current.name}`}
            account={current}
            panes={panes(current.id)}
            nameRef={nameRef}
            onClose={() => setSelected(null)}
          />
        ) : null}
      </div>
      {mode !== 'list' ? (
        <AddAccountDialog
          startAt={mode}
          onAdded={(id) => {
            setFresh(id)
            setSelected(null)
          }}
        />
      ) : null}
    </div>
  )
}

export function RemoveAccountDialog({ accountId }: { accountId: string }) {
  const actions = useActions()
  const account = useApp((s) => s.config.accounts.find((a) => a.id === accountId))
  const workspaces = useApp((s) => s.config.workspaces)
  const [deleteDir, setDeleteDir] = useState(false)
  const [error, setError] = useState<string | null>(null)
  const [busy, setBusy] = useState(false)
  if (!account) return null

  const uses = workspaces.reduce(
    (n, w) => n + w.panes.filter((p) => p.accountId === accountId).length,
    0
  )
  const panesMsg = uses
    ? `${uses} ${uses > 1 ? 'panes use' : 'pane uses'} this account and will close.`
    : 'No panes use this account.'

  const confirm = async (): Promise<void> => {
    setBusy(true)
    const err = await actions.removeAccount(accountId, deleteDir)
    setBusy(false)
    if (err) setError(err)
    else actions.closeModal()
  }

  return (
    <Dialog onClose={actions.closeModal} className="w4" role="alertdialog" labelledBy="rm-t">
      <div className="dlg-hd">
        <h2 id="rm-t">Remove account “{account.name}”?</h2>
      </div>
      <div className="dlg-bd" style={{ gap: 14 }}>
        <p className="hint" style={{ fontSize: 13, color: 'var(--mu)', margin: 0 }}>
          {panesMsg} The account is removed from Wraithgrid.
        </p>
        <div className="trow">
          <span className="tx">
            <span style={{ fontWeight: 600 }}>Also delete its config dir</span>
            <span className="hint mono">{account.configDir}</span>
            <span className="hint">
              Deletes the login, settings and history. This cannot be undone.
            </span>
          </span>
          <button
            className={`sw${deleteDir ? ' on' : ''}`}
            role="switch"
            aria-checked={deleteDir}
            aria-label="Also delete config dir"
            onClick={() => setDeleteDir((v) => !v)}
          />
        </div>
        {error ? <div className="hint err">{error}</div> : null}
      </div>
      <div className="dlg-ft">
        <button className="btn gh" onClick={actions.closeModal} data-autofocus>
          Cancel
        </button>
        <button className="btn dng" onClick={() => void confirm()} disabled={busy}>
          {deleteDir ? 'Remove and delete dir' : 'Remove account'}
        </button>
      </div>
    </Dialog>
  )
}
