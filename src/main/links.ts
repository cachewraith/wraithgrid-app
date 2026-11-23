// `wraithgrid://` links for the Electron build (the Tauri app's are desktop-rs/src/ipc/links.rs):
// the latest one from argv, a second launch or macOS's open-url, kept until the renderer
// takes it (lib/deep-link.ts parses it there; a link only fills in a dialog).

const LINK_MAX = 8192

/** A link worth keeping: our scheme, bounded, one line. */
export function validLink(link: string): boolean {
  if (link.length > LINK_MAX || !/^wraithgrid:\/\//i.test(link)) return false
  // eslint-disable-next-line no-control-regex
  return !/[\u0000-\u001f\u007f]/.test(link)
}

/** The link among a launch's arguments, never the program itself. */
export function linkFromArgs(argv: readonly string[]): string | null {
  return argv.slice(1).find(validLink) ?? null
}
