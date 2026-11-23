// The desktop end of the relay (the web app at app.wraithgrid.cachewraith.com, or a phone,
// chats with this app through wraithgrid-server; spec: wraithgrid-protocol docs/RELAY-APP.md).
// Runs in the renderer because the chats live here (store + ChatBus). Main only signs the
// server calls (`cloud:relay*`) and keeps relay.json. Every frame is sealed end to end
// (src/shared/relay): the server sees who is online and how much, never what.
//
// A paired browser is trusted only after its first sealed message, `pair.confirm`, carries the
// one-time code from our pairing link and the key it claimed with. Nothing it sends acts on its own:
// it can read chats, send a message to a chat, start one in a project, or stop a turn — as
// typing in this window would — but not approve tool calls.
import type { StoreApi } from 'zustand'
import type { CloudStatus, RelayEvent, SimpleResult } from '@shared/ipc-contract'
import { channel, newKeyPair, type Channel } from '@shared/relay/crypto'
import {
  Counter,
  decode,
  encode,
  pairingLinkUrl,
  ToDesktop,
  type ToWeb
} from '@shared/relay/messages'
import { findPane, paneView, type AppState } from '../../app/store'
import type { ChatBus } from '../chat-bus'
import { projectFolder } from '../team'
import { markSent, newChatAccount, snapshot, state, updates } from './wire'

/** What main does for the relay (`cloud:relay*`). */
export interface RelayApi {
  relayPair(): Promise<
    | {
        ok: true
        pairing: { id: string; deviceName: string; expiresAt: string }
        code: string
        server: string
        appUrl: string
      }
    | { ok: false; error: string; code?: string }
  >
  relayPairings(): Promise<
    | {
        ok: true
        pairings: {
          id: string
          status: string
          phoneName: string | null
          phoneKey: string | null
        }[]
      }
    | { ok: false; error: string }
  >
  relayUnpair(id: string): Promise<SimpleResult>
  relayConnect(gen: number): Promise<SimpleResult>
  relaySend(gen: number, text: string): Promise<SimpleResult>
  relayClose(gen: number): Promise<SimpleResult>
  onRelay(cb: (e: RelayEvent) => void): () => void
  relayLoad(): Promise<unknown>
  relaySave(data: unknown): Promise<SimpleResult>
  status(): Promise<CloudStatus>
  onChanged(cb: (s: CloudStatus) => void): () => void
}

interface SavedPairing {
  secretKey: string
  publicKey: string
  deviceName: string
  /** Until confirmed: the one-time code `pair.confirm` must repeat. */
  code: string | null
  phoneKey: string | null
  phoneName: string | null
  confirmed: boolean
  createdAt: number
  /** Last `n` sent: never reused across restarts. */
  out: number
}

interface Saved {
  v: 1
  enabled: boolean
  pairings: Record<string, SavedPairing>
}

export interface RelayView {
  enabled: boolean
  signedIn: boolean
  connected: boolean
  error: string | null
  pairings: { id: string; name: string | null; confirmed: boolean; online: boolean }[]
  /** The pairing link being shown, until it is claimed or expires. */
  pending: { id: string; link: string; expiresAt: number } | null
}

type Socket = {
  readyState: number
  send(text: string): void
  close(): void
  onopen: ((e: Event) => void) | null
  onmessage: ((e: MessageEvent) => void) | null
  onclose: ((e: CloseEvent) => void) | null
  onerror: ((e: Event) => void) | null
}

/** The socket main holds (`relay:*`), as a WebSocket-like object for the host. */
class MainSocket implements Socket {
  readyState = 0
  onopen: ((e: Event) => void) | null = null
  onmessage: ((e: MessageEvent) => void) | null = null
  onclose: ((e: CloseEvent) => void) | null = null
  onerror: ((e: Event) => void) | null = null
  constructor(
    private api: RelayApi,
    readonly gen: number
  ) {}
  send(text: string): void {
    void this.api.relaySend(this.gen, text)
  }
  close(): void {
    this.readyState = 3
    void this.api.relayClose(this.gen)
  }
  event(e: RelayEvent): void {
    if (e.kind === 'open') {
      this.readyState = 1
      this.onopen?.(new Event('open'))
    } else if (e.kind === 'frame') this.onmessage?.({ data: e.text } as MessageEvent)
    else {
      this.readyState = 3
      this.onclose?.({} as CloseEvent)
    }
  }
}

export interface HostDeps {
  api: RelayApi
  store: StoreApi<AppState>
  chat: ChatBus
  /** Opens the relay socket; default: main's (`relay:connect`). Tests pass their own. */
  openSocket?: () => Promise<Socket | { error: string }>
  now?: () => number
  /** ms; tests make these short. */
  timers?: { state: number; flush: number; startWait: number }
}

const START_WAIT_MS = 20_000

export class RelayHost {
  private saved: Saved = { v: 1, enabled: true, pairings: {} }
  private loaded = false
  private signedIn = false
  private ws: Socket | null = null
  private seq = 0
  private retry = 0
  private retryTimer: ReturnType<typeof setTimeout> | null = null
  private stateTimer: ReturnType<typeof setInterval> | null = null
  private saveTimer: ReturnType<typeof setTimeout> | null = null
  private online = new Set<string>()
  /** Pairings that asked for `state` (sent again whenever it changes). */
  private synced = new Set<string>()
  private lastState = new Map<string, string>()
  private channels = new Map<string, Channel>()
  private outCounters = new Map<string, Counter>()
  private inCounters = new Map<string, Counter>()
  /** pairing → pane → item id → last sent JSON. */
  private open = new Map<string, Map<string, Map<string, string>>>()
  private lastBusy = new Map<string, boolean>()
  private paneSubs = new Map<string, () => void>()
  private dirtyPanes = new Set<string>()
  private flushTimer: ReturnType<typeof setTimeout> | null = null
  private pending: RelayView['pending'] = null
  private error: string | null = null
  private listeners = new Set<() => void>()
  private view: RelayView = this.makeView()
  private stopCloud: (() => void) | null = null
  private readonly openSocket: () => Promise<Socket | { error: string }>
  private gen = 0
  private mainSocket: MainSocket | null = null
  private stopRelay: (() => void) | null = null
  private readonly now: () => number
  private readonly timers: NonNullable<HostDeps['timers']>

  constructor(private deps: HostDeps) {
    this.openSocket = deps.openSocket ?? (() => this.openInMain())
    this.now = deps.now ?? Date.now
    this.timers = deps.timers ?? { state: 1000, flush: 200, startWait: START_WAIT_MS }
  }

  // ---- lifecycle --------------------------------------------------------------------

  async start(): Promise<void> {
    const raw = await this.deps.api.relayLoad()
    if (raw && typeof raw === 'object' && (raw as Saved).v === 1) this.saved = raw as Saved
    this.loaded = true
    this.stopCloud = this.deps.api.onChanged((s) => this.setSignedIn(s.signedIn))
    if (!this.deps.openSocket)
      this.stopRelay = this.deps.api.onRelay((e) => {
        if (e.gen === this.mainSocket?.gen) this.mainSocket.event(e)
      })
    this.setSignedIn((await this.deps.api.status()).signedIn)
  }

  stop(): void {
    this.stopCloud?.()
    this.stopRelay?.()
    this.disconnect()
  }

  private setSignedIn(on: boolean): void {
    if (on === this.signedIn) return
    this.signedIn = on
    if (!on) {
      // Signing out ends this device's pairings on the server; main deletes relay.json.
      this.saved.pairings = {}
      this.pending = null
    }
    this.reconcile()
  }

  /** Connects when there is something to connect for, disconnects otherwise. */
  private reconcile(): void {
    const want =
      this.loaded &&
      this.signedIn &&
      this.saved.enabled &&
      Object.keys(this.saved.pairings).length > 0
    if (want && !this.ws && !this.retryTimer) void this.connect()
    if (!want) this.disconnect()
    this.changed()
  }

  private async openInMain(): Promise<Socket | { error: string }> {
    const sock = new MainSocket(this.deps.api, ++this.gen)
    this.mainSocket = sock
    const r = await this.deps.api.relayConnect(sock.gen)
    return r.ok ? sock : { error: r.error }
  }

  private async connect(): Promise<void> {
    this.retryTimer = null
    const ws = await this.openSocket()
    if ('error' in ws) {
      this.error = ws.error
      this.changed()
      return this.scheduleRetry()
    }
    this.ws = ws
    this.seq = 0
    const hello = (): void => ws.send(JSON.stringify({ t: 'hello', v: [1] }))
    ws.onopen = hello
    ws.onmessage = (e) => this.onFrame(String(e.data))
    ws.onerror = () => undefined
    ws.onclose = () => {
      if (this.ws !== ws) return
      this.ws = null
      this.online.clear()
      this.synced.clear()
      this.clearSubs()
      this.changed()
      this.scheduleRetry()
    }
    // Main's socket may have opened before these handlers were set.
    if (ws instanceof MainSocket && ws.readyState === 1) hello()
  }

  private scheduleRetry(): void {
    if (this.retryTimer || !this.signedIn || !this.saved.enabled) return
    const wait = Math.min(60_000, 1000 * 2 ** this.retry++)
    this.retryTimer = setTimeout(() => {
      this.retryTimer = null
      this.reconcile()
    }, wait)
  }

  private disconnect(): void {
    if (this.retryTimer) clearTimeout(this.retryTimer)
    this.retryTimer = null
    const ws = this.ws
    this.ws = null
    ws?.close()
    this.online.clear()
    this.synced.clear()
    this.clearSubs()
    this.stopStateTimer()
  }

  // ---- what the Settings row uses ------------------------------------------------------

  subscribe = (cb: () => void): (() => void) => {
    this.listeners.add(cb)
    return () => this.listeners.delete(cb)
  }

  getView = (): RelayView => this.view

  private makeView(): RelayView {
    return {
      enabled: this.saved.enabled,
      signedIn: this.signedIn,
      connected: this.ws?.readyState === 1,
      error: this.error,
      pairings: Object.entries(this.saved.pairings)
        .filter(([, p]) => p.phoneKey)
        .map(([id, p]) => ({
          id,
          name: p.phoneName,
          confirmed: p.confirmed,
          online: this.online.has(id)
        })),
      pending: this.pending
    }
  }

  private changed(): void {
    this.view = this.makeView()
    for (const cb of this.listeners) cb()
  }

  async setEnabled(on: boolean): Promise<void> {
    this.saved.enabled = on
    await this.persist()
    this.reconcile()
  }

  /** A new pairing: its one-time link for the browser. */
  async pair(): Promise<{ ok: true; link: string } | { ok: false; error: string }> {
    const r = await this.deps.api.relayPair()
    if (!r.ok) return { ok: false, error: r.error }
    const keys = newKeyPair()
    this.saved.pairings[r.pairing.id] = {
      secretKey: keys.secretKey,
      publicKey: keys.publicKey,
      deviceName: r.pairing.deviceName,
      code: r.code,
      phoneKey: null,
      phoneName: null,
      confirmed: false,
      createdAt: this.now(),
      out: 0
    }
    // A pending pairing nobody claimed is dropped (the server's lapse too).
    for (const [id, p] of Object.entries(this.saved.pairings))
      if (!p.phoneKey && id !== r.pairing.id) delete this.saved.pairings[id]
    await this.persist()
    const link = pairingLinkUrl(r.appUrl, {
      v: 1,
      server: r.server,
      pairingId: r.pairing.id,
      code: r.code,
      desktopKey: keys.publicKey,
      deviceName: r.pairing.deviceName
    })
    this.pending = { id: r.pairing.id, link, expiresAt: Date.parse(r.pairing.expiresAt) }
    this.error = null
    this.reconcile()
    return { ok: true, link }
  }

  cancelPair(): void {
    this.pending = null
    this.changed()
  }

  async unpair(id: string): Promise<SimpleResult> {
    const r = await this.deps.api.relayUnpair(id)
    // Gone on the server already: forget it here too.
    if (r.ok || /not found|No such/i.test(r.error)) await this.forget(id)
    return r.ok || /not found|No such/i.test(r.error) ? { ok: true } : r
  }

  private async forget(id: string): Promise<void> {
    delete this.saved.pairings[id]
    this.channels.delete(id)
    this.outCounters.delete(id)
    this.inCounters.delete(id)
    this.online.delete(id)
    this.synced.delete(id)
    this.closeAll(id)
    if (this.pending?.id === id) this.pending = null
    await this.persist()
    this.reconcile()
  }

  // ---- frames --------------------------------------------------------------------------

  private onFrame(text: string): void {
    let f: { t?: string; [k: string]: unknown }
    try {
      f = JSON.parse(text)
    } catch {
      return
    }
    switch (f.t) {
      case 'welcome':
        this.retry = 0
        this.error = null
        this.changed()
        // A claim made while we were offline only shows in the list.
        void this.syncPairings()
        break
      case 'presence': {
        const id = String(f.pairing)
        if (f.online && !this.saved.pairings[id]?.phoneKey) void this.syncPairings()
        if (f.online) this.online.add(id)
        else {
          this.online.delete(id)
          this.synced.delete(id)
          this.closeAll(id)
        }
        this.changed()
        break
      }
      case 'pairing.claimed': {
        const p = this.saved.pairings[String(f.pairing)]
        if (!p || p.phoneKey) break
        p.phoneKey = String(f.phoneKey)
        p.phoneName = String(f.phoneName).slice(0, 64)
        this.online.add(String(f.pairing))
        if (this.pending?.id === f.pairing) this.pending = null
        void this.persist()
        this.changed()
        break
      }
      case 'pairing.revoked':
        void this.forget(String(f.pairing))
        break
      case 'data':
        this.onData(String(f.pairing), String(f.payload))
        break
      case 'error':
        if (f.code === 'peer_offline') this.online.delete(String(f.ref ?? ''))
        break
    }
  }

  /**
   * Takes the server's list as the truth for who claimed what: fills in a browser's key for a
   * claim we missed while offline, and forgets pairings the server no longer has.
   */
  private async syncPairings(): Promise<void> {
    const r = await this.deps.api.relayPairings()
    if (!r.ok) return
    const live = new Map(r.pairings.map((p) => [p.id, p]))
    let dirty = false
    for (const [id, mine] of Object.entries(this.saved.pairings)) {
      const theirs = live.get(id)
      if (!theirs) {
        delete this.saved.pairings[id]
        this.channels.delete(id)
        if (this.pending?.id === id) this.pending = null
        dirty = true
      } else if (!mine.phoneKey && theirs.status === 'active' && theirs.phoneKey) {
        mine.phoneKey = theirs.phoneKey
        mine.phoneName = theirs.phoneName
        if (this.pending?.id === id) this.pending = null
        dirty = true
      }
    }
    if (dirty) {
      await this.persist()
      this.reconcile()
    }
  }

  private channelOf(id: string): Channel | null {
    const p = this.saved.pairings[id]
    if (!p?.phoneKey) return null
    let ch = this.channels.get(id)
    if (!ch) {
      try {
        ch = channel('desktop', p.secretKey, p.phoneKey, id)
      } catch {
        return null
      }
      this.channels.set(id, ch)
    }
    return ch
  }

  private onData(id: string, payload: string): void {
    const p = this.saved.pairings[id]
    const ch = this.channelOf(id)
    if (!p || !ch) return
    let inc = this.inCounters.get(id)
    if (!inc) this.inCounters.set(id, (inc = new Counter()))
    const msg = decode(ch, inc, payload, ToDesktop)
    if (!msg) return
    this.online.add(id)
    if (!p.confirmed) {
      // Only the one-time code from our pairing link, with the key it claimed with, makes it trusted.
      if (msg.kind === 'pair.confirm' && msg.code === p.code && msg.phoneKey === p.phoneKey) {
        p.confirmed = true
        p.code = null
        p.phoneName = msg.phoneName
        void this.persist()
        this.changed()
      }
      return
    }
    void this.handle(id, msg)
  }

  private send(id: string, msg: ToWeb): void {
    const ws = this.ws
    const ch = this.channelOf(id)
    const p = this.saved.pairings[id]
    if (!ws || ws.readyState !== 1 || !ch || !p?.confirmed) return
    let out = this.outCounters.get(id)
    if (!out) this.outCounters.set(id, (out = new Counter(p.out)))
    const payload = encode(ch, out, msg)
    p.out = out.last
    this.persistSoon()
    ws.send(JSON.stringify({ t: 'data', pairing: id, seq: ++this.seq, payload }))
  }

  // ---- messages ------------------------------------------------------------------------

  private async handle(id: string, msg: ToDesktop): Promise<void> {
    const s = this.deps.store.getState()
    switch (msg.kind) {
      case 'pair.confirm':
        return
      case 'sync':
        this.synced.add(id)
        this.lastState.delete(id)
        this.pushState()
        this.startStateTimer()
        return
      case 'chat.open': {
        if (!findPane(s.config, msg.paneId) || paneView(s.config, msg.paneId) !== 'chat')
          return this.send(id, { kind: 'chat.gone', paneId: msg.paneId })
        if (!this.deps.chat.thread(msg.paneId).items.length) await this.deps.chat.load(msg.paneId)
        const snap = snapshot(msg.paneId, this.deps.chat.thread(msg.paneId))
        let panes = this.open.get(id)
        if (!panes) this.open.set(id, (panes = new Map()))
        const sent = new Map<string, string>()
        markSent(sent, snap.items)
        panes.set(msg.paneId, sent)
        this.lastBusy.set(`${id} ${msg.paneId}`, snap.busy)
        this.watch(msg.paneId)
        return this.send(id, snap)
      }
      case 'chat.close':
        this.open.get(id)?.delete(msg.paneId)
        return this.unwatchIdle(msg.paneId)
      case 'chat.send': {
        const err = await this.ready(msg.paneId)
        if (err) return this.send(id, { kind: 'ack', req: msg.req, ok: false, error: err })
        this.deps.store.getState().chatSend(msg.paneId, msg.text, [])
        return this.send(id, { kind: 'ack', req: msg.req, ok: true })
      }
      case 'chat.stop': {
        if (!findPane(s.config, msg.paneId))
          return this.send(id, {
            kind: 'ack',
            req: msg.req,
            ok: false,
            error: 'That chat is gone.'
          })
        s.chatInterrupt(msg.paneId)
        return this.send(id, { kind: 'ack', req: msg.req, ok: true })
      }
      case 'chat.new': {
        const r = this.newChat(msg.wsId)
        if ('error' in r)
          return this.send(id, { kind: 'ack', req: msg.req, ok: false, error: r.error })
        if (msg.text) {
          const err = await this.ready(r.paneId)
          if (err)
            return this.send(id, {
              kind: 'ack',
              req: msg.req,
              ok: false,
              error: err,
              paneId: r.paneId
            })
          this.deps.store.getState().chatSend(r.paneId, msg.text, [])
        }
        this.pushState()
        return this.send(id, { kind: 'ack', req: msg.req, ok: true, paneId: r.paneId })
      }
    }
  }

  /** Starts a chat that isn't running yet; an error to report, or null once it can take input. */
  private async ready(paneId: string): Promise<string | null> {
    const s = this.deps.store.getState()
    if (!findPane(s.config, paneId) || paneView(s.config, paneId) !== 'chat')
      return 'That chat is gone.'
    s.requestStart(paneId)
    const until = this.now() + this.timers.startWait
    for (;;) {
      const rt = this.deps.store.getState().runtime[paneId]
      if (rt?.started) return null
      if (rt?.exit) return 'The chat could not start on your computer.'
      if (this.now() > until) return 'The chat is taking too long to start on your computer.'
      await new Promise((r) => setTimeout(r, 200))
    }
  }

  /** A chat in a project, without moving the desktop's window off what it shows. */
  private newChat(wsId: string): { paneId: string } | { error: string } {
    const s = this.deps.store.getState()
    const ws = s.config.workspaces.find((w) => w.id === wsId)
    if (!ws) return { error: 'That project is gone.' }
    if (ws.machineId) return { error: 'Chats on another machine start from the desktop only.' }
    const cwd = projectFolder(ws)
    const accountId = newChatAccount(s.config, wsId)
    if (!cwd || !accountId) return { error: 'This project has no folder or account to chat with.' }
    const back = s.config.activeWorkspace
    if (back !== wsId) s.switchWorkspace(wsId)
    const paneId = this.deps.store
      .getState()
      .createPane({ accountId, cwd, args: [], shell: false, slotId: null })
    if (back && back !== wsId) this.deps.store.getState().switchWorkspace(back)
    return { paneId }
  }

  // ---- pushing state and chat updates -------------------------------------------------

  private pushState(): void {
    if (!this.synced.size) return
    const s = this.deps.store.getState()
    const msg = state(Object.values(this.saved.pairings)[0]?.deviceName ?? 'Wraithgrid', {
      config: s.config,
      isChat: (paneId) => paneView(s.config, paneId) === 'chat',
      thread: (paneId) => this.deps.chat.thread(paneId),
      activeAt: (paneId) => s.runtime[paneId]?.activeAt ?? null
    })
    const json = JSON.stringify(msg)
    const live = new Set(msg.projects.flatMap((p) => p.chats.map((c) => c.paneId)))
    for (const id of this.synced) {
      if (this.lastState.get(id) !== json) {
        this.lastState.set(id, json)
        this.send(id, msg)
      }
      // Chats closed on the desktop.
      for (const paneId of [...(this.open.get(id)?.keys() ?? [])])
        if (!live.has(paneId) && !findPane(s.config, paneId)) {
          this.open.get(id)?.delete(paneId)
          this.send(id, { kind: 'chat.gone', paneId })
          this.unwatchIdle(paneId)
        }
    }
  }

  private startStateTimer(): void {
    if (!this.stateTimer) this.stateTimer = setInterval(() => this.pushState(), this.timers.state)
  }

  private stopStateTimer(): void {
    if (this.stateTimer) clearInterval(this.stateTimer)
    this.stateTimer = null
  }

  private watch(paneId: string): void {
    if (this.paneSubs.has(paneId)) return
    this.paneSubs.set(
      paneId,
      this.deps.chat.subscribe(paneId, () => {
        this.dirtyPanes.add(paneId)
        this.flushSoon()
      })
    )
  }

  private unwatchIdle(paneId: string): void {
    for (const panes of this.open.values()) if (panes.has(paneId)) return
    this.paneSubs.get(paneId)?.()
    this.paneSubs.delete(paneId)
  }

  private flushSoon(): void {
    if (this.flushTimer) return
    this.flushTimer = setTimeout(() => {
      this.flushTimer = null
      const panes = [...this.dirtyPanes]
      this.dirtyPanes.clear()
      for (const paneId of panes) {
        const thread = this.deps.chat.thread(paneId)
        for (const [id, open] of this.open) {
          const sent = open.get(paneId)
          if (!sent) continue
          const key = `${id} ${paneId}`
          for (const f of updates(paneId, thread, sent, this.lastBusy.get(key) ?? null))
            this.send(id, f)
          this.lastBusy.set(key, thread.busy)
        }
      }
    }, this.timers.flush)
  }

  private closeAll(id: string): void {
    const panes = [...(this.open.get(id)?.keys() ?? [])]
    this.open.delete(id)
    for (const paneId of panes) this.unwatchIdle(paneId)
  }

  private clearSubs(): void {
    for (const off of this.paneSubs.values()) off()
    this.paneSubs.clear()
    this.open.clear()
  }

  // ---- saving ----------------------------------------------------------------------------

  private async persist(): Promise<void> {
    if (this.saveTimer) clearTimeout(this.saveTimer)
    this.saveTimer = null
    await this.deps.api.relaySave(this.saved)
  }

  private persistSoon(): void {
    if (!this.saveTimer) this.saveTimer = setTimeout(() => void this.persist(), 2000)
  }
}
