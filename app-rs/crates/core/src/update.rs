//! Updates from the public GitHub releases (`src/main/update-check.ts`, `update-install.ts`):
//! check for a newer version, download the file for this install's package type, verify it
//! against the release's `SHA256SUMS.txt`, and install it in place (#166 phase 6). URLs are
//! built here from the fixed repo and the asset name, never taken from config or the UI.
//! macOS builds are ad-hoc signed, so there the release page opens instead, as today.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::Value;
use sha2::{Digest, Sha256};

pub const RELEASES_REPO: &str = "cachewraith/wraithgrid";
const LATEST_URL: &str = "https://api.github.com/repos/cachewraith/wraithgrid/releases/latest";
const MAX_BODY_BYTES: usize = 1_000_000;
const MAX_NOTES: usize = 8000;
/// The PR list / full changelog the workflow appends; the app cuts the notes here.
pub const NOTES_FOOTER_MARKER: &str = "<!-- wraithgrid:footer -->";

// ---- Versions ------------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Version {
    pub major: u64,
    pub minor: u64,
    pub patch: u64,
    pub pre: Vec<String>,
}

/// `v1.2.3`, `1.2.3-beta.1`, `1.2.3+build` (build metadata is ignored).
pub fn parse_version(s: &str) -> Option<Version> {
    let s = s.trim();
    let s = s.strip_prefix('v').unwrap_or(s);
    let (core, build) = match s.split_once('+') {
        Some((c, b)) => (c, Some(b)),
        None => (s, None),
    };
    let ident_ok = |x: &str| {
        (1..=64).contains(&x.len())
            && x.chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-')
    };
    if build.is_some_and(|b| !ident_ok(b)) {
        return None;
    }
    let (nums, pre) = match core.split_once('-') {
        Some((n, p)) => (n, Some(p)),
        None => (core, None),
    };
    if pre.is_some_and(|p| !ident_ok(p)) {
        return None;
    }
    let parts: Vec<&str> = nums.split('.').collect();
    let num = |x: &str| {
        (1..=9)
            .contains(&x.len())
            .then(|| x.parse::<u64>().ok())
            .flatten()
            .filter(|_| x.chars().all(|c| c.is_ascii_digit()))
    };
    if parts.len() != 3 {
        return None;
    }
    Some(Version {
        major: num(parts[0])?,
        minor: num(parts[1])?,
        patch: num(parts[2])?,
        pre: pre
            .map(|p| p.split('.').map(String::from).collect())
            .unwrap_or_default(),
    })
}

/// Semver precedence; a release outranks its own prereleases (1.2.0 > 1.2.0-beta.1).
pub fn compare_versions(a: &Version, b: &Version) -> std::cmp::Ordering {
    use std::cmp::Ordering::*;
    let core = (a.major, a.minor, a.patch).cmp(&(b.major, b.minor, b.patch));
    if core != Equal {
        return core;
    }
    match (a.pre.is_empty(), b.pre.is_empty()) {
        (true, true) => return Equal,
        (true, false) => return Greater,
        (false, true) => return Less,
        _ => {}
    }
    for i in 0..a.pre.len().max(b.pre.len()) {
        let (Some(x), Some(y)) = (a.pre.get(i), b.pre.get(i)) else {
            return if a.pre.len() < b.pre.len() {
                Less
            } else {
                Greater
            };
        };
        if x == y {
            continue;
        }
        let digits = |s: &str| s.chars().all(|c| c.is_ascii_digit());
        return match (digits(x), digits(y)) {
            (true, true) => x
                .parse::<u64>()
                .unwrap_or(0)
                .cmp(&y.parse::<u64>().unwrap_or(0)),
            (true, false) => Less,
            (false, true) => Greater,
            _ => x.cmp(y),
        };
    }
    Equal
}

// ---- GitHub's answer -----------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq)]
pub struct ReleaseInfo {
    pub version: String,
    pub tag: String,
    /// Rebuilt from the tag, not trusted from the answer.
    pub url: String,
    pub notes: String,
    pub published_at: Option<String>,
    /// Asset file names in the release.
    pub assets: Vec<String>,
}

pub fn clean_release_body(body: &str, max: usize) -> String {
    let text = body.replace("\r\n", "\n");
    let cut = text
        .find(NOTES_FOOTER_MARKER)
        .map_or(text.as_str(), |i| &text[..i]);
    cut.trim().chars().take(max).collect()
}

pub fn release_page_url(tag: &str) -> String {
    let encoded: String = tag
        .bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || b"-_.!~*'()".contains(&b) {
                (b as char).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect();
    format!("https://github.com/{RELEASES_REPO}/releases/tag/{encoded}")
}

/// Validates GitHub's answer; drafts, odd tags and junk are None.
pub fn parse_release(raw: &Value) -> Option<ReleaseInfo> {
    let tag = raw.get("tag_name")?.as_str().filter(|t| t.len() <= 128)?;
    if raw.get("draft").and_then(Value::as_bool) == Some(true) {
        return None;
    }
    parse_version(tag)?;
    let assets = raw
        .get("assets")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|x| x.get("name")?.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();
    Some(ReleaseInfo {
        version: tag.trim_start_matches('v').to_string(),
        tag: tag.to_string(),
        url: release_page_url(tag),
        notes: clean_release_body(
            raw.get("body").and_then(Value::as_str).unwrap_or(""),
            MAX_NOTES,
        ),
        published_at: raw
            .get("published_at")
            .and_then(Value::as_str)
            .filter(|p| p.len() <= 64)
            .map(String::from),
        assets,
    })
}

#[derive(Clone, Debug, PartialEq)]
pub enum CheckResult {
    Available {
        current: String,
        latest: ReleaseInfo,
    },
    Current {
        current: String,
        latest: ReleaseInfo,
    },
    Error {
        current: String,
        error: String,
    },
}

/// What the HTTP layer answers: status and body, or a transport failure.
pub enum Fetched {
    Response { status: u16, body: Vec<u8> },
    TimedOut,
    Failed,
}

/// Asks GitHub for the latest release and compares it with `current`.
pub fn check(current: &str, fetch: &dyn Fn(&str) -> Fetched) -> CheckResult {
    let error = |e: &str| CheckResult::Error {
        current: current.into(),
        error: e.into(),
    };
    let body = match fetch(LATEST_URL) {
        Fetched::TimedOut => return error("GitHub did not answer in time."),
        Fetched::Failed => return error("Could not reach GitHub. Check your connection."),
        Fetched::Response { status: 404, .. } => {
            return error("No release has been published yet.")
        }
        Fetched::Response {
            status: 403 | 429, ..
        } => return error("GitHub is rate-limiting update checks. Try again in a while."),
        Fetched::Response { status, .. } if !(200..300).contains(&status) => {
            return error(&format!("GitHub answered with status {status}."))
        }
        Fetched::Response { body, .. } => body,
    };
    if body.len() > MAX_BODY_BYTES {
        return error("GitHub sent an unexpected answer.");
    }
    let Some(latest) = serde_json::from_slice::<Value>(&body)
        .ok()
        .as_ref()
        .and_then(parse_release)
    else {
        return error("GitHub sent an unexpected answer.");
    };
    let (Some(theirs), Some(mine)) = (parse_version(&latest.version), parse_version(current))
    else {
        return error("GitHub sent an unexpected answer.");
    };
    if compare_versions(&theirs, &mine).is_gt() {
        CheckResult::Available {
            current: current.into(),
            latest,
        }
    } else {
        CheckResult::Current {
            current: current.into(),
            latest,
        }
    }
}

// ---- Which file to install -----------------------------------------------------------------

/// How this copy was installed, which decides the file and the install step.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PackageKind {
    AppImage,
    Deb,
    Rpm,
    Pacman,
    /// The Windows installer.
    Nsis,
    /// macOS (ad-hoc signed) or a build from source: the release page opens instead.
    Manual,
}

/// The NSIS uninstaller beside `Wraithgrid.exe`, named as electron-builder names it (both
/// the Electron installer and `app-rs/packaging/windows` write it).
pub const NSIS_UNINSTALLER: &str = "Uninstall Wraithgrid.exe";

/// The packaging writes `package-type` (deb, rpm, pacman) beside the binary, as
/// electron-builder does; an AppImage is recognised by `$APPIMAGE`, an NSIS install by its
/// uninstaller.
pub fn package_kind(exe: &Path, env: impl Fn(&str) -> Option<String>) -> PackageKind {
    if cfg!(target_os = "macos") {
        return PackageKind::Manual;
    }
    if cfg!(windows) {
        return if exe
            .parent()
            .is_some_and(|d| d.join(NSIS_UNINSTALLER).is_file())
        {
            PackageKind::Nsis
        } else {
            PackageKind::Manual
        };
    }
    if env("APPIMAGE").is_some_and(|p| !p.is_empty()) {
        return PackageKind::AppImage;
    }
    let marker = exe
        .parent()
        .map(|d| d.join("package-type"))
        .and_then(|f| std::fs::read_to_string(f).ok())
        .unwrap_or_default();
    match marker.trim() {
        "deb" => PackageKind::Deb,
        "rpm" => PackageKind::Rpm,
        "pacman" => PackageKind::Pacman,
        _ => PackageKind::Manual,
    }
}

/// The release file for a package kind, named as the existing releases name them.
pub fn asset_name(kind: PackageKind, version: &str, arm: bool) -> Option<String> {
    Some(match kind {
        PackageKind::Deb => format!(
            "wraithgrid-{version}-{}.deb",
            if arm { "arm64" } else { "amd64" }
        ),
        PackageKind::Rpm => format!(
            "wraithgrid-{version}-{}.rpm",
            if arm { "aarch64" } else { "x86_64" }
        ),
        PackageKind::Pacman => format!(
            "wraithgrid-{version}-{}.pacman",
            if arm { "aarch64" } else { "x64" }
        ),
        PackageKind::AppImage => format!(
            "Wraithgrid-{version}-{}.AppImage",
            if arm { "arm64" } else { "x86_64" }
        ),
        PackageKind::Nsis => format!(
            "Wraithgrid-Setup-{version}-{}.exe",
            if arm { "arm64" } else { "x64" }
        ),
        PackageKind::Manual => return None,
    })
}

pub fn download_url(tag: &str, asset: &str) -> String {
    format!("https://github.com/{RELEASES_REPO}/releases/download/{tag}/{asset}")
}

/// The hash `sha256sum` printed for `file` in a SHA256SUMS.txt, lower case.
pub fn expected_sha256(sums: &str, file: &str) -> Option<String> {
    sums.lines().find_map(|line| {
        let (hash, name) = line.trim().split_once(char::is_whitespace)?;
        let name = name.trim().trim_start_matches('*');
        (name == file && hash.len() == 64 && hash.chars().all(|c| c.is_ascii_hexdigit()))
            .then(|| hash.to_lowercase())
    })
}

pub fn sha256_file(path: &Path) -> std::io::Result<String> {
    let mut f = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 1 << 16];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect())
}

// ---- Downloading and installing ------------------------------------------------------------

#[derive(Clone, Debug, PartialEq)]
pub enum Progress {
    Checking,
    Downloading { version: String, percent: u8 },
    Ready { version: String },
    Installing { version: String },
}

pub const MANUAL: &str =
    "This copy of Wraithgrid can’t update itself. Download the new version instead.";

/// Downloads `url` to `dest`, reporting percent; a `cancel` that returns true stops it.
pub fn download(
    url: &str,
    dest: &Path,
    on_progress: &mut dyn FnMut(u8),
    cancel: &dyn Fn() -> bool,
) -> Result<(), String> {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(600)))
        .build()
        .into();
    let response = agent.get(url).call().map_err(|e| e.to_string())?;
    let total: Option<u64> = response
        .headers()
        .get("content-length")
        .and_then(|v| v.to_str().ok()?.parse().ok());
    let mut reader = response.into_body().into_reader();
    let mut file = std::fs::File::create(dest).map_err(|e| e.to_string())?;
    let mut buf = vec![0u8; 1 << 16];
    let mut done: u64 = 0;
    loop {
        if cancel() {
            return Err("cancelled".into());
        }
        let n = reader.read(&mut buf).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        std::io::Write::write_all(&mut file, &buf[..n]).map_err(|e| e.to_string())?;
        done += n as u64;
        if let Some(t) = total.filter(|t| *t > 0) {
            on_progress((done * 100 / t).min(100) as u8);
        }
    }
    Ok(())
}

/// Fetches a small text file (SHA256SUMS.txt) or the API answer.
pub fn fetch(url: &str, user_agent: &str) -> Fetched {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(10)))
        .http_status_as_error(false)
        .build()
        .into();
    match agent
        .get(url)
        .header("Accept", "application/vnd.github+json")
        .header("X-GitHub-Api-Version", "2022-11-28")
        .header("User-Agent", user_agent)
        .call()
    {
        Ok(mut r) => {
            let status = r.status().as_u16();
            let body = r
                .body_mut()
                .with_config()
                .limit(MAX_BODY_BYTES as u64 + 1)
                .read_to_vec()
                .unwrap_or_default();
            Fetched::Response { status, body }
        }
        Err(ureq::Error::Timeout(_)) => Fetched::TimedOut,
        Err(_) => Fetched::Failed,
    }
}

/// Downloads the release's file for this install, checks it against SHA256SUMS.txt, and
/// returns where it is. Nothing runs before the hash matches.
pub fn fetch_update(
    release: &ReleaseInfo,
    kind: PackageKind,
    dir: &Path,
    user_agent: &str,
    on_progress: &mut dyn FnMut(u8),
    cancel: &dyn Fn() -> bool,
) -> Result<PathBuf, String> {
    let asset = asset_name(kind, &release.version, cfg!(target_arch = "aarch64")).ok_or(MANUAL)?;
    if !release.assets.is_empty() && !release.assets.contains(&asset) {
        return Err(format!(
            "This release has no {asset}. Download the new version instead."
        ));
    }
    let sums = match fetch(&download_url(&release.tag, "SHA256SUMS.txt"), user_agent) {
        Fetched::Response {
            status: 200..=299,
            body,
        } => String::from_utf8_lossy(&body).into_owned(),
        _ => return Err("Could not reach GitHub to fetch the update.".into()),
    };
    let expected = expected_sha256(&sums, &asset)
        .ok_or("The download failed or did not match its checksum.")?;
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let dest = dir.join(&asset);
    download(
        &download_url(&release.tag, &asset),
        &dest,
        on_progress,
        cancel,
    )
    .map_err(|e| {
        if e == "cancelled" {
            e
        } else {
            "The download failed or did not match its checksum.".into()
        }
    })?;
    let actual = sha256_file(&dest).map_err(|e| e.to_string())?;
    if actual != expected {
        let _ = std::fs::remove_file(&dest);
        return Err("The download failed or did not match its checksum.".into());
    }
    Ok(dest)
}

/// The command that installs a downloaded file (system packages ask for the password through
/// pkexec, as electron-updater does). An AppImage is swapped in place instead (`install_appimage`).
pub fn install_command(kind: PackageKind, file: &Path) -> Option<(String, Vec<String>)> {
    let f = file.to_string_lossy().into_owned();
    let pk = |args: &[&str]| {
        Some((
            "pkexec".to_string(),
            args.iter()
                .map(|s| s.to_string())
                .chain([f.clone()])
                .collect(),
        ))
    };
    match kind {
        PackageKind::Deb => pk(&["dpkg", "-i"]),
        PackageKind::Rpm => pk(&["rpm", "-U", "--replacepkgs", "--replacefiles"]),
        PackageKind::Pacman => pk(&["pacman", "-U", "--noconfirm"]),
        // The NSIS installer updates silently and starts the app again.
        PackageKind::Nsis => Some((
            f,
            vec!["/S".into(), "--updated".into(), "--force-run".into()],
        )),
        PackageKind::AppImage | PackageKind::Manual => None,
    }
}

/// Replaces the running AppImage file with the downloaded one (same path, executable).
pub fn install_appimage(downloaded: &Path, appimage: &Path) -> std::io::Result<()> {
    let staged = appimage.with_extension("AppImage.new");
    std::fs::copy(downloaded, &staged)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&staged, std::fs::Permissions::from_mode(0o755))?;
    }
    std::fs::rename(&staged, appimage)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::cmp::Ordering::*;

    fn v(s: &str) -> Version {
        parse_version(s).unwrap()
    }

    #[test]
    fn versions() {
        assert_eq!(
            v("v1.26.0"),
            Version {
                major: 1,
                minor: 26,
                patch: 0,
                pre: vec![]
            }
        );
        assert_eq!(v("2.0.0-beta.3+abc").pre, vec!["beta", "3"]);
        assert!(
            parse_version("1.2").is_none()
                && parse_version("1.2.3.4").is_none()
                && parse_version("x1.2.3").is_none()
        );
        assert_eq!(compare_versions(&v("1.2.0"), &v("1.2.0-beta.1")), Greater);
        assert_eq!(
            compare_versions(&v("1.2.0-beta.2"), &v("1.2.0-beta.10")),
            Less
        );
        assert_eq!(
            compare_versions(&v("1.2.0-alpha"), &v("1.2.0-alpha.1")),
            Less
        );
        assert_eq!(compare_versions(&v("1.2.0-1"), &v("1.2.0-a")), Less);
        assert_eq!(compare_versions(&v("1.10.0"), &v("1.9.9")), Greater);
        assert_eq!(compare_versions(&v("1.2.3+x"), &v("v1.2.3")), Equal);
    }

    #[test]
    fn releases() {
        let r = parse_release(&json!({
            "tag_name": "v2.0.0", "html_url": "https://evil.example", "body": "notes\n<!-- wraithgrid:footer -->\nPRs",
            "published_at": "2026-12-01T00:00:00Z", "assets": [{ "name": "SHA256SUMS.txt" }]
        }))
        .unwrap();
        assert_eq!(
            r.url,
            "https://github.com/cachewraith/wraithgrid/releases/tag/v2.0.0"
        );
        assert_eq!(r.notes, "notes");
        assert_eq!(r.assets, vec!["SHA256SUMS.txt"]);
        assert!(parse_release(&json!({ "tag_name": "v2.0.0", "draft": true })).is_none());
        assert!(parse_release(&json!({ "tag_name": "latest" })).is_none());
        let long =
            parse_release(&json!({ "tag_name": "1.0.0", "body": "x".repeat(9000) })).unwrap();
        assert_eq!(long.notes.len(), MAX_NOTES);
    }

    #[test]
    fn checking() {
        let answer = |body: Value| {
            move |_: &str| Fetched::Response {
                status: 200,
                body: body.to_string().into_bytes(),
            }
        };
        assert!(matches!(
            check("1.26.0", &answer(json!({ "tag_name": "v2.0.0" }))),
            CheckResult::Available { .. }
        ));
        assert!(matches!(
            check("2.0.0", &answer(json!({ "tag_name": "v2.0.0" }))),
            CheckResult::Current { .. }
        ));
        // The final release is newer than a prerelease build of it.
        assert!(matches!(
            check("2.0.0-beta.1", &answer(json!({ "tag_name": "v2.0.0" }))),
            CheckResult::Available { .. }
        ));
        let err = |f: Fetched| match check("1.0.0", &move |_| match &f {
            Fetched::Response { status, body } => Fetched::Response {
                status: *status,
                body: body.clone(),
            },
            Fetched::TimedOut => Fetched::TimedOut,
            Fetched::Failed => Fetched::Failed,
        }) {
            CheckResult::Error { error, .. } => error,
            other => panic!("{other:?}"),
        };
        assert_eq!(
            err(Fetched::Failed),
            "Could not reach GitHub. Check your connection."
        );
        assert_eq!(err(Fetched::TimedOut), "GitHub did not answer in time.");
        assert_eq!(
            err(Fetched::Response {
                status: 404,
                body: vec![]
            }),
            "No release has been published yet."
        );
        assert_eq!(
            err(Fetched::Response {
                status: 429,
                body: vec![]
            }),
            "GitHub is rate-limiting update checks. Try again in a while."
        );
        assert_eq!(
            err(Fetched::Response {
                status: 200,
                body: b"junk".to_vec()
            }),
            "GitHub sent an unexpected answer."
        );
    }

    #[test]
    fn assets_and_sums() {
        assert_eq!(
            asset_name(PackageKind::Deb, "1.26.0", false).unwrap(),
            "wraithgrid-1.26.0-amd64.deb"
        );
        assert_eq!(
            asset_name(PackageKind::Pacman, "1.26.0", false).unwrap(),
            "wraithgrid-1.26.0-x64.pacman"
        );
        assert_eq!(
            asset_name(PackageKind::AppImage, "1.26.0", false).unwrap(),
            "Wraithgrid-1.26.0-x86_64.AppImage"
        );
        assert_eq!(
            asset_name(PackageKind::Nsis, "1.26.0", false).unwrap(),
            "Wraithgrid-Setup-1.26.0-x64.exe"
        );
        assert_eq!(asset_name(PackageKind::Manual, "1.26.0", false), None);
        let hash = "a".repeat(64);
        let sums = format!(
            "{hash}  wraithgrid-1.26.0-amd64.deb\n{}  *Wraithgrid-1.26.0-x86_64.AppImage\n",
            "B".repeat(64)
        );
        assert_eq!(
            expected_sha256(&sums, "wraithgrid-1.26.0-amd64.deb"),
            Some(hash)
        );
        assert_eq!(
            expected_sha256(&sums, "Wraithgrid-1.26.0-x86_64.AppImage"),
            Some("b".repeat(64))
        );
        assert_eq!(expected_sha256(&sums, "other"), None);
        assert_eq!(
            install_command(PackageKind::Pacman, Path::new("/t/w.pacman"))
                .unwrap()
                .1,
            vec!["pacman", "-U", "--noconfirm", "/t/w.pacman"]
        );
    }

    #[test]
    fn hashing_and_appimage_swap() {
        let dir = std::env::temp_dir().join(format!("wg-update-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("x");
        std::fs::write(&f, "hello").unwrap();
        assert_eq!(
            sha256_file(&f).unwrap(),
            "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"
        );
        let app = dir.join("Wraithgrid.AppImage");
        std::fs::write(&app, "old").unwrap();
        install_appimage(&f, &app).unwrap();
        assert_eq!(std::fs::read_to_string(&app).unwrap(), "hello");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn package_kinds() {
        let dir = std::env::temp_dir().join(format!("wg-pkg-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let exe = dir.join("wraithgrid");
        if cfg!(target_os = "linux") {
            assert_eq!(package_kind(&exe, |_| None), PackageKind::Manual);
            std::fs::write(dir.join("package-type"), "pacman\n").unwrap();
            assert_eq!(package_kind(&exe, |_| None), PackageKind::Pacman);
            assert_eq!(
                package_kind(&exe, |k| (k == "APPIMAGE").then(|| "/x.AppImage".into())),
                PackageKind::AppImage
            );
        }
        if cfg!(windows) {
            assert_eq!(package_kind(&exe, |_| None), PackageKind::Manual);
            std::fs::write(dir.join(NSIS_UNINSTALLER), "").unwrap();
            assert_eq!(package_kind(&exe, |_| None), PackageKind::Nsis);
        }
        let _ = std::fs::remove_dir_all(dir);
    }
}
