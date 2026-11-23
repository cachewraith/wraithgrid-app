// Git repository URLs for New project → Git repository and `wraithgrid://clone` links: the
// checks made before cloning (the same as app-rs/crates/git/src/clone.rs for the Tauri app;
// src/main/git.ts uses these), so the dialog can say what's wrong while typing, plus the
// folder name the clone will get.

export const CLONE_URL_MAX = 2000
export const BRANCH_MAX = 100

const HTTPS =
  /^https:\/\/(?:[^@/\s]+@)?[A-Za-z0-9][A-Za-z0-9.-]*(?::[0-9]{1,5})?\/[A-Za-z0-9._~%/+-]+$/
const SSH =
  /^ssh:\/\/(?:[A-Za-z0-9._-]+@)?[A-Za-z0-9][A-Za-z0-9.-]*(?::[0-9]{1,5})?\/[A-Za-z0-9._~%/+-]+$/
const SCP = /^[A-Za-z0-9._-]+@[A-Za-z0-9][A-Za-z0-9.-]*:[A-Za-z0-9._~/+][A-Za-z0-9._~%/+-]*$/
/** GitHub's `owner/repo` shorthand. */
const SHORTHAND = /^([A-Za-z0-9-]{1,39})\/([A-Za-z0-9._-]{1,100})$/

/** A remote https/ssh repository URL Wraithgrid will clone (main checks it again). */
export function validCloneUrl(url: string): boolean {
  return (
    url.length >= 1 &&
    url.length <= CLONE_URL_MAX &&
    !url.startsWith('-') &&
    !url.includes('::') &&
    (HTTPS.test(url) || SSH.test(url) || SCP.test(url))
  )
}

/**
 * What the user typed, as the URL to clone: trimmed, `owner/repo` → its GitHub https URL.
 * null when it isn't one we'd clone.
 */
export function normalizeCloneUrl(input: string): string | null {
  const url = input.trim()
  const [, owner, name] = SHORTHAND.exec(url) ?? []
  if (owner && name && name !== '.' && name !== '..')
    return `https://github.com/${owner}/${name.replace(/\.git$/, '')}.git`
  return validCloneUrl(url) ? url : null
}

/** The folder `git clone` makes: the last path part without `.git` (clone.rs `repo_dir_name`). */
export function repoDirName(url: string): string {
  const path = url.replace(/\/+$/, '')
  const last = path.split(/[/:]/).pop() ?? ''
  const base = last.endsWith('.git') ? last.slice(0, -4) : last
  const name = base
    .replace(/[^A-Za-z0-9._-]/g, '')
    .slice(0, 100)
    .replace(/^[.-]+/, '')
  return name || 'repo'
}

/** Branch names main accepts (`valid_branch_name` in the git crate). */
export function validBranch(name: string): boolean {
  return (
    name.length >= 1 &&
    name.length <= BRANCH_MAX &&
    /^[A-Za-z0-9._/-]+$/.test(name) &&
    !name.startsWith('-') &&
    !name.includes('..') &&
    !name.includes('//') &&
    !name.endsWith('.lock') &&
    !/[./]$/.test(name)
  )
}

/** A short `host/owner/repo` label for a repository URL (the dialog's summary line). */
export function repoLabel(url: string): string {
  const m =
    /^(?:https:\/\/(?:[^@/]+@)?|ssh:\/\/(?:[^@/]+@)?|[^@]+@)([^/:]+)(?::[0-9]+)?[/:](.+)$/.exec(url)
  if (!m?.[1] || !m[2]) return url
  return `${m[1]}/${m[2].replace(/\/+$/, '').replace(/\.git$/, '')}`
}
