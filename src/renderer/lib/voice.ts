// Voice typing's pure parts (the hook is components/chat/useVoiceTyping.ts): where spoken text
// goes in the message box, and the labels the bar under the box shows.

/**
 * Puts `spoken` where the caret (or selection) is, with a space on each side only where words
 * would otherwise run together. Returns the new text and where the caret goes (after it).
 */
export function insertSpoken(
  text: string,
  start: number,
  end: number,
  spoken: string
): { text: string; caret: number } {
  const words = spoken.trim()
  const a = Math.max(0, Math.min(start, text.length))
  const b = Math.max(a, Math.min(end, text.length))
  if (!words) return { text, caret: b }
  const before = text.slice(0, a)
  const after = text.slice(b)
  const lead = before && !/\s$/.test(before) ? ' ' : ''
  const trail = after && !/^[\s.,!?;:)\]}]/.test(after) ? ' ' : ''
  const head = before + lead + words + trail
  return { text: head + after, caret: head.length }
}

/** 0:07, 1:30 */
export function clock(ms: number): string {
  const s = Math.max(0, Math.floor(ms / 1000))
  return `${Math.floor(s / 60)}:${String(s % 60).padStart(2, '0')}`
}

/** 190 MB */
export function megabytes(bytes: number): string {
  return `${Math.round(bytes / 1_000_000)} MB`
}

export function percent(done: number, total: number): number {
  return total > 0 ? Math.min(100, Math.floor((done / total) * 100)) : 0
}
