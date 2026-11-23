import { execFileSync, spawn } from 'node:child_process'
import fs from 'node:fs'
import os from 'node:os'
import path from 'node:path'
import { _electron as electron, expect, test, type ElectronApplication } from '@playwright/test'

// New project from a Git repository, typed in or from a `wraithgrid://clone` link (the
// website's "Open in Wraithgrid"). The https URL is mapped to a local bare repo by git's
// `insteadOf`, so nothing touches the network.
const ROOT = path.resolve(__dirname, '../..')
const MAIN = path.join(ROOT, 'out/main/index.js')
const FAKE_CLAUDE = path.join(ROOT, 'tests/fixtures/fake-claude.sh')
const URL = 'https://git.example.test/team/demo.git'
/** `WRAITHGRID_E2E_SHOTS=<dir>`: save screenshots of the dialog. */
const SHOTS = process.env.WRAITHGRID_E2E_SHOTS

test.skip(process.platform === 'win32', 'POSIX-only fixture')

function setup(): { tmp: string; home: string; userData: string; env: Record<string, string> } {
  const tmp = fs.mkdtempSync(path.join(os.tmpdir(), 'wraithgrid-clone-'))
  const home = path.join(tmp, 'home')
  const userData = path.join(tmp, 'userdata')
  const bare = path.join(tmp, 'demo.git')
  const work = path.join(tmp, 'work')
  const gitconfig = path.join(tmp, 'gitconfig')
  for (const d of [home, userData, work]) fs.mkdirSync(d, { recursive: true })
  fs.writeFileSync(
    gitconfig,
    `[url "${bare}"]\n\tinsteadOf = ${URL}\n[init]\n\tdefaultBranch = main\n[user]\n\tname = t\n\temail = t@t\n`
  )
  const env: Record<string, string> = {
    ...(process.env as Record<string, string>),
    HOME: home,
    WRAITHGRID_USER_DATA_DIR: userData,
    GIT_CONFIG_GLOBAL: gitconfig,
    GIT_CONFIG_NOSYSTEM: '1'
  }
  const git = (args: string[], cwd: string): void => {
    execFileSync('git', args, { cwd, env, stdio: 'ignore' })
  }
  git(['init', '--bare', bare], tmp)
  git(['init'], work)
  fs.writeFileSync(path.join(work, 'README.md'), '# demo\n')
  git(['add', '.'], work)
  git(['commit', '-m', 'first'], work)
  git(['push', bare, 'main'], work)
  fs.writeFileSync(
    path.join(userData, 'config.json'),
    JSON.stringify({
      version: 1,
      claudePath: FAKE_CLAUDE,
      accounts: [
        {
          id: 'a-work',
          name: 'work',
          configDir: '~/.wraithgrid/accounts/work',
          color: '#7c5cff',
          signedIn: true,
          imported: false
        }
      ],
      workspaces: [{ id: 'ws-main', name: 'main', panes: [], layout: null }],
      activeWorkspace: 'ws-main',
      recentFolders: [],
      settings: { checkUpdatesOnLaunch: false }
    })
  )
  return { tmp, home, userData, env }
}

const saved = (userData: string): { workspaces?: { name: string; folder: string }[] } | null => {
  try {
    return JSON.parse(fs.readFileSync(path.join(userData, 'config.json'), 'utf8'))
  } catch {
    return null
  }
}

const link = (q: Record<string, string>): string =>
  `wraithgrid://clone?${new URLSearchParams(q).toString()}`

test('New project from a Git repository: checks the URL, clones it, opens the first chat', async () => {
  const { tmp, home, userData, env } = setup()
  let app: ElectronApplication | null = null
  try {
    app = await electron.launch({ args: [MAIN], env })
    const win = await app.firstWindow()
    await win.waitForSelector('.app .tb')
    const side = win.getByRole('complementary', { name: 'Sidebar' })
    await side.getByRole('button', { name: 'New project', exact: true }).click()
    const dlg = win.getByRole('dialog', { name: 'New project' })
    await expect(dlg.getByRole('radio', { name: 'A folder on this computer' })).toBeChecked()
    await dlg.getByRole('radio', { name: 'A Git repository' }).click()

    // A local path is refused while typing; the button stays off.
    const repo = dlg.getByLabel('Repository', { exact: true })
    await repo.fill('/etc')
    await expect(dlg.locator('#npj-repo-h')).toContainText('Use an https:// or SSH address')
    await expect(dlg.getByRole('button', { name: 'Clone and create' })).toBeDisabled()

    // A good URL names the project and the folder it lands in.
    await repo.fill(URL)
    await expect(dlg.getByLabel('Name')).toHaveValue('demo')
    await expect(dlg.getByRole('button', { name: /^Clone into demo in ~/ })).toBeVisible()
    if (SHOTS) await dlg.screenshot({ path: path.join(SHOTS, 'new-project-git.png') })
    await dlg.getByRole('button', { name: 'Clone and create' }).click()

    await expect(dlg).toHaveCount(0, { timeout: 30_000 })
    expect(fs.existsSync(path.join(home, 'demo', 'README.md'))).toBe(true)
    await expect(side.locator('.ws-row', { hasText: 'demo' })).toBeVisible()
    await expect(win.locator('section.pane .chat-hello')).toBeVisible()
    await expect
      .poll(() => saved(userData)?.workspaces?.find((w) => w.name === 'demo') ?? null)
      .toMatchObject({ folder: '~/demo' })

    // Again into the same folder: a project name that exists is refused before cloning.
    await side.getByRole('button', { name: 'New project', exact: true }).click()
    await dlg.getByRole('radio', { name: 'A Git repository' }).click()
    await dlg.getByLabel('Repository', { exact: true }).fill(URL)
    await dlg.getByRole('button', { name: 'Clone and create' }).click()
    await expect(dlg.getByRole('alert')).toContainText('A project named demo exists.')
    // Another name: the existing clone of the same repo is used as it is.
    await dlg.getByLabel('Name').fill('demo two')
    await dlg.getByRole('button', { name: 'Clone and create' }).click()
    await expect(dlg).toHaveCount(0, { timeout: 30_000 })
    await expect
      .poll(() => saved(userData)?.workspaces?.find((w) => w.name === 'demo two') ?? null)
      .toMatchObject({ folder: '~/demo' })
  } finally {
    await app?.close()
    fs.rmSync(tmp, { recursive: true, force: true })
  }
})

test('A wraithgrid://clone link fills in New project and puts its question in the first chat, unsent', async () => {
  const { tmp, home, userData, env } = setup()
  let app: ElectronApplication | null = null
  try {
    const question = 'Explain what this repo does'
    app = await electron.launch({ args: [MAIN, link({ repo: URL, prompt: question })], env })
    const win = await app.firstWindow()
    await win.waitForSelector('.app .tb')

    // Nothing is cloned until Clone and create.
    const dlg = win.getByRole('dialog', { name: 'New project' })
    await expect(dlg.getByRole('radio', { name: 'A Git repository' })).toBeChecked()
    await expect(dlg.getByLabel('Repository', { exact: true })).toHaveValue(URL)
    await expect(dlg.getByLabel('Name')).toHaveValue('demo')
    await expect(dlg.locator('.np-ask')).toContainText(question)
    if (SHOTS) await dlg.screenshot({ path: path.join(SHOTS, 'new-project-link.png') })
    expect(fs.existsSync(path.join(home, 'demo'))).toBe(false)
    await dlg.getByRole('button', { name: 'Clone and create' }).click()
    await expect(dlg).toHaveCount(0, { timeout: 30_000 })

    // The question waits in the box; the chat has no message of the user's yet.
    const box = win.locator('section.pane').getByLabel('Message', { exact: true })
    await expect(box).toHaveValue(question)
    await expect(win.locator('section.pane .chat-hello')).toBeVisible()

    // A second launch with a link hands it to this window and exits.
    const electronBin = (await import('electron')).default as unknown as string
    const second = spawn(
      electronBin,
      [MAIN, link({ repo: 'git@git.example.test:team/other.git' })],
      {
        env,
        stdio: 'ignore'
      }
    )
    const code = await new Promise<number | null>((resolve) => second.on('exit', resolve))
    expect(code).toBe(0)
    await expect(dlg.getByLabel('Repository', { exact: true })).toHaveValue(
      'git@git.example.test:team/other.git'
    )
    await expect(dlg.getByLabel('Name')).toHaveValue('other')
    await dlg.getByRole('button', { name: 'Cancel' }).click()
    await expect(dlg).toHaveCount(0)
    expect(saved(userData)?.workspaces?.some((w) => w.name === 'other')).toBe(false)
  } finally {
    await app?.close()
    fs.rmSync(tmp, { recursive: true, force: true })
  }
})
