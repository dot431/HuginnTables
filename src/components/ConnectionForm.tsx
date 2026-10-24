import { useState } from "react";
import { open } from "@tauri-apps/plugin-dialog";
import { api, ConnectionConfig, DEFAULT_PORTS, errorMessage, SslMode } from "../lib/api";
import { ConnectionTemplate, connectionFromTemplate, TEMPLATES, templateFor } from "../lib/templates";
import { Modal } from "./ui";

const COLORS = ["#5b8def", "#2f9e5b", "#c98a12", "#d6453d", "#9b59d0", "#1aa3a3", "#7a7f8c"];

/** Step one of a new connection: pick what to connect to. */
function TemplatePicker(props: { onPick: (t: ConnectionTemplate) => void; onClose: () => void }) {
  const groups = [...new Set(TEMPLATES.map((t) => t.group))];
  return (
    <Modal title="New Connection" wide onClose={props.onClose}>
      {groups.map((g) => (
        <div key={g} className="tpl-group">
          <div className="group-label">{g}</div>
          <div className="tpl-grid">
            {TEMPLATES.filter((t) => t.group === g).map((t) => (
              <button key={t.id} className="tpl" onClick={() => props.onPick(t)}>
                <span className="tpl-mark" style={{ background: t.color }}>{t.mark}</span>
                <span className="tpl-title">{t.title}</span>
              </button>
            ))}
          </div>
        </div>
      ))}
    </Modal>
  );
}

export function ConnectionForm(props: {
  initial: ConnectionConfig;
  /** Start on the template gallery (new connections). */
  pickTemplate?: boolean;
  onClose: () => void;
  onSaved: (cfg: ConnectionConfig, connect: boolean) => void;
}) {
  const [picking, setPicking] = useState(!!props.pickTemplate);
  const [cfg, setCfg] = useState<ConnectionConfig>(props.initial);
  const [status, setStatus] = useState<{ ok: boolean; text: string } | null>(null);
  const [busy, setBusy] = useState(false);
  const [mongoMode, setMongoMode] = useState<"uri" | "host">(props.initial.uri ? "uri" : "host");
  const isNew = !props.initial.id;
  const saved = !isNew;
  const tpl = templateFor(cfg);

  if (picking) {
    return (
      <TemplatePicker
        onClose={props.onClose}
        onPick={(t) => {
          const next = connectionFromTemplate(t);
          setCfg(next);
          setMongoMode(next.uri ? "uri" : "host");
          setPicking(false);
        }}
      />
    );
  }

  const set = <K extends keyof ConnectionConfig>(key: K, value: ConnectionConfig[K]) =>
    setCfg((c) => ({ ...c, [key]: value }));
  const setSsh = <K extends keyof ConnectionConfig["ssh"]>(key: K, value: ConnectionConfig["ssh"][K]) =>
    setCfg((c) => ({ ...c, ssh: { ...c.ssh, [key]: value } }));

  const normalized = (): ConnectionConfig => ({
    ...cfg,
    uri: cfg.driver === "mongodb" && mongoMode === "uri" ? cfg.uri.trim() : "",
    name: cfg.name.trim() || defaultName({ ...cfg, uri: mongoMode === "uri" ? cfg.uri : "" }),
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
    const filters =
      key === "keyPath"
        ? undefined
        : cfg.driver === "bigquery"
          ? [{ name: "Service account key", extensions: ["json"] }]
          : [{ name: "SQLite database", extensions: ["sqlite", "sqlite3", "db", "db3", "s3db"] }, { name: "All files", extensions: ["*"] }];
    const picked = await open({ multiple: false, directory: false, filters });
    if (typeof picked === "string") {
      if (key === "filePath") set("filePath", picked);
      else setSsh("keyPath", picked);
    }
  };

  const secretPlaceholder = saved ? "Stored in Keychain" : "";
  const d = cfg.driver;
  const hostFields = d === "postgres" || d === "mysql" || d === "redis" || (d === "mongodb" && mongoMode === "host");
  const canTunnel = hostFields;

  const tlsSelect = (
    <select value={cfg.sslMode} onChange={(e) => set("sslMode", e.target.value as SslMode)}>
      {d === "postgres" || d === "mysql" ? (
        <>
          <option value="disable">Disabled</option>
          <option value="prefer">Preferred (use if available)</option>
          <option value="require">Required (don't verify certificate)</option>
          <option value="verifyFull">Verify full (certificate + host name)</option>
        </>
      ) : (
        <>
          <option value="disable">Disabled</option>
          <option value="require">TLS (don't verify certificate)</option>
          <option value="verifyFull">TLS, verify certificate</option>
        </>
      )}
    </select>
  );

  return (
    <Modal
      title={
        <span className="row">
          <span className="tpl-mark small" style={{ background: tpl.color }}>{tpl.mark}</span>
          {isNew ? tpl.title : `Edit ${props.initial.name}`}
          {isNew && (
            <button className="ghost" style={{ marginLeft: "auto", fontWeight: 400 }} onClick={() => setPicking(true)}>
              Change type
            </button>
          )}
        </span>
      }
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
      {tpl.help && <div className="notice info" style={{ marginBottom: 12 }}>{tpl.help}</div>}
      <div className="form-grid">
        <label>Name</label>
        <input value={cfg.name} placeholder={defaultName(cfg)} onChange={(e) => set("name", e.target.value)} autoFocus />

        <label>Color</label>
        <div className="color-swatches">
          {[tpl.color, ...COLORS.filter((c) => c !== tpl.color)].map((c) => (
            <button
              key={c}
              className={cfg.color === c ? "on" : ""}
              style={{ background: c }}
              onClick={() => set("color", c)}
              aria-label={`Color ${c}`}
            />
          ))}
        </div>

        <label>Safety</label>
        <div className="safety-options">
          <label>
            <input type="checkbox" checked={!!cfg.production} onChange={(e) => set("production", e.target.checked)} />
            <span>
              Production <span className="faint">— marked in red; writes show their SQL and ask first</span>
            </span>
          </label>
          <label>
            <input type="checkbox" checked={!!cfg.readOnly} onChange={(e) => set("readOnly", e.target.checked)} />
            <span>
              Read-only <span className="faint">— refuse every write, from the grid and the query editor</span>
            </span>
          </label>
        </div>

        {d === "mongodb" && (
          <>
            <label>Connect with</label>
            <div className="segmented">
              <button className={mongoMode === "uri" ? "on" : ""} onClick={() => setMongoMode("uri")}>Connection string</button>
              <button className={mongoMode === "host" ? "on" : ""} onClick={() => setMongoMode("host")}>Host &amp; port</button>
            </div>
          </>
        )}

        {d === "mongodb" && mongoMode === "uri" && (
          <>
            <label>URI</label>
            <textarea
              className="mono"
              rows={3}
              value={cfg.uri}
              placeholder="mongodb+srv://user:<password>@cluster0.example.mongodb.net/"
              onChange={(e) => set("uri", e.target.value)}
            />
            <label>Password</label>
            <input
              type="password"
              value={cfg.password ?? ""}
              placeholder={saved ? "Stored in Keychain (replaces <password>)" : "Replaces <password> in the URI"}
              onChange={(e) => set("password", e.target.value)}
            />
          </>
        )}

        {hostFields && (
          <>
            <label>Host</label>
            <div className="row">
              <input
                style={{ flex: 1 }}
                value={cfg.host}
                placeholder={tpl.hostPlaceholder ?? "127.0.0.1"}
                onChange={(e) => set("host", e.target.value)}
              />
              <span className="muted">Port</span>
              <input
                type="number"
                style={{ width: 80 }}
                value={cfg.port || ""}
                placeholder={String(DEFAULT_PORTS[d])}
                onChange={(e) => set("port", Number(e.target.value) || 0)}
              />
            </div>
            <label>User</label>
            <input
              value={cfg.user}
              placeholder={d === "redis" ? "default (optional, Redis 6+ ACL)" : d === "mongodb" ? "optional" : ""}
              onChange={(e) => set("user", e.target.value)}
            />
            <label>Password</label>
            <input
              type="password"
              value={cfg.password ?? ""}
              placeholder={secretPlaceholder || (d === "redis" ? "AUTH password (optional)" : "")}
              onChange={(e) => set("password", e.target.value)}
            />
            <label>{d === "redis" ? "Database" : "Database"}</label>
            {d === "redis" ? (
              <input
                type="number"
                min={0}
                style={{ width: 80 }}
                value={cfg.database}
                onChange={(e) => set("database", e.target.value)}
              />
            ) : (
              <input
                value={cfg.database}
                placeholder={d === "postgres" ? "postgres" : d === "mongodb" ? "default database (optional)" : "optional"}
                onChange={(e) => set("database", e.target.value)}
              />
            )}
            <label>{d === "postgres" || d === "mysql" ? "SSL" : "TLS"}</label>
            {tlsSelect}
          </>
        )}

        {d === "sqlite" && (
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

        {d === "bigquery" && (
          <>
            <label>Project ID</label>
            <input value={cfg.database} placeholder="my-gcp-project" onChange={(e) => set("database", e.target.value)} />
            <label>Key file</label>
            <div className="row">
              <input
                style={{ flex: 1 }}
                value={cfg.filePath}
                placeholder="Empty = gcloud application-default credentials"
                onChange={(e) => set("filePath", e.target.value)}
              />
              <button onClick={() => pickFile("filePath")}>Choose…</button>
            </div>
            <label>API endpoint</label>
            <input
              value={cfg.host}
              placeholder="https://bigquery.googleapis.com (or an emulator URL)"
              onChange={(e) => set("host", e.target.value)}
            />
          </>
        )}
      </div>

      {canTunnel && (
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
                The host above is resolved from the SSH server. New SSH hosts are added to ~/.ssh/known_hosts; a
                changed host key is refused.
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
  switch (cfg.driver) {
    case "sqlite":
      return cfg.filePath.split("/").pop() || "SQLite";
    case "bigquery":
      return cfg.database ? `BigQuery · ${cfg.database}` : "BigQuery";
    case "mongodb":
      if (cfg.uri.trim()) {
        const host = /@([^/?,]+)/.exec(cfg.uri)?.[1] ?? /\/\/([^/?,]+)/.exec(cfg.uri)?.[1];
        return host ?? "MongoDB";
      }
      break;
  }
  if (!cfg.host) return templateFor(cfg).title;
  const db = cfg.database && cfg.driver !== "redis" ? `${cfg.database} @ ` : "";
  return `${db}${cfg.host}`;
}
