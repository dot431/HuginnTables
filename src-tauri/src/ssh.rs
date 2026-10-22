//! Local port forwarding over SSH: a loopback listener whose connections are
//! piped through `direct-tcpip` channels to the database host.

use std::net::{IpAddr, Ipv4Addr};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use russh::client::{self, Handle};
use russh::keys::{self, PrivateKeyWithHashAlg, PublicKeyOrCertificate};
use tokio::net::TcpListener;
use tokio::task::JoinHandle;

use crate::model::{SshAuth, SshConfig};

struct HostKeyCheck {
    host: String,
    port: u16,
    problem: Arc<Mutex<Option<String>>>,
}

impl client::Handler for HostKeyCheck {
    type Error = russh::Error;

    /// Behaves like OpenSSH's `StrictHostKeyChecking=accept-new`: unknown hosts
    /// are added to ~/.ssh/known_hosts, changed keys are refused.
    async fn check_server_key(&mut self, key: &PublicKeyOrCertificate) -> Result<bool, Self::Error> {
        let PublicKeyOrCertificate::PublicKey { key, .. } = key else {
            return Ok(true);
        };
        let path = known_hosts_path();
        match keys::check_known_hosts_path(&self.host, self.port, key, &path) {
            Ok(true) => Ok(true),
            Err(keys::Error::KeyChanged { line }) => {
                *self.problem.lock().unwrap() = Some(format!(
                    "The SSH host key for {} has changed ({} line {line}). \
                     Refusing to connect: remove the old entry if the change is expected.",
                    self.host,
                    path.display()
                ));
                Ok(false)
            }
            // Unknown host, or no known_hosts file yet: trust on first use.
            _ => {
                if let Some(dir) = path.parent() {
                    let _ = std::fs::create_dir_all(dir);
                }
                let _ = keys::known_hosts::learn_known_hosts_path(&self.host, self.port, key, &path);
                Ok(true)
            }
        }
    }
}

/// `HUGINN_KNOWN_HOSTS` overrides the file, so tests don't touch ~/.ssh.
fn known_hosts_path() -> PathBuf {
    std::env::var_os("HUGINN_KNOWN_HOSTS")
        .map(PathBuf::from)
        .unwrap_or_else(|| dirs::home_dir().unwrap_or_default().join(".ssh/known_hosts"))
}

pub struct Tunnel {
    pub local_port: u16,
    handle: Arc<Handle<HostKeyCheck>>,
    accept_task: JoinHandle<()>,
}

impl Tunnel {
    pub fn local_addr(&self) -> (IpAddr, u16) {
        (IpAddr::V4(Ipv4Addr::LOCALHOST), self.local_port)
    }

    pub async fn open(
        cfg: &SshConfig,
        password: Option<&str>,
        passphrase: Option<&str>,
        target_host: &str,
        target_port: u16,
    ) -> Result<Self> {
        let port = if cfg.port == 0 { 22 } else { cfg.port };
        let problem = Arc::new(Mutex::new(None));
        let handler = HostKeyCheck { host: cfg.host.clone(), port, problem: problem.clone() };
        let config = Arc::new(client::Config {
            inactivity_timeout: None,
            keepalive_interval: Some(Duration::from_secs(30)),
            nodelay: true,
            ..Default::default()
        });

        let connect = client::connect(config, (cfg.host.as_str(), port), handler);
        let mut session = match tokio::time::timeout(Duration::from_secs(15), connect).await {
            Err(_) => bail!("SSH connection to {}:{port} timed out", cfg.host),
            Ok(Err(e)) => {
                if let Some(msg) = problem.lock().unwrap().take() {
                    bail!(msg);
                }
                return Err(anyhow!(e).context(format!("SSH connection to {}:{port} failed", cfg.host)));
            }
            Ok(Ok(s)) => s,
        };

        let auth = match cfg.auth {
            SshAuth::Password => session
                .authenticate_password(&cfg.user, password.unwrap_or_default())
                .await?,
            SshAuth::PrivateKey => {
                let path = expand_home(&cfg.key_path);
                let key = keys::load_secret_key(&path, passphrase.filter(|p| !p.is_empty()))
                    .with_context(|| format!("could not load SSH key {}", path.display()))?;
                let hash = session.best_supported_rsa_hash().await?.flatten();
                session
                    .authenticate_publickey(&cfg.user, PrivateKeyWithHashAlg::new(Arc::new(key), hash))
                    .await?
            }
        };
        if !auth.success() {
            bail!("SSH authentication failed for {}@{}", cfg.user, cfg.host);
        }

        let handle = Arc::new(session);
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await?;
        let local_port = listener.local_addr()?.port();
        let target_host = target_host.to_string();
        let forward = handle.clone();
        let accept_task = tokio::spawn(async move {
            while let Ok((mut socket, peer)) = listener.accept().await {
                let handle = forward.clone();
                let target_host = target_host.clone();
                tokio::spawn(async move {
                    let channel = match handle
                        .channel_open_direct_tcpip(
                            target_host,
                            u32::from(target_port),
                            peer.ip().to_string(),
                            u32::from(peer.port()),
                        )
                        .await
                    {
                        Ok(c) => c,
                        Err(_) => return,
                    };
                    let mut stream = channel.into_stream();
                    let _ = tokio::io::copy_bidirectional(&mut socket, &mut stream).await;
                });
            }
        });

        Ok(Self { local_port, handle, accept_task })
    }

    pub async fn close(&self) {
        self.accept_task.abort();
        let _ = self
            .handle
            .disconnect(russh::Disconnect::ByApplication, "", "en")
            .await;
    }
}

fn expand_home(path: &str) -> PathBuf {
    match path.strip_prefix("~/") {
        Some(rest) => dirs::home_dir().unwrap_or_default().join(rest),
        None => PathBuf::from(path),
    }
}
