// What a paired web app and this desktop say inside the relay's sealed frames (v1; spec:
// wraithgrid-protocol docs/RELAY-APP.md). Copied into wraithgrid-web as src/relay/messages.ts:
// keep both identical. Every sealed frame is one `Envelope`; `n` only ever grows per direction,
// so the server can't replay or reorder frames (a stale or repeated one is dropped).
import { z } from 'zod'
import { open, seal, type Channel } from './crypto'

export const RELAY_APP_VERSION = 1

const Id = z.string().min(1).max(200)
const Key = z.string().regex(/^[A-Za-z0-9_-]{43}$/)
/** A request's own id, echoed in its `ack`. */
const Req = z.string().min(1).max(64)
export const TEXT_MAX = 20_000
export const MESSAGE_MAX = 20_000

/** One chat item as the web shows it (the desktop's ChatItem, flattened). */
export const WireItem = z.object({
  id: Id,
  kind: z.enum(['user', 'assistant', 'thinking', 'tool', 'notice', 'turn']),
  text: z.string().max(TEXT_MAX),
  /** The text was cut to fit (the start is kept for messages, the end for tool output). */
  cut: z.boolean().optional(),
  tool: z
    .object({
      name: z.string().max(200),
      subtitle: z.string().max(500),
      state: z.enum(['running', 'done', 'error', 'approval'])
    })
    .optional(),
  level: z.enum(['info', 'error', 'divider']).optional()
})
export type WireItem = z.infer<typeof WireItem>

export const WireChat = z.object({
  paneId: Id,
  title: z.string().max(200),
  /** The agent CLI: claude, codex, gemini… */
  cli: z.string().max(40),
  account: z.string().max(100),
  busy: z.boolean(),
  /** A tool call waits for approval on the desktop. */
  waiting: z.boolean(),
  activeAt: z.number().nullable()
})
export type WireChat = z.infer<typeof WireChat>

export const WireProject = z.object({
  id: Id,
  name: z.string().max(100),
  /** The project's folder on the desktop (`~/…` for the home folder); null when it has none. */
  folder: z.string().max(4096).nullable(),
  chats: z.array(WireChat).max(200),
  /** A new chat can start here (the project has a folder and an account to use). */
  canChat: z.boolean(),
  /** Its icon, `<set>:<name>` (shared/project-icons.ts); left out for the plain folder. 2.5.2+ */
  icon: z.string().max(64).optional()
})
export type WireProject = z.infer<typeof WireProject>

/** A slash command or skill the chat's CLI offers, for the web's `/` menu. 2.5.2+ */
export const WireCommand = z.object({
  name: z.string().min(1).max(100),
  description: z.string().max(300),
  /** What goes after the name, e.g. `<topic>`; '' when it takes nothing. */
  hint: z.string().max(100),
  /** `$` for a skill named in the text (codex) rather than run as `/name`. */
  sigil: z.enum(['/', '$']),
  /** Part of the CLI itself, not a plugin, skill or the user's own file. */
  builtin: z.boolean()
})
export type WireCommand = z.infer<typeof WireCommand>

/** From the web app to the desktop. */
export const ToDesktop = z.discriminatedUnion('kind', [
  /** The first message on a new pairing (protocol `PairingConfirm`). */
  z.object({
    kind: z.literal('pair.confirm'),
    code: Key,
    phoneKey: Key,
    phoneName: z.string().trim().min(1).max(64)
  }),
  /** Send me `state` now (and whenever it changes while I'm connected). */
  z.object({ kind: z.literal('sync') }),
  /** Send this chat's `chat.snapshot`, then `chat.update`s until `chat.close`. */
  z.object({ kind: z.literal('chat.open'), paneId: Id }),
  z.object({ kind: z.literal('chat.close'), paneId: Id }),
  z.object({
    kind: z.literal('chat.send'),
    req: Req,
    paneId: Id,
    text: z.string().trim().min(1).max(MESSAGE_MAX)
  }),
  /** A new chat in a project, with the project's account; `text` is its first message. */
  z.object({
    kind: z.literal('chat.new'),
    req: Req,
    wsId: Id,
    text: z.string().trim().max(MESSAGE_MAX)
  }),
  z.object({ kind: z.literal('chat.stop'), req: Req, paneId: Id })
])
export type ToDesktop = z.infer<typeof ToDesktop>

/** From the desktop to the web app. */
export const ToWeb = z.discriminatedUnion('kind', [
  z.object({
    kind: z.literal('state'),
    device: z.string().max(100),
    projects: z.array(WireProject).max(100),
    /** Some chats were left out to fit one frame. */
    more: z.boolean()
  }),
  z.object({
    kind: z.literal('chat.snapshot'),
    paneId: Id,
    items: z.array(WireItem).max(500),
    busy: z.boolean(),
    /** Older items were left out. */
    more: z.boolean(),
    /** The CLI's commands, once it has said (2.5.2+; older desktops leave it out). */
    commands: z.array(WireCommand).max(200).optional()
  }),
  /** Items that are new or changed since the last snapshot/update, by `id`. */
  z.object({
    kind: z.literal('chat.update'),
    paneId: Id,
    items: z.array(WireItem).max(500),
    busy: z.boolean(),
    /** Sent when the CLI's commands changed since the snapshot or the last update. */
    commands: z.array(WireCommand).max(200).optional()
  }),
  z.object({
    kind: z.literal('ack'),
    req: Req,
    ok: z.boolean(),
    error: z.string().max(500).optional(),
    /** `chat.new`: the chat it made. */
    paneId: Id.optional()
  }),
  /** The chat is gone (closed or deleted on the desktop). */
  z.object({ kind: z.literal('chat.gone'), paneId: Id })
])
export type ToWeb = z.infer<typeof ToWeb>

const Envelope = z.object({
  v: z.literal(RELAY_APP_VERSION),
  /** Grows with every frame in this direction (microseconds since 1970, or last + 1). */
  n: z.number().int().positive(),
  m: z.unknown()
})

/** Frames older than this are refused even with a growing `n` (a replay after a restart). */
export const MAX_AGE_MS = 10 * 60 * 1000

/** One direction's counter: `next()` for sending, `accept(n)` for receiving. */
export class Counter {
  constructor(public last = 0) {}

  next(now = Date.now()): number {
    this.last = Math.max(now * 1000, this.last + 1)
    return this.last
  }

  accept(n: number, now = Date.now()): boolean {
    if (n <= this.last || n < (now - MAX_AGE_MS) * 1000) return false
    this.last = n
    return true
  }
}

/** Seals one message for the other end. */
export function encode(ch: Channel, out: Counter, message: ToDesktop | ToWeb): string {
  return seal(
    ch.send,
    ch.pairingId,
    JSON.stringify({ v: RELAY_APP_VERSION, n: out.next(), m: message })
  )
}

/**
 * Opens and checks one sealed frame: null when it doesn't open, isn't v1, is stale or repeated,
 * or isn't a message `schema` knows.
 */
export function decode<T>(
  ch: Channel,
  incoming: Counter,
  sealed: string,
  schema: z.ZodType<T>
): T | null {
  const text = open(ch.receive, ch.pairingId, sealed)
  if (text === null) return null
  let raw: unknown
  try {
    raw = JSON.parse(text)
  } catch {
    return null
  }
  const env = Envelope.safeParse(raw)
  if (!env.success || !incoming.accept(env.data.n)) return null
  const msg = schema.safeParse(env.data.m)
  return msg.success ? msg.data : null
}

/** A pairing link: what the desktop gives the browser. The part after `#` never reaches a server. */
export const PairingLink = z.object({
  v: z.literal(1),
  server: z.url(),
  pairingId: z.string().regex(/^pair_[A-Za-z0-9_-]{1,64}$/),
  code: Key,
  desktopKey: Key,
  deviceName: z.string().min(1).max(100)
})
export type PairingLink = z.infer<typeof PairingLink>

const utf8 = new TextEncoder()

export function pairingLinkUrl(appUrl: string, link: PairingLink): string {
  const json = utf8.encode(JSON.stringify(link))
  let bin = ''
  for (const b of json) bin += String.fromCharCode(b)
  const b64 = btoa(bin).replace(/\+/g, '-').replace(/\//g, '_').replace(/=+$/, '')
  return `${appUrl.replace(/\/+$/, '')}/pair#${b64}`
}

/** The pairing in a link's `#…` part (or the whole link), or null. */
export function parsePairingLink(text: string): PairingLink | null {
  const hash = text.includes('#') ? text.slice(text.indexOf('#') + 1) : text
  if (!/^[A-Za-z0-9_-]{1,4000}$/.test(hash.trim())) return null
  try {
    const pad = hash.trim().replace(/-/g, '+').replace(/_/g, '/')
    const bin = atob(pad + '='.repeat((4 - (pad.length % 4)) % 4))
    const bytes = Uint8Array.from(bin, (c) => c.charCodeAt(0))
    const r = PairingLink.safeParse(JSON.parse(new TextDecoder().decode(bytes)))
    return r.success ? r.data : null
  } catch {
    return null
  }
}
