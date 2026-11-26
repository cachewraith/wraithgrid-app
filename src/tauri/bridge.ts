// window.wraith for the Tauri app: the preload's API (src/preload/index.ts) over Tauri's
// invoke/listen instead of Electron's ipcRenderer. Every channel goes to the Rust `ipc`
// command (app-rs/crates/desktop); events come back under their channel names.
import { invoke } from '@tauri-apps/api/core'
import { listen } from '@tauri-apps/api/event'
import { getCurrentWebview } from '@tauri-apps/api/webview'
import { IPC } from '@shared/ipc-channels'
import type {
  CliAction,
  CloudStatus,
  LoginEvent,
  PtyDataEvent,
  PreviewEventMessage,
  RelayEvent,
  PtyExitEvent,
  UpdateProgress,
  UsageLimitsEvent,
  WraithApi
} from '@shared/ipc-contract'
import type { ChatEventMessage } from '@shared/chat'
import type { PreviewRequestFailure } from '@shared/preview'
import type { CloseMode } from '@shared/types'

// Saved config must be in place before whatever the renderer asks next (e.g. preview:start
// reads the command it just saved): these go through a command that runs in order.
const ORDERED = new Set<string>([IPC.configSet])

function call<T = unknown>(channel: string, payload?: unknown): Promise<T> {
  const command = ORDERED.has(channel) ? 'ipc_ordered' : 'ipc'
  const run = (): Promise<T> => invoke<T>(command, { channel, payload: payload ?? null })
  const key = paneKey(channel, payload)
  return key ? inOrder(key, run) : run()
}

function send(channel: string, payload?: unknown): void {
  call(channel, payload).catch((e: unknown) => console.error(`${channel}:`, e))
}

function subscribe<T>(channel: string, cb: (payload: T) => void): () => void {
  let stop: (() => void) | null = null
  let gone = false
  void listen<T>(channel, (e) => cb(e.payload)).then((unlisten) => {
    if (gone) unlisten()
    else stop = unlisten
  })
  return () => {
    gone = true
    stop?.()
  }
}

// Each invoke runs as its own task in Rust, so calls could overtake each other (a resize
// before its pane exists, keys out of order). Electron handled IPC in order: a pane's pty and
// chat calls run one after another here, and keys typed while one waits go as one write.
const chains = new Map<string, Promise<unknown>>()
function inOrder<T>(key: string, run: () => Promise<T>): Promise<T> {
  const next = (chains.get(key) ?? Promise.resolve()).then(run, run)
  chains.set(key, next)
  const done = (): void => {
    if (chains.get(key) === next) chains.delete(key)
  }
  next.then(done, done)
  return next
}

function paneKey(channel: string, payload: unknown): string | null {
  const group = channel.split(':')[0]
  if (group !== 'pty' && group !== 'chat') return null
  const paneId = (payload as { paneId?: unknown } | null)?.paneId
  return typeof paneId === 'string' ? `${group}:${paneId}` : null
}

const pendingWrites = new Map<string, string[]>()
function writeInOrder(paneId: string, data: string): void {
  const pending = pendingWrites.get(paneId)
  if (pending) {
    pending.push(data)
    return
  }
  const chunks = [data]
  pendingWrites.set(paneId, chunks)
  send(IPC.ptyWrite, {
    paneId,
    get data() {
      pendingWrites.delete(paneId)
      return chunks.join('')
    }
  })
}

// Renderer errors land in the app's log (there are no DevTools in a release build).
window.addEventListener('error', (e) =>
  send('debug:log', { message: `${e.message} at ${e.filename}:${e.lineno}`, stack: e.error?.stack })
)
window.addEventListener('unhandledrejection', (e) =>
  send('debug:log', { message: `unhandled: ${String(e.reason)}`, stack: e.reason?.stack })
)

// Paths of files dropped on the window, as Tauri reports them (a webview File has no path).
const dropped: string[] = []
void getCurrentWebview().onDragDropEvent((e) => {
  if (e.payload.type === 'drop') dropped.splice(0, dropped.length, ...e.payload.paths)
})
function droppedPath(file: File): string {
  return dropped.find((p) => p.split(/[\\/]/).pop() === file.name) ?? ''
}

const api: WraithApi = {
  pty: {
    create: (args) => call(IPC.ptyCreate, args),
    write: (paneId, data) => writeInOrder(paneId, data),
    resize: (paneId, cols, rows) => send(IPC.ptyResize, { paneId, cols, rows }),
    kill: (paneId) => send(IPC.ptyKill, { paneId }),
    onData: (cb) => subscribe<PtyDataEvent>(IPC.ptyData, cb),
    onExit: (cb) => subscribe<PtyExitEvent>(IPC.ptyExit, cb)
  },
  chat: {
    start: (args) => call(IPC.chatStart, args),
    moveAccount: (paneId, toAccountId, sessionId) =>
      call(IPC.chatMoveAccount, { paneId, toAccountId, sessionId }),
    send: (paneId, text, images) => send(IPC.chatSend, { paneId, text, images }),
    approve: (paneId, reqId, decision, answers) =>
      send(IPC.chatApprove, { paneId, reqId, decision, answers }),
    interrupt: (paneId) => send(IPC.chatInterrupt, { paneId }),
    setModel: (paneId, model) => send(IPC.chatSetModel, { paneId, model }),
    setMode: (paneId, mode) => send(IPC.chatSetMode, { paneId, mode }),
    stop: (paneId, forget) => send(IPC.chatStop, { paneId, forget }),
    onEvent: (cb) => subscribe<ChatEventMessage>(IPC.chatEvent, cb),
    load: (paneId) => call(IPC.chatLoad, { paneId }),
    save: (paneId, data) => send(IPC.chatSave, { paneId, data }),
    onFlush: (cb) => subscribe<void>(IPC.chatFlush, () => cb()),
    flushed: () => send(IPC.chatFlushed),
    pickFiles: (paneId) => call(IPC.chatPickFiles, { paneId }),
    listFiles: (paneId, query) => call(IPC.chatListFiles, { paneId, query }),
    filePath: (file) => droppedPath(file)
  },
  config: {
    get: () => call(IPC.configGet),
    set: (config) => call(IPC.configSet, config)
  },
  dialog: {
    pickFolder: (defaultPath) => call(IPC.dialogPickFolder, { defaultPath }),
    pickFile: (defaultPath) => call(IPC.dialogPickFile, { defaultPath })
  },
  fs: {
    listFolder: (path) => call(IPC.fsListFolder, { path }),
    reveal: (path) => call(IPC.fsReveal, { path })
  },
  window: {
    minimize: () => send(IPC.windowMinimize),
    maximize: () => send(IPC.windowMaximize),
    close: () => send(IPC.windowClose),
    onCloseMode: (cb) => subscribe<CloseMode>(IPC.windowCloseMode, cb)
  },
  claude: {
    detect: (override) => call(IPC.claudeDetect, { override })
  },
  account: {
    createDir: (slug) => call(IPC.accountCreateDir, { slug }),
    deleteDir: (dir) => call(IPC.accountDeleteDir, { dir, confirm: true })
  },
  shell: {
    openExternal: (url) => call(IPC.shellOpenExternal, { url })
  },
  login: {
    start: (accountId) => call(IPC.loginStart, { accountId }),
    code: (accountId, code) => send(IPC.loginCode, { accountId, code }),
    cancel: (accountId) => send(IPC.loginCancel, { accountId }),
    onEvent: (cb) => subscribe<LoginEvent>(IPC.loginEvent, cb)
  },
  shared: {
    apply: (mode) => call(IPC.sharedApply, { mode })
  },
  app: {
    info: () => call(IPC.appInfo),
    setZoom: (factor) => {
      if (Number.isFinite(factor))
        void getCurrentWebview().setZoom(Math.min(2, Math.max(0.67, factor)))
    }
  },
  update: {
    check: (reason) => call(IPC.updateCheck, { reason }),
    download: () => call(IPC.updateDownload, {}),
    restart: () => call(IPC.updateRestart, {}),
    cancel: () => call(IPC.updateCancel, {}),
    notes: () => call(IPC.updateNotes, {}),
    onProgress: (cb) => subscribe<UpdateProgress>(IPC.updateProgress, cb),
    onShow: (cb) => subscribe<void>(IPC.updateShow, () => cb())
  },
  project: {
    suggestIcons: (wsId) => call(IPC.projectSuggestIcons, { wsId })
  },
  git: {
    status: (paneId) => call(IPC.gitStatus, { paneId }),
    diff: (paneId) => call(IPC.gitDiff, { paneId }),
    addWorktree: (cwd, branch, reuse = false, from = 'new') =>
      call(IPC.gitWorktreeAdd, { cwd, branch, reuse, from }),
    branches: (paneId) => call(IPC.gitBranches, { paneId }),
    switchBranch: (paneId, kind, name) => call(IPC.gitSwitch, { paneId, kind, name }),
    commit: (paneId, message) => call(IPC.gitCommit, { paneId, message }),
    push: (paneId) => call(IPC.gitPush, { paneId }),
    pull: (paneId) => call(IPC.gitPull, { paneId }),
    log: (paneId, skip) => call(IPC.gitLog, { paneId, skip }),
    show: (paneId, sha) => call(IPC.gitShow, { paneId, sha }),
    openPullRequest: (paneId) => call(IPC.gitOpenPr, { paneId }),
    checkWorktree: (cwd) => call(IPC.gitWorktreeCheck, { cwd }),
    removeWorktree: (cwd, force) => call(IPC.gitWorktreeRemove, { cwd, force }),
    clone: (url, parent, branch) => call(IPC.gitClone, { url, parent, branch })
  },
  links: {
    take: () => call(IPC.appTakeLink, {}),
    onLink: (cb) => subscribe<void>(IPC.appLink, () => cb())
  },
  shells: {
    list: () => call(IPC.shellList, {})
  },
  clipboard: {
    image: (paneId) => call(IPC.clipboardImage, { paneId })
  },
  usage: {
    get: () => call(IPC.usageGet, {}),
    onLimits: (cb) => subscribe<UsageLimitsEvent>(IPC.usageLimits, cb)
  },
  themes: {
    list: () => call(IPC.themesList, {}),
    import: () => call(IPC.themesImport, {}),
    delete: (id) => call(IPC.themesDelete, { id }),
    export: (theme) => call(IPC.themesExport, { theme }),
    save: (theme) => call(IPC.themesSave, { theme }),
    community: () => call(IPC.themesCommunity, {}),
    communityShot: (repo) => call(IPC.themesCommunityShot, { repo }),
    communityInstall: (repo) => call(IPC.themesCommunityInstall, { repo }),
    communityRemove: (id) => call(IPC.themesCommunityRemove, { id }),
    openFolder: () => call(IPC.themesOpenFolder, {})
  },
  mods: {
    list: () => call(IPC.modsList, {}),
    setEnabled: (accountId, mod, enabled) =>
      call(IPC.modsSetEnabled, {
        accountId,
        id: mod.id,
        scope: mod.scope,
        projectPath: mod.projectPath,
        enabled
      }),
    catalog: () => call(IPC.modsCatalog, {}),
    community: () => call(IPC.modsCommunity, {}),
    install: (id) => call(IPC.modsInstall, { id }),
    change: (id, change) => call(IPC.modsChange, { id, change }),
    clone: (id) => call(IPC.modsClone, { id }),
    pull: (name) => call(IPC.modsPull, { name }),
    marketplaces: () => call(IPC.modsMarketplaces, {}),
    addMarketplace: (source) => call(IPC.modsMarketplaceAdd, { source }),
    removeMarketplace: (name) => call(IPC.modsMarketplaceRemove, { name })
  },
  preview: {
    detect: (paneId) => call(IPC.previewDetect, { paneId }),
    start: (paneId) => call(IPC.previewStart, { paneId }),
    stop: (paneId) => send(IPC.previewStop, { paneId }),
    onEvent: (cb) => subscribe<PreviewEventMessage>(IPC.previewEvent, cb),
    capture: (webContentsId) => call(IPC.previewCapture, { webContentsId }),
    onRequestFailed: (cb) => subscribe<PreviewRequestFailure>(IPC.previewRequestFailed, cb)
  },
  notify: {
    pane: (paneId, title, body) => send(IPC.notifyPane, { paneId, title, body }),
    onReveal: (cb) => subscribe<string>(IPC.paneReveal, cb)
  },
  machines: {
    sshHosts: () => call(IPC.machineSshHosts),
    test: (name, sshHost) => call(IPC.machineTest, { name, sshHost })
  },
  cli: {
    onAction: (cb) =>
      subscribe<{ id: number; action: CliAction }>(IPC.cliAction, (m) => cb(m.id, m.action)),
    reply: (id, result) => send(IPC.cliResult, { id, result })
  },
  cloud: {
    status: () => call(IPC.cloudStatus, {}),
    config: () => call(IPC.cloudConfig, {}),
    signIn: (provider) => call(IPC.cloudSignIn, { provider }),
    emailSignIn: (email, password) => call(IPC.cloudEmailSignIn, { email, password }),
    emailSignUp: (name, email, password) => call(IPC.cloudEmailSignUp, { name, email, password }),
    emailVerify: (email, otp) => call(IPC.cloudEmailVerify, { email, otp }),
    emailResend: (email) => call(IPC.cloudEmailResend, { email }),
    emailReset: (email) => call(IPC.cloudEmailReset, { email }),
    emailResetCheck: (email, otp) => call(IPC.cloudEmailResetCheck, { email, otp }),
    emailResetConfirm: (email, otp, password) =>
      call(IPC.cloudEmailResetConfirm, { email, otp, password }),
    updateName: (name) => call(IPC.cloudUpdateName, { name }),
    changePassword: (current, next) => call(IPC.cloudChangePassword, { current, next }),
    cancel: () => call(IPC.cloudCancel, {}),
    signOut: () => call(IPC.cloudSignOut, {}),
    refresh: () => call(IPC.cloudRefresh, {}),
    onChanged: (cb) => subscribe<CloudStatus>(IPC.cloudChanged, cb),
    relayPair: () => call(IPC.cloudRelayPair, {}),
    relayPairings: () => call(IPC.cloudRelayPairings, {}),
    relayUnpair: (id) => call(IPC.cloudRelayUnpair, { id }),
    relayConnect: (gen) => call(IPC.relayConnect, { gen }),
    relaySend: (gen, text) => call(IPC.relaySend, { gen, text }),
    relayClose: (gen) => call(IPC.relayClose, { gen }),
    onRelay: (cb) => subscribe<RelayEvent>(IPC.relayEvent, cb),
    relayLoad: () => call(IPC.cloudRelayLoad, {}),
    relaySave: (data) => call(IPC.cloudRelaySave, { data }),
    relayRequestKey: (id, desktopKey) => call(IPC.cloudRelayRequestKey, { id, desktopKey }),
    relayRequestAnswer: (id, allow) => call(IPC.cloudRelayRequestAnswer, { id, allow })
  }
}

window.wraith = api
