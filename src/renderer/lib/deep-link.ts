// `wraithgrid://` links (the website's "Open in Wraithgrid"), parsed. Anyone can make one, so
// a link only fills in a dialog the user confirms: the repo is cloned once they press Clone,
// and its question lands in the first chat's message box, never sent by itself.
import { normalizeCloneUrl, validBranch } from '@shared/git-url'

export const LINK_PROMPT_MAX = 4000

export type DeepLink = {
  kind: 'clone'
  repo: string
  branch: string | null
  /** Put in the first chat's box ('' = none). */
  prompt: string
}

/** `wraithgrid://clone?repo=<url>[&branch=<b>][&prompt=<text>]`; null for anything else. */
export function parseDeepLink(link: string): DeepLink | null {
  const m = /^wraithgrid:\/\/([^?#]*)(?:\?([^#]*))?/i.exec(link.trim())
  if (!m) return null
  const action = (m[1] ?? '').replace(/\/+$/, '').toLowerCase()
  if (action !== 'clone') return null
  let q: URLSearchParams
  try {
    q = new URLSearchParams(m[2] ?? '')
  } catch {
    return null
  }
  const repo = normalizeCloneUrl(q.get('repo') ?? '')
  if (!repo) return null
  const b = (q.get('branch') ?? '').trim()
  const prompt = (q.get('prompt') ?? '')
    // eslint-disable-next-line no-control-regex
    .replace(/[\u0000-\u0008\u000b-\u001f\u007f]/g, '')
    .trim()
    .slice(0, LINK_PROMPT_MAX)
  return { kind: 'clone', repo, branch: b && validBranch(b) ? b : null, prompt }
}
