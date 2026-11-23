import { describe, expect, it } from 'vitest'
import {
  CLONE_URL_MAX,
  normalizeCloneUrl,
  repoDirName,
  repoLabel,
  validBranch,
  validCloneUrl
} from '../../src/shared/git-url'
import { parseDeepLink } from '../../src/renderer/lib/deep-link'

describe('validCloneUrl (same rules as clone.rs)', () => {
  it('accepts remote https and ssh forms', () => {
    for (const ok of [
      'https://github.com/cachewraith/wraithgrid.git',
      'https://gitlab.example.com:8443/group/sub/repo.git',
      'https://user:tok@github.com/o/r.git',
      'ssh://git@github.com/o/r.git',
      'ssh://git@host:2222/o/r',
      'git@github.com:o/r.git'
    ])
      expect(validCloneUrl(ok), ok).toBe(true)
  })
  it('refuses local paths, other transports and option-like values', () => {
    for (const bad of [
      '',
      '--upload-pack=touch /tmp/x',
      'file:///etc',
      '/home/me/repo',
      'ext::sh -c touch% /tmp/pwned',
      'http://github.com/o/r',
      'https://github.com',
      'https://github.com/o/r two',
      'git@github.com:-o/r',
      'ssh://-oProxyCommand/y',
      'git@-oProxyCommand:o/r',
      `https://h/${'a'.repeat(CLONE_URL_MAX)}`
    ])
      expect(validCloneUrl(bad), bad).toBe(false)
  })
})

describe('normalizeCloneUrl', () => {
  it('trims, and turns GitHub owner/repo into its https URL', () => {
    expect(normalizeCloneUrl('  git@github.com:o/r.git ')).toBe('git@github.com:o/r.git')
    expect(normalizeCloneUrl('cachewraith/wraithgrid')).toBe(
      'https://github.com/cachewraith/wraithgrid.git'
    )
    expect(normalizeCloneUrl('o/r.git')).toBe('https://github.com/o/r.git')
    expect(normalizeCloneUrl('o/..')).toBeNull()
    expect(normalizeCloneUrl('a/b/c')).toBeNull()
    expect(normalizeCloneUrl('file:///x')).toBeNull()
  })
})

describe('repoDirName and repoLabel', () => {
  it('names the folder like git clone', () => {
    expect(repoDirName('https://github.com/o/wraithgrid.git')).toBe('wraithgrid')
    expect(repoDirName('https://github.com/o/wraithgrid/')).toBe('wraithgrid')
    expect(repoDirName('git@github.com:o/my.repo.git')).toBe('my.repo')
    expect(repoDirName('https://h/o/.git')).toBe('repo')
  })
  it('labels a repo by host and path', () => {
    expect(repoLabel('https://github.com/o/r.git')).toBe('github.com/o/r')
    expect(repoLabel('git@gitlab.com:g/s/r.git')).toBe('gitlab.com/g/s/r')
    expect(repoLabel('ssh://git@host:2222/o/r')).toBe('host/o/r')
  })
})

describe('validBranch', () => {
  it('matches the git crate', () => {
    expect(validBranch('feat/x-1.2')).toBe(true)
    for (const bad of ['', '-x', 'a..b', 'a//b', 'x.lock', 'x/', 'x.', 'a b', 'a~1'])
      expect(validBranch(bad), bad).toBe(false)
  })
})

describe('parseDeepLink', () => {
  const link = (q: Record<string, string>): string =>
    `wraithgrid://clone?${new URLSearchParams(q).toString()}`

  it('reads a clone link with its branch and question', () => {
    expect(
      parseDeepLink(
        link({
          repo: 'https://github.com/o/r.git',
          branch: 'dev',
          prompt: 'Explain how auth works'
        })
      )
    ).toEqual({
      kind: 'clone',
      repo: 'https://github.com/o/r.git',
      branch: 'dev',
      prompt: 'Explain how auth works'
    })
    expect(parseDeepLink('WRAITHGRID://clone/?repo=o%2Fr')).toEqual({
      kind: 'clone',
      repo: 'https://github.com/o/r.git',
      branch: null,
      prompt: ''
    })
  })
  it('drops a bad branch, control characters and an overlong question', () => {
    const p = parseDeepLink(
      link({
        repo: 'o/r',
        branch: '--upload-pack=x',
        prompt: `hi\u0000\u001b[31m${'x'.repeat(5000)}`
      })
    )
    expect(p?.branch).toBeNull()
    expect(p?.prompt.startsWith('hi[31m')).toBe(true)
    expect(p?.prompt.length).toBe(4000)
  })
  it('keeps new lines in the question', () => {
    expect(parseDeepLink(link({ repo: 'o/r', prompt: 'a\nb' }))?.prompt).toBe('a\nb')
  })
  it('refuses other actions, schemes and repos', () => {
    expect(parseDeepLink('wraithgrid://open?repo=o/r')).toBeNull()
    expect(parseDeepLink('https://clone?repo=o/r')).toBeNull()
    expect(parseDeepLink(link({ repo: 'file:///etc' }))).toBeNull()
    expect(parseDeepLink(link({ repo: '--upload-pack=x' }))).toBeNull()
    expect(parseDeepLink('wraithgrid://clone')).toBeNull()
  })
})

describe('linkFromArgs (Electron main)', () => {
  it('finds a one-line wraithgrid link among the arguments, never the program', async () => {
    const { linkFromArgs, validLink } = await import('../../src/main/links')
    expect(linkFromArgs(['electron', 'out/main/index.js', 'wraithgrid://clone?repo=o%2Fr'])).toBe(
      'wraithgrid://clone?repo=o%2Fr'
    )
    expect(linkFromArgs(['wraithgrid://clone'])).toBeNull()
    expect(validLink('wraithgrid://clone\nx')).toBe(false)
    expect(validLink(`wraithgrid://${'a'.repeat(9000)}`)).toBe(false)
    expect(validLink('https://x')).toBe(false)
  })
})
