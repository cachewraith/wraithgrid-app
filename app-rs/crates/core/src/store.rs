//! The single owner of config.json (`src/main/config-store.ts`): keeps the validated config
//! in memory, writes it atomically, and never lets a corrupt file stop the app from starting.
//! Debouncing is the caller's job (the app saves on a timer after changes).

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::config::{default_config, parse_config, Config};

/// Writes via a temp file + rename so a crash mid-write never leaves a torn config.
pub fn write_file_atomic(file: &Path, data: &str) -> std::io::Result<()> {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    let tmp = PathBuf::from(format!(
        "{}.{}.{millis}.tmp",
        file.display(),
        std::process::id()
    ));
    {
        let mut opts = fs::OpenOptions::new();
        opts.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        let mut f = opts.open(&tmp)?;
        f.write_all(data.as_bytes())?;
        f.sync_all()?;
    }
    // Windows refuses the rename while antivirus or the indexer holds the target open.
    let mut attempt = 0;
    loop {
        match fs::rename(&tmp, file) {
            Ok(()) => return Ok(()),
            Err(err) if attempt < 5 && err.kind() == std::io::ErrorKind::PermissionDenied => {
                attempt += 1;
                std::thread::sleep(Duration::from_millis(50 * attempt));
            }
            Err(err) => {
                let _ = fs::remove_file(&tmp);
                return Err(err);
            }
        }
    }
}

pub struct ConfigStore {
    pub file: PathBuf,
    config: Config,
    dirty: bool,
    log: Box<dyn Fn(&str) + Send>,
}

impl ConfigStore {
    pub fn new(file: PathBuf) -> Self {
        Self::with_log(file, Box::new(|msg| eprintln!("[wraithgrid] {msg}")))
    }

    pub fn with_log(file: PathBuf, log: Box<dyn Fn(&str) + Send>) -> Self {
        ConfigStore {
            file,
            config: default_config(),
            dirty: false,
            log,
        }
    }

    /// Reads the file once at startup. Missing → defaults. Corrupt → backed up, then defaults.
    pub fn load(&mut self) -> &Config {
        self.load_at(SystemTime::now())
    }

    fn load_at(&mut self, now: SystemTime) -> &Config {
        let text = match fs::read_to_string(&self.file) {
            Ok(text) => text,
            Err(err) => {
                if err.kind() != std::io::ErrorKind::NotFound {
                    (self.log)(&format!("could not read config: {err}"));
                }
                self.config = default_config();
                return &self.config;
            }
        };
        let error = match serde_json::from_str(&text) {
            Ok(raw) => match parse_config(&raw, true) {
                Ok(config) => {
                    self.config = config;
                    return &self.config;
                }
                Err(err) => err,
            },
            Err(err) => format!("invalid JSON: {err}"),
        };
        let backup = self.backup_path(now);
        match fs::rename(&self.file, &backup) {
            Ok(()) => (self.log)(&format!(
                "config was corrupt ({}); moved it to {}",
                error.lines().next().unwrap_or(""),
                backup.display()
            )),
            Err(err) => (self.log)(&format!(
                "config was corrupt and could not be backed up: {err}"
            )),
        }
        self.config = default_config();
        &self.config
    }

    pub fn get(&self) -> &Config {
        &self.config
    }

    /// Replaces the config; it is written on the next `save`.
    pub fn set(&mut self, config: Config) {
        if config != self.config {
            self.config = config;
            self.dirty = true;
        }
    }

    /// Changes the config in place; it is written on the next `save`.
    pub fn update(&mut self, change: impl FnOnce(&mut Config)) {
        let mut next = self.config.clone();
        change(&mut next);
        self.set(next);
    }

    pub fn is_dirty(&self) -> bool {
        self.dirty
    }

    /// Writes a pending change now. Errors are logged, and the change stays pending.
    pub fn save(&mut self) {
        if !self.dirty {
            return;
        }
        let data = self.config.to_json();
        let result = self
            .file
            .parent()
            .map_or(Ok(()), fs::create_dir_all)
            .and_then(|()| write_file_atomic(&self.file, &data));
        match result {
            Ok(()) => self.dirty = false,
            Err(err) => (self.log)(&format!("could not save config: {err}")),
        }
    }

    fn backup_path(&self, now: SystemTime) -> PathBuf {
        let ts = iso_timestamp(now).replace([':', '.'], "-");
        self.file.with_file_name(format!("config.bad-{ts}.json"))
    }
}

/// `Date.toISOString()`: `2026-11-12T08:30:00.123Z`.
pub fn iso_timestamp(t: SystemTime) -> String {
    let d = t.duration_since(UNIX_EPOCH).unwrap_or_default();
    let secs = d.as_secs() as i64;
    let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    // Civil date from days since 1970-01-01 (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.{:03}Z",
        rem / 3600,
        rem / 60 % 60,
        rem % 60,
        d.subsec_millis()
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("wg-store-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn iso() {
        assert_eq!(iso_timestamp(UNIX_EPOCH), "1970-01-01T00:00:00.000Z");
        let t = UNIX_EPOCH + Duration::from_millis(1_794_487_224_123);
        assert_eq!(iso_timestamp(t), "2026-11-12T12:40:24.123Z");
    }

    #[test]
    fn missing_file_gives_defaults() {
        let dir = temp_dir("missing");
        let mut s = ConfigStore::new(dir.join("config.json"));
        assert_eq!(s.load().workspaces[0].id, "ws-default");
    }

    #[test]
    fn corrupt_file_is_backed_up() {
        let dir = temp_dir("corrupt");
        let file = dir.join("config.json");
        fs::write(&file, "{ not json").unwrap();
        let logs = Arc::new(Mutex::new(Vec::new()));
        let sink = logs.clone();
        let mut s = ConfigStore::with_log(
            file.clone(),
            Box::new(move |m| sink.lock().unwrap().push(m.to_string())),
        );
        s.load_at(UNIX_EPOCH);
        assert!(!file.exists());
        assert!(dir
            .join("config.bad-1970-01-01T00-00-00-000Z.json")
            .exists());
        assert!(logs.lock().unwrap()[0].contains("config was corrupt (invalid JSON"));
    }

    #[test]
    fn save_round_trips() {
        let dir = temp_dir("save");
        let file = dir.join("sub").join("config.json");
        let mut s = ConfigStore::new(file.clone());
        s.load();
        s.update(|c| c.settings.font_size = 15);
        assert!(s.is_dirty());
        s.save();
        assert!(!s.is_dirty());
        let mut again = ConfigStore::new(file.clone());
        assert_eq!(again.load().settings.font_size, 15);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(&file).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
    }
}
