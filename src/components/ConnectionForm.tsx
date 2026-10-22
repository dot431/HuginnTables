import { useState } from "react";
import { open } from "@tauri-apps/plugin-dialog";
import {
  api,
  ConnectionConfig,
  DEFAULT_PORTS,
  DRIVER_LABELS,
  DriverKind,
  errorMessage,
  SslMode,
} from "../lib/api";
import { Modal } from "./ui";

const COLORS = ["#5b8def", "#2f9e5b", "#c98a12", "#d6453d", "#9b59d0", "#1aa3a3", "#7a7f8c"];

export function ConnectionForm(props: {
  initial: ConnectionConfig;
  onClose: () => void;
  onSaved: (cfg: ConnectionConfig, connect: boolean) => void;
}) {
  const [cfg, setCfg] = useState<ConnectionConfig>(props.initial);
  const [status, setStatus] = useState<{ ok: boolean; text: string } | null>(null);
  const [busy, setBusy] = useState(false);
  const isNew = !props.initial.id;
  const saved = !isNew;

  const set = <K extends keyof ConnectionConfig>(key: K, value: ConnectionConfig[K]) =>
    setCfg((c) => ({ ...c, [key]: value }));
  const setSsh = <K extends keyof ConnectionConfig["ssh"]>(key: K, value: ConnectionConfig["ssh"][K]) =>
    setCfg((c) => ({ ...c, ssh: { ...c.ssh, [key]: value } }));

  const changeDriver = (driver: DriverKind) =>
    setCfg((c) => ({
      ...c,
      driver,
      port: c.port === DEFAULT_PORTS[c.driver] ? DEFAULT_PORTS[driver] : c.port,
      user: c.user === "postgres" || c.user === "root" ? (driver === "mysql" ? "root" : "postgres") : c.user,
    }));

  const normalized = (): ConnectionConfig => ({
    ...cfg,
    name: cfg.name.trim() || defaultName(cfg),
  });

  const test = async () => {
    setBusy(true);
    setStatus(null);
    try {
      const version = await api.testConnection(normalized());
      setStatus({ ok: true, text: `Connected — ${version}` });
    } catch (e) {
      setStatus({ ok: false, text: errorMessage(e) });
    } finally {
      setBusy(false);
    }
  };

  const save = async (connect: boolean) => {
    setBusy(true);
    try {
      const stored = await api.saveConnection(normalized());
      props.onSaved(stored, connect);
    } catch (e) {
      setStatus({ ok: false, text: errorMessage(e) });
      setBusy(false);
    }
  };

  const pickFile = async (key: "filePath" | "keyPath") => {
    const picked = await open({
      multiple: false,
      directory: false,
      filters:
        key === "filePath"
          ? [{ name: "SQLite database", extensions: ["sqlite", "sqlite3", "db", "db3", "s3db"] }, { name: "All files", extensions: ["*"] }]
          : undefined,
    });
    if (typeof picked === "string") {
      if (key === "filePath") set("filePath", picked);
      else setSsh("keyPath", picked);
    }
  };

  const secretPlaceholder = saved ? "Stored in Keychain" : "";
  const server = cfg.driver !== "sqlite";

  return (
    <Modal
      title={isNew ? "New Connection" : `Edit ${props.initial.name}`}
      onClose={props.onClose}
      footer={
        <>
          <button onClick={test} disabled={busy}>Test</button>
          {busy && <div className="spinner" />}
          <div className="spacer" />
          <button onClick={props.onClose}>Cancel</button>
          <button onClick={() => save(false)} disabled={busy}>Save</button>
          <button className="primary" onClick={() => save(true)} disabled={busy}>Connect</button>
        </>
      }
    >
      <div className="form-grid">
        <label>Type</label>
        <div className="segmented">
          {(Object.keys(DRIVER_LABELS) as DriverKind[]).map((d) => (
            <button key={d} className={cfg.driver === d ? "on" : ""} onClick={() => changeDriver(d)}>
              {DRIVER_LABELS[d]}
            </button>
          ))}
        </div>

        <label>Name</label>
        <input value={cfg.name} placeholder={defaultName(cfg)} onChange={(e) => set("name", e.target.value)} autoFocus />

        <label>Color</label>
        <div className="color-swatches">
          {COLORS.map((c) => (
            <button
              key={c}
              className={cfg.color === c ? "on" : ""}
              style={{ background: c }}
              onClick={() => set("color", c)}
              aria-label={`Color ${c}`}
            />
          ))}
        </div>

        {server ? (
          <>
            <label>Host</label>
            <div className="row">
              <input style={{ flex: 1 }} value={cfg.host} onChange={(e) => set("host", e.target.value)} />
              <span className="muted">Port</span>
              <input
                type="number"
                style={{ width: 80 }}
                value={cfg.port || ""}
                onChange={(e) => set("port", Number(e.target.value) || 0)}
              />
            </div>
            <label>User</label>
            <input value={cfg.user} onChange={(e) => set("user", e.target.value)} />
            <label>Password</label>
            <input
              type="password"
              value={cfg.password ?? ""}
              placeholder={secretPlaceholder}
              onChange={(e) => set("password", e.target.value)}
            />
            <label>Database</label>
            <input
              value={cfg.database}
              placeholder={cfg.driver === "postgres" ? "postgres" : "optional"}
              onChange={(e) => set("database", e.target.value)}
            />
            <label>SSL</label>
            <select value={cfg.sslMode} onChange={(e) => set("sslMode", e.target.value as SslMode)}>
              <option value="disable">Disabled</option>
              <option value="prefer">Preferred (use if available)</option>
              <option value="require">Required (don't verify certificate)</option>
              <option value="verifyFull">Verify full (certificate + host name)</option>
            </select>
          </>
        ) : (
          <>
            <label>File</label>
            <div className="row">
              <input
                style={{ flex: 1 }}
                value={cfg.filePath}
                placeholder="/path/to/database.sqlite"
                onChange={(e) => set("filePath", e.target.value)}
              />
              <button onClick={() => pickFile("filePath")}>Choose…</button>
            </div>
          </>
        )}
      </div>

      {server && (
        <>
          <div className="form-section">
            <input
              type="checkbox"
              id="ssh-enabled"
              checked={cfg.ssh.enabled}
              onChange={(e) => setSsh("enabled", e.target.checked)}
            />
            <label htmlFor="ssh-enabled">Connect over SSH tunnel</label>
          </div>
          {cfg.ssh.enabled && (
            <div className="form-grid">
              <label>SSH host</label>
              <div className="row">
                <input style={{ flex: 1 }} value={cfg.ssh.host} onChange={(e) => setSsh("host", e.target.value)} />
                <span className="muted">Port</span>
                <input
                  type="number"
                  style={{ width: 80 }}
                  value={cfg.ssh.port || ""}
                  onChange={(e) => setSsh("port", Number(e.target.value) || 0)}
                />
              </div>
              <label>SSH user</label>
              <input value={cfg.ssh.user} onChange={(e) => setSsh("user", e.target.value)} />
              <label>Auth</label>
              <div className="segmented">
                <button className={cfg.ssh.auth === "password" ? "on" : ""} onClick={() => setSsh("auth", "password")}>
                  Password
                </button>
                <button
                  className={cfg.ssh.auth === "privateKey" ? "on" : ""}
                  onClick={() => setSsh("auth", "privateKey")}
                >
                  Private key
                </button>
              </div>
              {cfg.ssh.auth === "password" ? (
                <>
                  <label>SSH password</label>
                  <input
                    type="password"
                    value={cfg.sshPassword ?? ""}
                    placeholder={secretPlaceholder}
                    onChange={(e) => set("sshPassword", e.target.value)}
                  />
                </>
              ) : (
                <>
                  <label>Key file</label>
                  <div className="row">
                    <input
                      style={{ flex: 1 }}
                      value={cfg.ssh.keyPath}
                      onChange={(e) => setSsh("keyPath", e.target.value)}
                    />
                    <button onClick={() => pickFile("keyPath")}>Choose…</button>
                  </div>
                  <label>Passphrase</label>
                  <input
                    type="password"
                    value={cfg.sshPassphrase ?? ""}
                    placeholder={saved ? "Stored in Keychain (if any)" : "optional"}
                    onChange={(e) => set("sshPassphrase", e.target.value)}
                  />
                </>
              )}
              <div className="full faint" style={{ fontSize: 12 }}>
                The database host above is resolved from the SSH server. New SSH hosts are added to ~/.ssh/known_hosts;
                a changed host key is refused.
              </div>
            </div>
          )}
        </>
      )}

      {status && (
        <div className={`notice ${status.ok ? "ok" : "err"}`} style={{ marginTop: 14 }}>
          {status.text}
        </div>
      )}
    </Modal>
  );
}

export function defaultName(cfg: ConnectionConfig): string {
  if (cfg.driver === "sqlite") return cfg.filePath.split("/").pop() || "SQLite";
  const db = cfg.database ? `${cfg.database} @ ` : "";
  return `${db}${cfg.host || "localhost"}`;
}
