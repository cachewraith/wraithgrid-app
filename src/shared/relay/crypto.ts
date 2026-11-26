// End-to-end encryption for the relay (web app ↔ this desktop through wraithgrid-server,
// which only routes ciphertext; spec: wraithgrid-protocol docs/RELAY-APP.md). The same file is
// copied into the web app (wraithgrid-web src/relay/crypto.ts): keep both identical, the test
// vectors in tests/unit/relay-crypto.test.ts pin the format.
//
// Keys: one X25519 pair per side per pairing. Each direction has its own key:
//   k = HKDF-SHA256(ikm = X25519(mine, theirs), salt = utf8(pairingId), info = utf8(LABEL + dir), 32)
// with dir `d2p` (desktop → paired peer, the web app) or `p2d`. A message is XChaCha20-Poly1305 with a random
// 24-byte nonce and the pairing id as associated data; on the wire: base64url(nonce ‖ sealed).
import { x25519 } from '@noble/curves/ed25519.js'
import { xchacha20poly1305 } from '@noble/ciphers/chacha.js'
import { randomBytes } from '@noble/ciphers/utils.js'
import { hkdf } from '@noble/hashes/hkdf.js'
import { sha256 } from '@noble/hashes/sha2.js'

export const LABEL = 'wraithgrid-relay-v1 '
export type Direction = 'd2p' | 'p2d'

/** 64 KiB of sealed bytes is the server's limit per frame (protocol `Ciphertext`). */
export const SEALED_MAX = 65_536
const NONCE = 24
const TAG = 16
/** The most plaintext one frame carries. */
export const PLAINTEXT_MAX = SEALED_MAX - NONCE - TAG

const enc = new TextEncoder()
const dec = new TextDecoder('utf-8', { fatal: true })

export function toB64u(bytes: Uint8Array): string {
  let bin = ''
  for (const b of bytes) bin += String.fromCharCode(b)
  return btoa(bin).replace(/\+/g, '-').replace(/\//g, '_').replace(/=+$/, '')
}

export function fromB64u(text: string): Uint8Array {
  if (!/^[A-Za-z0-9_-]*$/.test(text)) throw new Error('not base64url')
  const pad = text.replace(/-/g, '+').replace(/_/g, '/') + '='.repeat((4 - (text.length % 4)) % 4)
  const bin = atob(pad)
  const out = new Uint8Array(bin.length)
  for (let i = 0; i < bin.length; i++) out[i] = bin.charCodeAt(i)
  return out
}

export interface KeyPair {
  /** base64url, 32 bytes. Never leaves this device. */
  secretKey: string
  /** base64url, 32 bytes. */
  publicKey: string
}

export function newKeyPair(): KeyPair {
  const { secretKey, publicKey } = x25519.keygen()
  return { secretKey: toB64u(secretKey), publicKey: toB64u(publicKey) }
}

export function publicKeyOf(secretKey: string): string {
  return toB64u(x25519.getPublicKey(fromB64u(secretKey)))
}

/** The key for one direction of one pairing. Throws on a bad (e.g. low-order) peer key. */
export function directionKey(
  mySecret: string,
  theirPublic: string,
  pairingId: string,
  dir: Direction
): Uint8Array {
  const shared = x25519.getSharedSecret(fromB64u(mySecret), fromB64u(theirPublic))
  if (shared.every((b) => b === 0)) throw new Error('bad peer key')
  return hkdf(sha256, shared, enc.encode(pairingId), enc.encode(LABEL + dir), 32)
}

/** Seals a UTF-8 string; `nonce` only for test vectors. */
export function seal(
  key: Uint8Array,
  pairingId: string,
  plaintext: string,
  nonce: Uint8Array = randomBytes(NONCE)
): string {
  const pt = enc.encode(plaintext)
  if (pt.length > PLAINTEXT_MAX) throw new Error('message too big for one frame')
  const ct = xchacha20poly1305(key, nonce, enc.encode(pairingId)).encrypt(pt)
  const out = new Uint8Array(NONCE + ct.length)
  out.set(nonce)
  out.set(ct, NONCE)
  return toB64u(out)
}

/** Opens a sealed frame; null when it isn't ours or was changed on the way. */
export function open(key: Uint8Array, pairingId: string, sealed: string): string | null {
  try {
    const raw = fromB64u(sealed)
    if (raw.length < NONCE + TAG) return null
    const pt = xchacha20poly1305(key, raw.subarray(0, NONCE), enc.encode(pairingId)).decrypt(
      raw.subarray(NONCE)
    )
    return dec.decode(pt)
  } catch {
    return null
  }
}

/**
 * The 6-digit code one-click pairing shows on both screens: from the pairing id and both public
 * keys, so the two match only if the server passed both keys on unchanged.
 *   n = big-endian uint32 of SHA-256(utf8("wraithgrid-pair-code-v1|" + id + "|" + desktop + "|" + web))[0..4]
 *   code = (n mod 1 000 000), six digits, shown as "123 456"
 */
export function pairingCode(pairingId: string, desktopKey: string, webKey: string): string {
  const h = sha256(enc.encode(`wraithgrid-pair-code-v1|${pairingId}|${desktopKey}|${webKey}`))
  const [a = 0, b = 0, c = 0, d = 0] = h
  const n = (((a << 24) | (b << 16) | (c << 8) | d) >>> 0) % 1_000_000
  const six = String(n).padStart(6, '0')
  return `${six.slice(0, 3)} ${six.slice(3)}`
}

/** Both directions' keys for one side of a pairing. */
export interface Channel {
  pairingId: string
  send: Uint8Array
  receive: Uint8Array
}

export function channel(
  role: 'desktop' | 'web',
  mySecret: string,
  theirPublic: string,
  pairingId: string
): Channel {
  const out: Direction = role === 'desktop' ? 'd2p' : 'p2d'
  const back: Direction = role === 'desktop' ? 'p2d' : 'd2p'
  return {
    pairingId,
    send: directionKey(mySecret, theirPublic, pairingId, out),
    receive: directionKey(mySecret, theirPublic, pairingId, back)
  }
}
