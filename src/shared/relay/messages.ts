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
/** A tool's details (command, each side of a diff) past this are cut. */
export const PREVIEW_MAX = 6_000
/** Base64 characters of an image per `chat.image` frame. */
export const IMAGE_CHUNK = 45_000
/** Frames one image may take (about 6.7 MB of base64). */
export const IMAGE_PARTS_MAX = 150
/** Images one message may carry (the desktop's own limit). */
export const IMAGES_MAX = 4
export const IMAGE_TYPES = ['image/png', 'image/jpeg', 'image/gif', 'image/webp'] as const

/**
 * What a paired desktop can do beyond 2.5.0, in `state.features`; the web app offers a control
 * only when its desktop lists it. `icons`, `commands`: 2.5.2. The rest: 2.5.3.
 */
export const RELAY_FEATURES = [
  'icons',
  'commands',
  'answer',
  'settings',
  'images',
  'preview'
] as const
export type RelayFeature = (typeof RELAY_FEATURES)[number]

/** What a tool call ran, for its details on the web (the desktop's ToolPreview, capped). 2.5.3+ */
export const WirePreview = z.discriminatedUnion('type', [
  z.object({ type: z.literal('code'), text: z.string().max(PREVIEW_MAX) }),
  z.object({
    type: z.literal('diff'),
    path: z.string().max(1000),
    before: z.string().max(PREVIEW_MAX),
    after: z.string().max(PREVIEW_MAX)
  }),
  z.object({
    type: z.literal('todos'),
    items: z
      .array(z.object({ text: z.string().max(500), status: z.enum(['pending', 'active', 'done']) }))
      .max(100)
  })
])
export type WirePreview = z.infer<typeof WirePreview>

/** One of claude's AskUserQuestion questions, waiting for `chat.answer`. 2.5.3+ */
export const WireQuestion = z.object({
  question: z.string().max(1000),
  header: z.string().max(100),
  multiSelect: z.boolean(),
  options: z
    .array(z.object({ label: z.string().max(300), description: z.string().max(1000) }))
    .max(12)
})
export type WireQuestion = z.infer<typeof WireQuestion>

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
  level: z.enum(['info', 'error', 'divider']).optional(),
  /** A tool call's details: its command, diff or task list (2.5.3+). */
  preview: WirePreview.optional(),
  /** claude asks (AskUserQuestion) and waits: answer with `chat.answer {ask: id}` (2.5.3+). */
  ask: z.object({ id: Id, questions: z.array(WireQuestion).min(1).max(8) }).optional(),
  /** A user message's attached images, counted (2.5.3+). */
  images: z.number().int().min(0).max(20).optional()
})
export type WireItem = z.infer<typeof WireItem>

/**
 * A chat's model, effort and permission mode, and what the web may switch them to (2.5.3+).
 * `modes` never has Full access or Auto: those let it run tools without asking, and tool
 * approvals stay on the computer.
 */
export const WireSettings = z.object({
  /** null: the account's default. */
  model: z.string().max(100).nullable(),
  models: z
    .array(
      z.object({
        value: z.string().max(100),
        label: z.string().max(100),
        description: z.string().max(300),
        efforts: z.array(z.string().max(20)).max(10)
      })
    )
    .max(40),
  /** null: the CLI's default. */
  effort: z.string().max(20).nullable(),
  mode: z.string().max(40),
  modes: z.array(z.string().max(40)).max(10)
})
export type WireSettings = z.infer<typeof WireSettings>

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
  /** `images`: uploads sent before as `chat.image` frames (2.5.3+); then `text` may be empty. */
  z
    .object({
      kind: z.literal('chat.send'),
      req: Req,
      paneId: Id,
      text: z.string().trim().max(MESSAGE_MAX),
      images: z.array(Req).max(IMAGES_MAX).optional()
    })
    .refine((m) => m.text.length > 0 || !!m.images?.length, 'An empty message'),
  /** A new chat in a project, with the project's account; `text` is its first message. */
  z.object({
    kind: z.literal('chat.new'),
    req: Req,
    wsId: Id,
    text: z.string().trim().max(MESSAGE_MAX),
    images: z.array(Req).max(IMAGES_MAX).optional()
  }),
  z.object({ kind: z.literal('chat.stop'), req: Req, paneId: Id }),
  /** One frame of an image for a later `chat.send`/`chat.new` (`upload` names it). 2.5.3+ */
  z.object({
    kind: z.literal('chat.image'),
    upload: Req,
    part: z
      .number()
      .int()
      .min(0)
      .max(IMAGE_PARTS_MAX - 1),
    parts: z.number().int().min(1).max(IMAGE_PARTS_MAX),
    mediaType: z.enum(IMAGE_TYPES),
    data: z
      .string()
      .max(IMAGE_CHUNK)
      .regex(/^[A-Za-z0-9+/]*={0,2}$/)
  }),
  /**
   * Answers claude's questions (`WireItem.ask`), by question text; several picks joined with
   * ", ". `skip`: no answer (it goes on without). Nothing else can be approved this way. 2.5.3+
   */
  z
    .object({
      kind: z.literal('chat.answer'),
      req: Req,
      paneId: Id,
      ask: Id,
      answers: z.record(z.string().max(1000), z.string().trim().min(1).max(2000)).optional(),
      skip: z.literal(true).optional()
    })
    .refine(
      (m) =>
        m.skip
          ? m.answers === undefined
          : !!m.answers && Object.keys(m.answers).length >= 1 && Object.keys(m.answers).length <= 8,
      'Answers or skip'
    ),
  /** Switches the chat's model (null: default), effort (null: default) or mode. 2.5.3+ */
  z
    .object({
      kind: z.literal('chat.set'),
      req: Req,
      paneId: Id,
      model: z.string().max(100).nullable().optional(),
      effort: z.string().max(20).nullable().optional(),
      mode: z.string().max(40).optional()
    })
    .refine(
      (m) => [m.model, m.effort, m.mode].filter((v) => v !== undefined).length === 1,
      'One setting at a time'
    )
])
export type ToDesktop = z.infer<typeof ToDesktop>

/** From the desktop to the web app. */
export const ToWeb = z.discriminatedUnion('kind', [
  z.object({
    kind: z.literal('state'),
    device: z.string().max(100),
    projects: z.array(WireProject).max(100),
    /** Some chats were left out to fit one frame. */
    more: z.boolean(),
    /** What this desktop can do beyond 2.5.0 (`RELAY_FEATURES`; unknown ones are ignored). */
    features: z.array(z.string().max(40)).max(40).optional()
  }),
  z.object({
    kind: z.literal('chat.snapshot'),
    paneId: Id,
    items: z.array(WireItem).max(500),
    busy: z.boolean(),
    /** Older items were left out. */
    more: z.boolean(),
    /** The CLI's commands, once it has said (2.5.2+; older desktops leave it out). */
    commands: z.array(WireCommand).max(200).optional(),
    /** Model, effort and mode, and what the web may pick (2.5.3+). */
    settings: WireSettings.optional()
  }),
  /** Items that are new or changed since the last snapshot/update, by `id`. */
  z.object({
    kind: z.literal('chat.update'),
    paneId: Id,
    items: z.array(WireItem).max(500),
    busy: z.boolean(),
    /** Sent when the CLI's commands changed since the snapshot or the last update. */
    commands: z.array(WireCommand).max(200).optional(),
    /** Sent when the settings (or what may be picked) changed. */
    settings: WireSettings.optional()
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
