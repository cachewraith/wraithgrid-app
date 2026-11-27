// The desktop end of the relay (the web app at app.wraithgrid.cachewraith.com, or a phone,
// chats with this app through wraithgrid-server; spec: wraithgrid-protocol docs/RELAY-APP.md).
// Runs in the renderer because the chats live here (store + ChatBus). Main only signs the
// server calls (`cloud:relay*`) and keeps relay.json. Every frame is sealed end to end
// (src/shared/relay): the server sees who is online and how much, never what.
//
// A paired browser is trusted only after its first sealed message, `pair.confirm`, carries the
// one-time code from our pairing link and the key it claimed with. Nothing it sends acts on its own:
// it can read chats, send a message (with images) to a chat, start one in a project, stop a
// turn, answer claude's questions, or switch a chat's model, effort or a mode that still asks —
// as using this window would — but not approve tool calls.
import type { StoreApi } from 'zustand'
import type { CloudStatus, RelayEvent, SimpleResult } from '@shared/ipc-contract'
import { channel, newKeyPair, pairingCode, type Channel } from '@shared/relay/crypto'
import {
  Counter,
  decode,
  encode,
  pairingLinkUrl,
  ToDesktop,
  type ToWeb,
  type WireCommand,
  type WireSettings,
  type WireTask
} from '@shared/relay/messages'
import {
  CHAT_EFFORTS,
  CHAT_IMAGE_MAX,
  CHAT_MODEL_PATTERN,
  type ChatEffort,
  type ChatImage,
  type ChatMode
} from '@shared/chat'
import { findPane, paneView, type AppState } from '../../app/store'
import type { ChatBus } from '../chat-bus'
import { projectFolder } from '../team'
import {
  markSent,
  newChatAccount,
  snapshot,
  state,
  thumbFrames,
  updates,
  wireCommands,
  wireSettings,
  wireThumbs,
  backgroundKey,
  wireBackground
} from './wire'

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
  relayRequestKey(id: string, desktopKey: string): Promise<SimpleResult>
  relayRequestAnswer(id: string, allow: boolean): Promise<SimpleResult>
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
  /** This computer's name on the account (from a pairing or a request). */
  deviceName?: string
}

export interface RelayView {
  enabled: boolean
  signedIn: boolean
  connected: boolean
  error: string | null
  pairings: { id: string; name: string | null; confirmed: boolean; online: boolean }[]
  /** The pairing link being shown, until it is claimed or expires. */
  pending: { id: string; link: string; expiresAt: number } | null
  /**
   * One-click pairing: a signed-in browser asks to use this computer. Shown app-wide with
   * Allow / Deny and the code the browser shows too.
   */
  request: { id: string; name: string; code: string; expiresAt: number } | null
}

/** How long the desktop's Allow / Deny stays open (the server's own limit). */
const REQUEST_MS = 2 * 60_000

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
/** An image upload not finished (or not sent) within this is dropped. */
const UPLOAD_MS = 3 * 60_000
/** One message's thumbnails go out at most once in this long per pairing (`chat.thumbs`). */
const THUMBS_MS = 2000
/** Image uploads one browser may have going at once. */
const UPLOADS_MAX = 8

interface Upload {
  mediaType: ChatImage['mediaType']
  parts: (string | undefined)[]
  got: number
  size: number
  at: number
}

export class RelayHost {
  private saved: Saved = { v: 1, enabled: true, pairings: {} }
  private loaded = false
  private signedIn = false
  private ws: Socket | null = null
  private seq = 0
  private retry = 0
  private retryTimer: ReturnType<typeof setTimeout> | null = null
  /** A connect is waiting for main's socket: a second one would race it (see `connect`). */
  private connecting = false
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
  /** pairing + pane → the commands last sent (JSON), so a change goes out once. */
  private lastCommands = new Map<string, string>()
  /** `${pairing} ${pane}` → the background work last sent (`backgroundKey`). */
  private lastBackground = new Map<string, string>()
  /** pairing + pane → the settings last sent (JSON). */
  private lastSettings = new Map<string, string>()
  /** pairing → its image uploads in progress (`chat.image`), by upload id. */
  private uploads = new Map<string, Map<string, Upload>>()
  /** When each `${pairing} ${pane} ${item}`'s thumbnails were last sent. */
  private thumbsSent = new Map<string, number>()
  private paneSubs = new Map<string, () => void>()
  private dirtyPanes = new Set<string>()
  private flushTimer: ReturnType<typeof setTimeout> | null = null
  private pending: RelayView['pending'] = null
  private request: RelayView['request'] = null
  private requestTimer: ReturnType<typeof setTimeout> | null = null
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
      this.request = null
    }
    this.reconcile()
  }

  /** Connects when there is something to connect for, disconnects otherwise. */
  private reconcile(): void {
    // Connected whenever web access is on, so a browser can ask to pair with one click.
    const want = this.loaded && this.signedIn && this.saved.enabled
    if (want && !this.ws && !this.retryTimer && !this.connecting) void this.connect()
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
    // One at a time: two overlapping connects could resolve out of order and leave `this.ws` on
    // the older socket, whose events are never delivered (it never opens or closes), so the
    // desktop would stay offline for good.
    this.connecting = true
    let ws: Socket | { error: string }
    try {
      ws = await this.openSocket()
    } catch (e) {
      ws = { error: e instanceof Error ? e.message : String(e) }
    } finally {
      this.connecting = false
    }
    if (!this.loaded || !this.signedIn || !this.saved.enabled) {
      if (!('error' in ws)) ws.close()
      return
    }
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

  /**
   * The network is back or the computer woke from sleep: reconnect now instead of waiting out
   * the backoff, and replace a socket that may have died while asleep (the server dropped it
   * and told the browsers this computer is offline; main's ping would notice only later).
   */
  wake(): void {
    if (!this.loaded || !this.signedIn || !this.saved.enabled) return
    if (this.retryTimer) clearTimeout(this.retryTimer)
    this.retryTimer = null
    this.retry = 0
    const ws = this.ws
    if (ws) {
      this.ws = null
      ws.close()
      this.online.clear()
      this.synced.clear()
      this.clearSubs()
    }
    this.reconcile()
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
        // A one-click request (no code) is listed once allowed; until then it's the dialog.
        .filter(([, p]) => p.phoneKey && (p.confirmed || p.code))
        .map(([id, p]) => ({
          id,
          name: p.phoneName,
          confirmed: p.confirmed,
          online: this.online.has(id)
        })),
      pending: this.pending,
      request: this.request
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
    this.saved.deviceName = r.pairing.deviceName
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

  /** One-click pairing: the user's answer to the browser that asked. */
  async answerRequest(allow: boolean): Promise<SimpleResult> {
    const req = this.request
    if (!req) return { ok: true }
    this.request = null
    if (this.requestTimer) clearTimeout(this.requestTimer)
    this.requestTimer = null
    const r = await this.deps.api.relayRequestAnswer(req.id, allow)
    const p = this.saved.pairings[req.id]
    if (allow && r.ok && p) {
      p.confirmed = true
      this.online.add(req.id)
      await this.persist()
      this.reconcile()
    } else await this.forget(req.id)
    return r
  }

  private async onRequest(id: string, phoneName: string, phoneKey: string, deviceName: string) {
    if (!this.saved.enabled || this.saved.pairings[id] || !/^[A-Za-z0-9_-]{43}$/.test(phoneKey))
      return
    // A newer request replaces one still open (the browser asked again).
    if (this.request) void this.answerRequest(false)
    const keys = newKeyPair()
    this.saved.pairings[id] = {
      secretKey: keys.secretKey,
      publicKey: keys.publicKey,
      deviceName,
      code: null,
      phoneKey,
      phoneName: phoneName.slice(0, 64),
      // Trusted once the user presses Allow here, after comparing the codes.
      confirmed: false,
      createdAt: this.now(),
      out: 0
    }
    this.saved.deviceName = deviceName
    await this.persist()
    const r = await this.deps.api.relayRequestKey(id, keys.publicKey)
    if (!r.ok) {
      await this.forget(id)
      return
    }
    this.request = {
      id,
      name: phoneName.slice(0, 64),
      code: pairingCode(id, keys.publicKey, phoneKey),
      expiresAt: this.now() + REQUEST_MS
    }
    this.requestTimer = setTimeout(() => {
      if (this.request?.id === id) void this.answerRequest(false)
    }, REQUEST_MS)
    this.changed()
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
    if (this.request?.id === id) this.request = null
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
      case 'pairing.request':
        void this.onRequest(
          String(f.pairing),
          String(f.phoneName),
          String(f.phoneKey),
          String(f.deviceName ?? '')
        )
        break
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
        const snap = snapshot(
          msg.paneId,
          this.deps.chat.thread(msg.paneId),
          this.settingsOf(msg.paneId),
          this.now()
        )
        let panes = this.open.get(id)
        if (!panes) this.open.set(id, (panes = new Map()))
        const sent = new Map<string, string>()
        markSent(sent, snap.items)
        panes.set(msg.paneId, sent)
        this.lastBusy.set(`${id} ${msg.paneId}`, snap.busy)
        this.lastCommands.set(`${id} ${msg.paneId}`, JSON.stringify(snap.commands ?? []))
        this.lastBackground.set(
          `${id} ${msg.paneId}`,
          backgroundKey(this.deps.chat.thread(msg.paneId))
        )
        this.lastSettings.set(`${id} ${msg.paneId}`, JSON.stringify(snap.settings ?? null))
        this.watch(msg.paneId)
        return this.send(id, snap)
      }
      case 'chat.close':
        this.open.get(id)?.delete(msg.paneId)
        return this.unwatchIdle(msg.paneId)
      case 'chat.send': {
        const imgs = this.takeImages(id, msg.images)
        if ('error' in imgs)
          return this.send(id, { kind: 'ack', req: msg.req, ok: false, error: imgs.error })
        const err = await this.ready(msg.paneId)
        if (err) return this.send(id, { kind: 'ack', req: msg.req, ok: false, error: err })
        this.deps.store.getState().chatSend(msg.paneId, msg.text, imgs.images)
        return this.send(id, { kind: 'ack', req: msg.req, ok: true })
      }
      case 'chat.image':
        return this.addImagePart(id, msg)
      case 'chat.thumbs':
        return this.sendThumbs(id, msg.paneId, msg.item)
      case 'chat.answer':
        return this.send(id, { kind: 'ack', req: msg.req, ...this.answer(msg) })
      case 'chat.set': {
        const r = this.setting(msg)
        if (r.ok) {
          this.dirtyPanes.add(msg.paneId)
          this.flushSoon()
        }
        return this.send(id, { kind: 'ack', req: msg.req, ...r })
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
        const imgs = this.takeImages(id, msg.images)
        if ('error' in imgs)
          return this.send(id, { kind: 'ack', req: msg.req, ok: false, error: imgs.error })
        const r = this.newChat(msg.wsId)
        if ('error' in r)
          return this.send(id, { kind: 'ack', req: msg.req, ok: false, error: r.error })
        if (msg.text || imgs.images.length) {
          const err = await this.ready(r.paneId)
          if (err)
            return this.send(id, {
              kind: 'ack',
              req: msg.req,
              ok: false,
              error: err,
              paneId: r.paneId
            })
          this.deps.store.getState().chatSend(r.paneId, msg.text, imgs.images)
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
    const device =
      this.saved.deviceName || Object.values(this.saved.pairings)[0]?.deviceName || 'Wraithgrid'
    const msg = state(device, {
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
    if (!this.stateTimer)
      this.stateTimer = setInterval(() => {
        this.pushState()
        // Settings changed in this window (no chat event) reach open chats within a tick.
        for (const [id, panes] of this.open)
          for (const paneId of panes.keys()) this.sendExtras(id, paneId)
      }, this.timers.state)
  }

  /** A chat's settings for the web, null when it is not a chat (anymore). */
  private settingsOf(paneId: string): WireSettings | null {
    const s = this.deps.store.getState()
    const found = findPane(s.config, paneId)
    if (!found || paneView(s.config, paneId) !== 'chat') return null
    return wireSettings(found.pane, this.deps.chat.thread(paneId))
  }

  /**
   * The CLI's commands and the chat's settings, in an update when they changed since last sent:
   * the CLI says its commands once it starts (on the first send), and settings change here too.
   */
  private sendExtras(id: string, paneId: string): void {
    const key = `${id} ${paneId}`
    const thread = this.deps.chat.thread(paneId)
    const extra: { commands?: WireCommand[]; settings?: WireSettings; background?: WireTask[] } = {}
    const commands = wireCommands(thread)
    const cj = JSON.stringify(commands)
    if (this.lastCommands.get(key) !== cj) {
      this.lastCommands.set(key, cj)
      extra.commands = commands
    }
    const settings = this.settingsOf(paneId)
    const sj = JSON.stringify(settings)
    if (settings && this.lastSettings.get(key) !== sj) {
      this.lastSettings.set(key, sj)
      extra.settings = settings
    }
    const bk = backgroundKey(thread)
    if (this.lastBackground.get(key) !== bk) {
      this.lastBackground.set(key, bk)
      extra.background = wireBackground(thread, this.now())
    }
    if (extra.commands || extra.settings || extra.background)
      this.send(id, { kind: 'chat.update', paneId, items: [], busy: thread.busy, ...extra })
  }

  // ---- what a browser may change -------------------------------------------------------

  /**
   * Answers claude's question (AskUserQuestion) as this window's question card would. Only a
   * question waiting for its answer: any other approval is refused, as tool approvals stay here.
   */
  private answer(msg: Extract<ToDesktop, { kind: 'chat.answer' }>): {
    ok: boolean
    error?: string
  } {
    const s = this.deps.store.getState()
    if (!findPane(s.config, msg.paneId)) return { ok: false, error: 'That chat is gone.' }
    const item = this.deps.chat
      .thread(msg.paneId)
      .items.find((it) => it.kind === 'tool' && it.approval?.reqId === msg.ask)
    const a = item?.kind === 'tool' ? item.approval : null
    if (!a?.questions?.length) return { ok: false, error: 'There is no such question to answer.' }
    if (a.decided !== null) return { ok: false, error: 'That question was already answered.' }
    if (msg.skip) {
      s.chatApprove(msg.paneId, msg.ask, 'deny')
      return { ok: true }
    }
    const asked = new Set(a.questions.map((q) => q.question))
    const answers = msg.answers ?? {}
    if (!Object.keys(answers).every((q) => asked.has(q)))
      return { ok: false, error: 'Those answers don’t match the questions.' }
    s.chatApprove(msg.paneId, msg.ask, 'allow', answers)
    return { ok: true }
  }

  /** Switches a chat's model, effort or mode, only to what `wireSettings` offers. */
  private setting(msg: Extract<ToDesktop, { kind: 'chat.set' }>): {
    ok: boolean
    error?: string
  } {
    const s = this.deps.store.getState()
    const found = findPane(s.config, msg.paneId)
    if (!found || paneView(s.config, msg.paneId) !== 'chat')
      return { ok: false, error: 'That chat is gone.' }
    const thread = this.deps.chat.thread(msg.paneId)
    const offered = wireSettings(found.pane, thread)
    if (msg.mode !== undefined) {
      if (!offered.modes.includes(msg.mode))
        return { ok: false, error: 'That mode can only be picked on the computer.' }
      s.chatSetMode(msg.paneId, msg.mode as ChatMode)
    } else if (msg.model !== undefined) {
      if (
        msg.model !== null &&
        (!CHAT_MODEL_PATTERN.test(msg.model) || !thread.models.some((m) => m.value === msg.model))
      )
        return { ok: false, error: 'That model isn’t offered for this chat.' }
      s.chatSetModel(msg.paneId, msg.model)
    } else if (msg.effort !== undefined) {
      const chosen =
        thread.models.find((m) => m.value === (found.pane.model ?? 'default')) ?? thread.models[0]
      const e = msg.effort
      if (
        e !== null &&
        (!(CHAT_EFFORTS as readonly string[]).includes(e) || !chosen?.efforts.includes(e))
      )
        return { ok: false, error: 'That effort isn’t offered for this model.' }
      s.chatSetEffort(msg.paneId, e as ChatEffort | null)
    }
    return { ok: true }
  }

  /** One frame of an image upload; uploads past their time or limits are dropped. */
  private addImagePart(id: string, msg: Extract<ToDesktop, { kind: 'chat.image' }>): void {
    const now = this.now()
    let mine = this.uploads.get(id)
    if (!mine) this.uploads.set(id, (mine = new Map()))
    for (const [k, u] of mine) if (now - u.at > UPLOAD_MS) mine.delete(k)
    let u = mine.get(msg.upload)
    if (!u) {
      if (mine.size >= UPLOADS_MAX) return
      u = { mediaType: msg.mediaType, parts: new Array(msg.parts), got: 0, size: 0, at: now }
      mine.set(msg.upload, u)
    }
    if (
      u.parts.length !== msg.parts ||
      u.mediaType !== msg.mediaType ||
      u.parts[msg.part] !== undefined
    )
      return
    if (u.size + msg.data.length > CHAT_IMAGE_MAX) {
      mine.delete(msg.upload)
      return
    }
    u.parts[msg.part] = msg.data
    u.got++
    u.size += msg.data.length
  }

  /** A user message's thumbnails, to a pairing that has its chat open; not again within THUMBS_MS. */
  private sendThumbs(id: string, paneId: string, item: string): void {
    if (!this.open.get(id)?.has(paneId)) return
    const it = this.deps.chat.thread(paneId).items.find((x) => x.id === item)
    if (it?.kind !== 'user') return
    const thumbs = wireThumbs(it.thumbs)
    if (!thumbs.length) return
    const now = this.now()
    for (const [k, at] of this.thumbsSent) if (now - at > THUMBS_MS) this.thumbsSent.delete(k)
    const key = `${id} ${paneId} ${item}`
    if (this.thumbsSent.has(key)) return
    this.thumbsSent.set(key, now)
    for (const f of thumbFrames(paneId, item, thumbs)) this.send(id, f)
  }

  /** The finished uploads a message names, as images (and forgotten); or why not. */
  private takeImages(
    id: string,
    ids: string[] | undefined
  ): { images: ChatImage[] } | { error: string } {
    if (!ids?.length) return { images: [] }
    const mine = this.uploads.get(id)
    const images: ChatImage[] = []
    for (const k of ids) {
      const u = mine?.get(k)
      if (!u || u.got !== u.parts.length || this.now() - u.at > UPLOAD_MS)
        return { error: 'An image didn’t arrive in full. Send it again.' }
      const data = u.parts.join('')
      if (data.length % 4 !== 0) return { error: 'An image didn’t arrive in full. Send it again.' }
      images.push({ mediaType: u.mediaType, data })
    }
    for (const k of ids) mine?.delete(k)
    return { images }
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
          this.sendExtras(id, paneId)
        }
      }
    }, this.timers.flush)
  }

  private closeAll(id: string): void {
    this.uploads.delete(id)
    for (const k of this.thumbsSent.keys()) if (k.startsWith(`${id} `)) this.thumbsSent.delete(k)
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
