//! Saved connections: settings in `connections.json`, secrets in the keychain.

use std::path::PathBuf;
use std::sync::Mutex;

use anyhow::{Context, Result};

use crate::model::ConnectionConfig;

const KEYCHAIN_SERVICE: &str = "HuginnTables";

pub struct ConnectionStore {
    path: PathBuf,
    lock: Mutex<()>,
}

#[derive(Clone, Copy)]
enum Secret {
    Password,
    SshPassword,
    SshPassphrase,
}

impl Secret {
    const ALL: [Secret; 3] = [Secret::Password, Secret::SshPassword, Secret::SshPassphrase];

    fn account(self, id: &str) -> String {
        let suffix = match self {
            Secret::Password => "password",
            Secret::SshPassword => "ssh-password",
            Secret::SshPassphrase => "ssh-passphrase",
        };
        format!("{id}/{suffix}")
    }

    fn field(self, cfg: &mut ConnectionConfig) -> &mut Option<String> {
        match self {
            Secret::Password => &mut cfg.password,
            Secret::SshPassword => &mut cfg.ssh_password,
            Secret::SshPassphrase => &mut cfg.ssh_passphrase,
        }
    }
}

fn entry(account: &str) -> Result<keyring::Entry> {
    keyring::Entry::new(KEYCHAIN_SERVICE, account).context("keychain unavailable")
}

impl ConnectionStore {
    pub fn new(dir: PathBuf) -> Self {
        Self { path: dir.join("connections.json"), lock: Mutex::new(()) }
    }

    fn read(&self) -> Result<Vec<ConnectionConfig>> {
        match std::fs::read_to_string(&self.path) {
            Ok(text) => Ok(serde_json::from_str(&text).context("connections.json is corrupt")?),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
            Err(e) => Err(e.into()),
        }
    }

    fn write(&self, list: &[ConnectionConfig]) -> Result<()> {
        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = self.path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(list)?)?;
        std::fs::rename(tmp, &self.path)?;
        Ok(())
    }

    pub fn list(&self) -> Result<Vec<ConnectionConfig>> {
        let _guard = self.lock.lock().unwrap();
        self.read()
    }

    /// Saves settings to disk. A secret that is `Some` replaces the stored one
    /// (an empty string deletes it); `None` keeps whatever is stored.
    pub fn save(&self, mut cfg: ConnectionConfig) -> Result<ConnectionConfig> {
        let _guard = self.lock.lock().unwrap();
        if cfg.id.is_empty() {
            cfg.id = uuid::Uuid::new_v4().to_string();
        }
        for secret in Secret::ALL {
            if let Some(value) = secret.field(&mut cfg).take() {
                let entry = entry(&secret.account(&cfg.id))?;
                if value.is_empty() {
                    let _ = entry.delete_credential();
                } else {
                    entry.set_password(&value)?;
                }
            }
        }
        let mut list = self.read()?;
        match list.iter_mut().find(|c| c.id == cfg.id) {
            Some(existing) => *existing = cfg.clone(),
            None => list.push(cfg.clone()),
        }
        self.write(&list)?;
        Ok(cfg)
    }

    pub fn delete(&self, id: &str) -> Result<()> {
        let _guard = self.lock.lock().unwrap();
        let mut list = self.read()?;
        list.retain(|c| c.id != id);
        self.write(&list)?;
        for secret in Secret::ALL {
            if let Ok(entry) = entry(&secret.account(id)) {
                let _ = entry.delete_credential();
            }
        }
        Ok(())
    }

    /// Fills secrets the UI didn't send from the keychain.
    pub fn fill_secrets(&self, cfg: &mut ConnectionConfig) {
        if cfg.id.is_empty() {
            return;
        }
        let id = cfg.id.clone();
        for secret in Secret::ALL {
            let field = secret.field(cfg);
            if field.is_none() {
                *field = entry(&secret.account(&id))
                    .ok()
                    .and_then(|e| e.get_password().ok());
            }
        }
    }
}
