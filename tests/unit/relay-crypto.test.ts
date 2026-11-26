// The relay's encryption and message envelope (src/shared/relay). The vectors pin the wire
// format: wraithgrid-web carries the same files and the same vectors, and they were checked
// against an independent implementation (Python cryptography: X25519, HKDF, HChaCha20 +
// ChaCha20-Poly1305).
import { describe, expect, it } from 'vitest'
import {
  channel,
  directionKey,
  newKeyPair,
  open,
  pairingCode,
  PLAINTEXT_MAX,
  publicKeyOf,
  seal,
  toB64u,
  fromB64u
} from '../../src/shared/relay/crypto'
import {
  Counter,
  decode,
  encode,
  MAX_AGE_MS,
  pairingLinkUrl,
  parsePairingLink,
  ToDesktop,
  ToWeb,
  type PairingLink
} from '../../src/shared/relay/messages'

const V = {
  dS: 'AQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQE',
  pS: 'AgICAgICAgICAgICAgICAgICAgICAgICAgICAgICAgI',
  dP: 'pOCSkrZRwni5dyxWn1-puxPZBrRqtoyd-dwrRAn4ogk',
  pP: 'zo060cy2M-x7cMF4FKXHbs0CloUFDTRHRboFhw5YfVk',
  d2p: 'BwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcH5ej3_eMRdAzo6SHQVwanVNc0H4AKnYpXSQ',
  p2d: 'BwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHZS982Wnk5FTxgDLpqZ7ZXWptOzKS07evJE4Bohk'
}
const PAIR = 'pair_vector'

describe('relay crypto', () => {
  it('matches the fixed vectors (wire format)', () => {
    expect(publicKeyOf(V.dS)).toBe(V.dP)
    expect(publicKeyOf(V.pS)).toBe(V.pP)
    const nonce = new Uint8Array(24).fill(7)
    const desktop = channel('desktop', V.dS, V.pP, PAIR)
    expect(seal(desktop.send, PAIR, 'hello web', nonce)).toBe(V.d2p)
    expect(seal(desktop.receive, PAIR, 'hello desktop', nonce)).toBe(V.p2d)
    // The phone side opens what the desktop sealed, and the other way round.
    const phone = channel('web', V.pS, V.dP, PAIR)
    expect(open(phone.receive, PAIR, V.d2p)).toBe('hello web')
    expect(open(desktop.receive, PAIR, V.p2d)).toBe('hello desktop')
  })

  it('derives the one-click pairing code from the id and both keys (vector)', () => {
    // Independently: Python hashlib over the same string.
    expect(pairingCode(PAIR, V.dP, V.pP)).toBe('369 759')
    // Any swapped key gives another code.
    expect(pairingCode(PAIR, V.pP, V.dP)).not.toBe('369 759')
    expect(pairingCode('pair_other', V.dP, V.pP)).toMatch(/^\d{3} \d{3}$/)
  })

  it('refuses a changed frame, another pairing, the wrong direction', () => {
    const d = channel('desktop', V.dS, V.pP, PAIR)
    const p = channel('web', V.pS, V.dP, PAIR)
    const sealed = seal(d.send, PAIR, 'hi')
    const raw = fromB64u(sealed)
    raw[raw.length - 1] = raw.at(-1)! ^ 1
    expect(open(p.receive, PAIR, toB64u(raw))).toBeNull()
    expect(open(p.receive, 'pair_other', sealed)).toBeNull()
    // A desktop frame reflected back to the desktop doesn't open.
    expect(open(d.receive, PAIR, sealed)).toBeNull()
    expect(open(p.receive, PAIR, 'not base64!')).toBeNull()
    expect(open(p.receive, PAIR, 'AAAA')).toBeNull()
  })

  it('makes fresh keys, refuses a low-order peer key and oversized messages', () => {
    const a = newKeyPair()
    expect(a.publicKey).toMatch(/^[A-Za-z0-9_-]{43}$/)
    expect(publicKeyOf(a.secretKey)).toBe(a.publicKey)
    expect(() => directionKey(a.secretKey, toB64u(new Uint8Array(32)), PAIR, 'd2p')).toThrow()
    const d = channel('desktop', V.dS, V.pP, PAIR)
    expect(() => seal(d.send, PAIR, 'x'.repeat(PLAINTEXT_MAX + 1))).toThrow()
  })
})

describe('relay messages', () => {
  const d = channel('desktop', V.dS, V.pP, PAIR)
  const p = channel('web', V.pS, V.dP, PAIR)

  it('carries a message once, in order; drops repeats, stale and unknown ones', () => {
    const out = new Counter()
    const inc = new Counter()
    const one = encode(p, out, { kind: 'sync' })
    const two = encode(p, out, { kind: 'chat.open', paneId: 'p1' })
    expect(decode(d, inc, one, ToDesktop)).toEqual({ kind: 'sync' })
    expect(decode(d, inc, two, ToDesktop)).toEqual({ kind: 'chat.open', paneId: 'p1' })
    // Replayed or reordered by the server: dropped.
    expect(decode(d, inc, two, ToDesktop)).toBeNull()
    expect(decode(d, inc, one, ToDesktop)).toBeNull()
    // Valid crypto, but not a message the desktop takes.
    const bad = encode(d, new Counter(), { kind: 'chat.gone', paneId: 'x' })
    expect(decode(p, new Counter(), bad, ToDesktop)).toBeNull()
    expect(decode(p, new Counter(), bad, ToWeb)).toEqual({ kind: 'chat.gone', paneId: 'x' })
  })

  it('counts up even within one millisecond, and refuses frames from long ago', () => {
    const c = new Counter()
    const a = c.next(1000)
    expect(c.next(1000)).toBe(a + 1)
    const r = new Counter()
    const now = 10 * MAX_AGE_MS
    expect(r.accept((now - MAX_AGE_MS - 1) * 1000, now)).toBe(false)
    expect(r.accept(now * 1000, now)).toBe(true)
  })

  it('round-trips a pairing link and refuses junk', () => {
    const link: PairingLink = {
      v: 1,
      server: 'https://api.wraithgrid.cachewraith.com',
      pairingId: 'pair_abc',
      code: V.dP,
      desktopKey: V.pP,
      deviceName: 'Work laptop'
    }
    const url = pairingLinkUrl('https://app.wraithgrid.cachewraith.com/', link)
    expect(url.startsWith('https://app.wraithgrid.cachewraith.com/pair#')).toBe(true)
    expect(parsePairingLink(url)).toEqual(link)
    expect(parsePairingLink(url.split('#')[1]!)).toEqual(link)
    expect(parsePairingLink('https://x/pair#nope')).toBeNull()
    expect(parsePairingLink('https://x/pair#' + btoa('{"v":2}'))).toBeNull()
  })
})
