// New project → Git repository (src/main/git.ts cloneRepo) against real git: an https URL
// that git's `insteadOf` maps to a local bare repo, so no network is needed.
import { execFile } from 'node:child_process'
import { existsSync, writeFileSync } from 'node:fs'
import { mkdir, mkdtemp, realpath, rm, writeFile } from 'node:fs/promises'
import os from 'node:os'
import path from 'node:path'
import { afterEach, beforeEach, describe, expect, it } from 'vitest'
import { cloneRepo, type GitRunner } from '../../src/main/git'

const URL = 'https://git.example.test/team/demo.git'

let tmp = ''
let gitconfig = ''

const git =
  (extra: Record<string, string> = {}): GitRunner =>
  (args, cwd) =>
    new Promise((resolve, reject) => {
      execFile(
        'git',
        args,
        {
          cwd,
          env: {
            ...process.env,
            GIT_AUTHOR_NAME: 't',
            GIT_AUTHOR_EMAIL: 't@t',
            GIT_COMMITTER_NAME: 't',
            GIT_COMMITTER_EMAIL: 't@t',
            GIT_CONFIG_NOSYSTEM: '1',
            GIT_CONFIG_GLOBAL: gitconfig,
            GIT_TERMINAL_PROMPT: '0',
            LC_ALL: 'C',
            ...extra
          }
        },
        (err, stdout, stderr) =>
          err ? reject(Object.assign(err, { stderr })) : resolve(String(stdout))
      )
    })

beforeEach(async () => {
  tmp = await realpath(await mkdtemp(path.join(os.tmpdir(), 'wg-clone-')))
  const bare = path.join(tmp, 'demo.git')
  const work = path.join(tmp, 'work')
  gitconfig = path.join(tmp, 'gitconfig')
  // Not os.devNull (Windows refuses \\.\nul); the bare repo answers for the https URL.
  writeFileSync(
    gitconfig,
    `[url "${bare.replace(/\\/g, '/')}"]\n\tinsteadOf = ${URL}\n[init]\n\tdefaultBranch = main\n`
  )
  const run = git()
  await run(['init', '--bare', bare], tmp)
  await mkdir(work)
  await run(['init'], work)
  await writeFile(path.join(work, 'README.md'), '# demo\n')
  await run(['add', '.'], work)
  await run(['commit', '-m', 'first'], work)
  await run(['branch', 'dev'], work)
  await run(['push', bare, 'main', 'dev'], work)
  await mkdir(path.join(tmp, 'projects'))
}, 30_000)

afterEach(async () => {
  await rm(tmp, { recursive: true, force: true, maxRetries: 5 })
})

describe('cloneRepo', () => {
  it('clones into parent/<repo name>, on a branch when asked', async () => {
    const parent = path.join(tmp, 'projects')
    const r = await cloneRepo(git(), parent, URL, 'dev')
    expect(r).toEqual({ ok: true, dir: path.join(parent, 'demo'), existing: false })
    expect(existsSync(path.join(parent, 'demo', 'README.md'))).toBe(true)
    const head = await git()(['rev-parse', '--abbrev-ref', 'HEAD'], path.join(parent, 'demo'))
    expect(head.trim()).toBe('dev')
  }, 30_000)

  it('reuses a clone of the same repository and refuses any other folder', async () => {
    const parent = path.join(tmp, 'projects')
    expect((await cloneRepo(git(), parent, URL, null)).ok).toBe(true)
    // Same repo, said differently: used as it is.
    expect(await cloneRepo(git(), parent, `${URL.slice(0, -4)}/`, null)).toEqual({
      ok: true,
      dir: path.join(parent, 'demo'),
      existing: true
    })
    await mkdir(path.join(parent, 'other'))
    const r = await cloneRepo(git(), parent, 'git@git.example.test:x/other.git', null)
    expect(r).toEqual({
      ok: false,
      error: `other already exists in ${parent}. Pick another folder.`
    })
  }, 30_000)

  it('says what went wrong: a missing branch, a missing parent, a bad URL', async () => {
    const parent = path.join(tmp, 'projects')
    const r = await cloneRepo(git(), parent, URL, 'nope')
    expect(r).toEqual({ ok: false, error: 'Remote branch nope not found in upstream origin' })
    expect(existsSync(path.join(parent, 'demo'))).toBe(false)
    expect(await cloneRepo(git(), path.join(tmp, 'missing'), URL, null)).toEqual({
      ok: false,
      error: `Folder not found: ${path.join(tmp, 'missing')}`
    })
    for (const bad of [path.join(tmp, 'demo.git'), 'file:///etc', '--upload-pack=x', 'ext::sh'])
      expect((await cloneRepo(git(), parent, bad, null)).ok).toBe(false)
    expect(await cloneRepo(git(), parent, URL, '-b')).toEqual({
      ok: false,
      error: 'Not a valid branch name.'
    })
  }, 30_000)
})
