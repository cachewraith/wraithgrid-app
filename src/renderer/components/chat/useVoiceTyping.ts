// Voice typing in the chat box: mic → main records → local Whisper → text handed to the box,
// never sent by itself. While recording, `live` is what's been heard so far (shown in the box as
// it's said); `heard` is the final text on stop. The first use offers the model's download.
import { useCallback, useEffect, useRef, useState } from 'react'
import type { VoiceApi } from '@shared/ipc-contract'
import { typeAhead } from '../../lib/voice'

/**
 * `target` typed out a character at a time (`typeAhead`), so the live words appear as if
 * someone were typing them while you talk rather than in jumps each time Whisper reads again.
 * A reading that changes earlier words changes them in place; only the new end is typed.
 */
export function useTypewriter(target: string): string {
  const [n, setN] = useState(0)
  const [still] = useState(() => matchMedia('(prefers-reduced-motion: reduce)').matches)
  const shown = useRef(0)
  // A new recording starts typing from nothing (React's reset-on-change, during render).
  if (!target && n !== 0) setN(0)
  useEffect(() => {
    shown.current = Math.min(shown.current, target.length)
    if (still || !target) return
    let raf = 0
    let last = performance.now()
    const tick = (now: number): void => {
      shown.current = typeAhead(shown.current, target.length, now - last)
      last = now
      setN(Math.floor(shown.current))
      if (shown.current < target.length) raf = requestAnimationFrame(tick)
    }
    raf = requestAnimationFrame(tick)
    return () => cancelAnimationFrame(raf)
  }, [target, still])
  return still ? target : target.slice(0, n)
}

export type VoiceState =
  | { kind: 'idle' }
  /** The model isn't downloaded: the bar offers it. */
  | { kind: 'offer'; bytes: number }
  | { kind: 'downloading'; done: number; total: number }
  | { kind: 'recording'; since: number }
  | { kind: 'transcribing' }
  | { kind: 'error'; message: string }

export interface VoiceTyping {
  state: VoiceState
  /** The last thing said (a new object each time), for the box to put in. */
  heard: { text: string } | null
  /** While recording: the words heard so far ('' until the first ones). */
  live: string
  /** 0–1 while recording. */
  level: number
  /** The mic button: start, or finish and write it down. */
  toggle(): void
  /** Esc: stop without writing anything; also closes the offer or an error. */
  cancel(): void
  download(): void
}

export function useVoiceTyping(api: VoiceApi | undefined): VoiceTyping | null {
  const [state, setStateNow] = useState<VoiceState>({ kind: 'idle' })
  const [level, setLevel] = useState(0)
  const [heard, setHeard] = useState<{ text: string } | null>(null)
  const [live, setLive] = useState('')
  // Read by the event callbacks; written with the state, never during render.
  const stateRef = useRef<VoiceState>({ kind: 'idle' })
  const setState = useCallback((s: VoiceState) => {
    stateRef.current = s
    setStateNow(s)
  }, [])

  const finish = useCallback(async () => {
    if (!api) return
    setState({ kind: 'transcribing' })
    setLevel(0)
    const r = await api.stop()
    setLive('')
    if (!r.ok) setState({ kind: 'error', message: r.error })
    else {
      setState({ kind: 'idle' })
      if (r.text) setHeard({ text: r.text })
    }
  }, [api, setState])

  const start = useCallback(async () => {
    if (!api) return
    setLive('')
    const r = await api.start()
    if (r.ok) setState({ kind: 'recording', since: Date.now() })
    else if (r.code === 'no_model') {
      const s = await api.status()
      setState(
        s.downloading
          ? { kind: 'downloading', done: 0, total: s.bytes }
          : { kind: 'offer', bytes: s.bytes }
      )
    } else setState({ kind: 'error', message: r.error })
  }, [api, setState])

  useEffect(() => {
    if (!api) return
    const offLevel = api.onLevel((l) => {
      if (stateRef.current.kind === 'recording') setLevel(l)
    })
    const offPartial = api.onPartial((text) => {
      if (stateRef.current.kind === 'recording') setLive(text)
    })
    const offProgress = api.onProgress(({ done, total }) => {
      if (stateRef.current.kind === 'downloading') setState({ kind: 'downloading', done, total })
    })
    // Hit the time limit: write down what was said so far.
    const offEnded = api.onEnded(() => {
      if (stateRef.current.kind === 'recording') void finish()
    })
    return () => {
      offLevel()
      offPartial()
      offProgress()
      offEnded()
    }
  }, [api, finish, setState])

  // A chat box going away mid-recording lets the microphone go.
  useEffect(
    () => () => {
      if (stateRef.current.kind === 'recording') void api?.cancel()
    },
    [api]
  )

  const download = useCallback(async () => {
    if (!api) return
    const total = stateRef.current.kind === 'offer' ? stateRef.current.bytes : 0
    setState({ kind: 'downloading', done: 0, total })
    const r = await api.download()
    if (!r.ok) {
      setState(/cancel/i.test(r.error) ? { kind: 'idle' } : { kind: 'error', message: r.error })
      return
    }
    // Downloaded: start listening right away, which is what the click was for.
    await start()
  }, [api, start, setState])

  const toggle = useCallback(() => {
    const k = stateRef.current.kind
    if (k === 'recording') void finish()
    else if (k === 'idle' || k === 'error') void start()
    else if (k === 'offer') void download()
  }, [finish, start, download])

  const cancel = useCallback(() => {
    const k = stateRef.current.kind
    if (k === 'recording') void api?.cancel()
    if (k === 'downloading') void api?.cancelDownload()
    if (k !== 'transcribing') {
      setState({ kind: 'idle' })
      setLevel(0)
      setLive('')
    }
  }, [api, setState])

  if (!api) return null
  return { state, heard, live, level, toggle, cancel, download: () => void download() }
}
