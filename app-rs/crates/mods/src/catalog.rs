//! The Mods page's lists (`lib/mod-catalog.ts`): every claude account's installed mods merged
//! into one list (a mod is meant to be in every account), the catalog searched by name and
//! description with one row per mod name, and Download / Update for a community mod that is a
//! bare plugin, kept as a git clone in `~/.wraithgrid/mods/<name>` (the modsClone / modsPull
//! handlers of `ipc.ts`).

use std::path::Path;
use std::process::{Command, Stdio};

use crate::mod_manager::AccountMods;
use crate::mods::{last_line, CatalogMod, Env, ModInfo, GITHUB_REPO, MARKETPLACE_NAME};
use crate::util::contract_home;

/// One installed mod across the accounts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MergedMod {
    /// As one account lists it; `enabled` only when it is on everywhere it is installed.
    pub info: ModInfo,
    /// Accounts that have it, and the ones among them where it is on.
    pub accounts: Vec<String>,
    pub on: Vec<String>,
    /// Accounts that answered and lack it.
    pub missing: Vec<String>,
}

/// The accounts' lists as one, a row per mod (id, scope and project), in first-seen order.
pub fn merge_mods(results: &[AccountMods]) -> Vec<MergedMod> {
    let answered: Vec<(&String, &Vec<ModInfo>)> = results
        .iter()
        .filter_map(|r| r.mods.as_ref().ok().map(|m| (&r.account_id, m)))
        .collect();
    let mut rows: Vec<(String, MergedMod)> = Vec::new();
    for (account, mods) in &answered {
        for m in mods.iter() {
            let key = format!(
                "{}|{}|{}",
                m.id,
                m.scope,
                m.project_path.as_deref().unwrap_or("")
            );
            let at = match rows.iter().position(|(k, _)| *k == key) {
                Some(i) => i,
                None => {
                    rows.push((
                        key,
                        MergedMod {
                            info: m.clone(),
                            accounts: vec![],
                            on: vec![],
                            missing: vec![],
                        },
                    ));
                    rows.len() - 1
                }
            };
            let row = &mut rows[at].1;
            row.accounts.push((*account).clone());
            if m.enabled {
                row.on.push((*account).clone());
            }
            if row.info.url.is_none() {
                row.info.url = m.url.clone();
            }
            for n in &m.notes {
                if !row.info.notes.contains(n) {
                    row.info.notes.push(n.clone());
                }
            }
        }
    }
    rows.into_iter()
        .map(|(_, mut row)| {
            row.info.enabled = row.on.len() == row.accounts.len();
            row.missing = answered
                .iter()
                .map(|(a, _)| (*a).clone())
                .filter(|a| !row.accounts.contains(a))
                .collect();
            row
        })
        .collect()
}

/// One entry per mod name: the listing with the most installs. Order is kept.
pub fn unique_mods(mods: &[CatalogMod]) -> Vec<CatalogMod> {
    let mut best: Vec<CatalogMod> = Vec::new();
    for m in mods {
        match best.iter_mut().find(|b| b.name == m.name) {
            Some(seen) => {
                if m.installs.map_or(-1, |n| n as i128) > seen.installs.map_or(-1, |n| n as i128) {
                    *seen = m.clone();
                }
            }
            None => best.push(m.clone()),
        }
    }
    best
}

/// Mods matching every word of `query`, best first: a name that starts with the query, then a
/// name that holds it, then a description match; most installed first within each. An empty
/// query lists the most installed.
pub fn search_mods(mods: &[CatalogMod], query: &str) -> Vec<CatalogMod> {
    let lower = query.to_lowercase();
    let words: Vec<&str> = lower.split_whitespace().collect();
    let whole = words.join(" ");
    let mut ranked: Vec<(u8, CatalogMod)> = unique_mods(mods)
        .into_iter()
        .filter_map(|m| {
            let name = m.name.to_lowercase().replace(['-', '_'], " ");
            if words.is_empty() || name.starts_with(&whole) {
                return Some((0, m));
            }
            // "status line" also finds `statusline`.
            if words.iter().all(|w| name.contains(w))
                || name.replace(' ', "").contains(&words.concat())
            {
                return Some((1, m));
            }
            let text = format!("{name} {}", m.description.to_lowercase());
            words.iter().all(|w| text.contains(w)).then_some((2, m))
        })
        .collect();
    let count = |m: &CatalogMod| m.installs.or(m.stars).unwrap_or(0);
    ranked.sort_by(|(ra, a), (rb, b)| {
        ra.cmp(rb)
            .then(count(b).cmp(&count(a)))
            .then(a.name.cmp(&b.name))
    });
    ranked.into_iter().map(|(_, m)| m).collect()
}

/// 1290640 → "1.3M", 4520 → "4.5k".
pub fn format_installs(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 10_000 {
        format!("{:.0}k", n as f64 / 1000.0)
    } else if n >= 1000 {
        format!("{:.1}k", n as f64 / 1000.0)
    } else {
        n.to_string()
    }
}

/// Where Download puts a bare plugin: `~/.wraithgrid/mods/<name>`, as saved in `modDirs`.
pub fn download_dir(name: &str) -> String {
    format!("~/.wraithgrid/mods/{name}")
}

fn git(args: &[&str], cwd: &Path, env: &Env) -> Result<String, String> {
    let out = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .env_clear()
        .envs(env)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("git: {e}"))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        Err(last_line(&String::from_utf8_lossy(&out.stderr))
            .unwrap_or_else(|| format!("git {} failed", args[0])))
    }
}

/// Download: a community mod that is a bare plugin (`clone` set), cloned into
/// `~/.wraithgrid/mods/<name>`; Ok is that folder as `modDirs` keeps it.
pub fn clone_mod(home: &Path, m: &CatalogMod, env: &Env) -> Result<String, String> {
    let repo = m.clone.as_deref().unwrap_or("");
    if !GITHUB_REPO.is_match(repo) || !MARKETPLACE_NAME.is_match(&m.name) || m.name.contains("..") {
        return Err("That mod is no longer offered. Refresh the list.".into());
    }
    let root = home.join(".wraithgrid").join("mods");
    let dir = root.join(&m.name);
    if dir.exists() {
        return Err(format!("{} already exists.", contract_home(&dir, home)));
    }
    std::fs::create_dir_all(&root).map_err(|e| format!("Could not download the mod: {e}"))?;
    let url = format!("https://github.com/{repo}.git");
    git(
        &["clone", "--depth", "1", "--", &url, &dir.to_string_lossy()],
        &root,
        env,
    )?;
    Ok(contract_home(&dir, home))
}

/// Update on a downloaded mod: `git pull --ff-only` in its clone; Ok says whether it changed.
pub fn pull_mod(home: &Path, name: &str, env: &Env) -> Result<bool, String> {
    if !MARKETPLACE_NAME.is_match(name) || name.contains("..") {
        return Err("Not a mod name.".into());
    }
    let dir = home.join(".wraithgrid").join("mods").join(name);
    if !dir.join(".git").exists() {
        return Err(format!(
            "{} is not a downloaded mod.",
            contract_home(&dir, home)
        ));
    }
    let before = git(&["rev-parse", "HEAD"], &dir, env)?;
    git(&["pull", "--ff-only"], &dir, env)?;
    let after = git(&["rev-parse", "HEAD"], &dir, env)?;
    Ok(before.trim() != after.trim())
}

/// The ids every answering account has: for "Installed" / "Add to all accounts" in Browse.
pub fn installed_everywhere(results: &[AccountMods], id: &str) -> (usize, usize) {
    let answered: Vec<&Vec<ModInfo>> = results
        .iter()
        .filter_map(|r| r.mods.as_ref().ok())
        .collect();
    let have = answered
        .iter()
        .filter(|mods| mods.iter().any(|m| m.id == id))
        .count();
    (have, answered.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(id: &str, on: bool) -> ModInfo {
        ModInfo {
            id: id.into(),
            version: "1.0.0".into(),
            scope: "user".into(),
            enabled: on,
            project_path: None,
            url: None,
            notes: vec![],
        }
    }

    fn cat(name: &str, desc: &str, installs: Option<u64>) -> CatalogMod {
        CatalogMod {
            id: format!("{name}@m"),
            name: name.into(),
            description: desc.into(),
            marketplace: "m".into(),
            version: None,
            installs,
            stars: None,
            url: None,
            clone: None,
        }
    }

    #[test]
    fn merges_the_accounts_lists() {
        let rows = merge_mods(&[
            AccountMods {
                account_id: "a".into(),
                mods: Ok(vec![info("x@m", true), info("y@m", true)]),
            },
            AccountMods {
                account_id: "b".into(),
                mods: Ok(vec![info("x@m", false)]),
            },
            AccountMods {
                account_id: "c".into(),
                mods: Err("no".into()),
            },
        ]);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].accounts, ["a", "b"]);
        assert_eq!(rows[0].on, ["a"]);
        assert!(!rows[0].info.enabled);
        assert!(rows[0].missing.is_empty());
        assert_eq!(rows[1].missing, ["b"]);
        assert!(rows[1].info.enabled);
    }

    #[test]
    fn searches_names_first_then_descriptions() {
        let mods = [
            cat("burn-meter", "shows tokens", Some(10)),
            cat("statusline", "a status line", Some(5)),
            cat("meter", "other", Some(1)),
            cat("statusline", "dup with more", Some(50)),
        ];
        let names = |q: &str| -> Vec<String> {
            search_mods(&mods, q)
                .into_iter()
                .map(|m| format!("{}:{}", m.name, m.installs.unwrap_or(0)))
                .collect()
        };
        assert_eq!(names(""), ["statusline:50", "burn-meter:10", "meter:1"]);
        assert_eq!(names("status line"), ["statusline:50"]);
        assert_eq!(names("meter"), ["meter:1", "burn-meter:10"]);
        assert_eq!(names("tokens"), ["burn-meter:10"]);
        assert_eq!(format_installs(1_290_640), "1.3M");
        assert_eq!(format_installs(4520), "4.5k");
        assert_eq!(format_installs(45_200), "45k");
    }

    #[test]
    fn download_refuses_unsafe_names() {
        let home = std::env::temp_dir();
        let mut m = cat("..", "", None);
        m.clone = Some("owner/repo".into());
        assert!(clone_mod(&home, &m, &Env::new()).is_err());
        assert!(pull_mod(&home, "../x", &Env::new()).is_err());
    }
}
