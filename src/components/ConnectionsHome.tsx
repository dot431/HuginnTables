import { useEffect, useMemo, useState } from "react";
import {
  api,
  ConnectionConfig,
  emptyConnection,
  errorMessage,
  parseConnectionUrl,
} from "../lib/api";
import { templateFor } from "../lib/templates";
import { ConnectionForm } from "./ConnectionForm";
import { ContextMenu, DriverBadge, HuginnMark, MenuState, Modal, toast } from "./ui";

export function ConnectionsHome(props: {
  connecting: string | null;
  onConnect: (cfg: ConnectionConfig) => void;
  onOpenSettings: () => void;
}) {
  const [connections, setConnections] = useState<ConnectionConfig[]>([]);
  const [search, setSearch] = useState("");
  const [selected, setSelected] = useState<string | null>(null);
  const [editing, setEditing] = useState<ConnectionConfig | null>(null);
  const [pickTemplate, setPickTemplate] = useState(false);
  const startNew = () => {
    setPickTemplate(true);
    setEditing(emptyConnection());
  };
  const edit = (c: ConnectionConfig) => {
    setPickTemplate(false);
    setEditing(c);
  };
  const [importing, setImporting] = useState(false);
  const [menu, setMenu] = useState<MenuState | null>(null);

  const reload = () =>
    api.listConnections().then(setConnections).catch((e) => toast(errorMessage(e), "error"));
  useEffect(() => void reload(), []);

  const visible = useMemo(() => {
    const q = search.toLowerCase();
    return connections
      .filter((c) => [c.name, c.host, c.database, c.filePath, c.uri, templateFor(c).title].some((v) => (v ?? "").toLowerCase().includes(q)))
      .sort((a, b) => a.name.localeCompare(b.name));
  }, [connections, search]);

  const remove = async (c: ConnectionConfig) => {
    if (!window.confirm(`Delete the connection "${c.name}"? Its saved passwords are removed from the Keychain.`)) return;
    await api.deleteConnection(c.id).catch((e) => toast(errorMessage(e), "error"));
    reload();
  };

  const duplicate = (c: ConnectionConfig) => edit({ ...c, id: "", name: `${c.name} copy`, password: undefined });

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (editing || importing) return;
      if (e.metaKey && e.key === "n") {
        e.preventDefault();
        startNew();
      }
      const current = visible.find((c) => c.id === selected);
      if (e.key === "Enter" && current) props.onConnect(current);
      if (e.key === "ArrowDown" || e.key === "ArrowUp") {
        e.preventDefault();
        const idx = visible.findIndex((c) => c.id === selected);
        const next = visible[Math.max(0, Math.min(visible.length - 1, idx + (e.key === "ArrowDown" ? 1 : -1)))];
        if (next) setSelected(next.id);
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [visible, selected, editing, importing, props]);

  return (
    <div className="home">
      <div className="home-inner">
        <div className="brand">
          <HuginnMark />
          <div>
            <h1 className="wordmark" aria-label="HuginnTables">
              huginn<span>table</span>
            </h1>
            <p>PostgreSQL · MySQL · SQLite</p>
          </div>
          <div className="spacer" />
          <button className="ghost" title="Settings (⌘,)" onClick={props.onOpenSettings}>
            Settings
          </button>
        </div>

        <div className="home-toolbar">
          <input placeholder="Search connections" value={search} onChange={(e) => setSearch(e.target.value)} />
          <button onClick={() => setImporting(true)}>Import URL…</button>
          <button className="primary" onClick={startNew}>
            New Connection
          </button>
        </div>

        {connections.length === 0 ? (
          <div className="conn-list empty-state">
            <h3>No connections yet</h3>
            <p>PostgreSQL, MySQL, SQLite, Redis, MongoDB and BigQuery — self-hosted, Google Cloud, AWS or Atlas.</p>
            <button className="primary" onClick={startNew}>
              New Connection
            </button>
          </div>
        ) : (
          <div className="conn-list">
            {visible.map((c) => (
              <div
                key={c.id}
                className={`conn-item ${selected === c.id ? "selected" : ""}`}
                onClick={() => setSelected(c.id)}
                onDoubleClick={() => props.onConnect(c)}
                onContextMenu={(e) => {
                  e.preventDefault();
                  setSelected(c.id);
                  setMenu({
                    x: e.clientX,
                    y: e.clientY,
                    items: [
                      { label: "Connect", action: () => props.onConnect(c) },
                      { label: "Edit…", action: () => edit(c) },
                      { label: "Duplicate", action: () => duplicate(c) },
                      { separator: true, label: "" },
                      { label: "Delete", action: () => remove(c) },
                    ],
                  });
                }}
              >
                <DriverBadge driver={c.driver} color={c.color} mark={templateFor(c).mark} />
                <div className="conn-meta">
                  <div className="title">{c.name}</div>
                  <div className="sub">
                    {templateFor(c).title} · {connectionTarget(c)}
                    {c.ssh.enabled && ` · via SSH ${c.ssh.host}`}
                  </div>
                </div>
                {props.connecting === c.id ? (
                  <div className="spinner" />
                ) : (
                  <div className="conn-actions">
                    <button className="ghost" onClick={(e) => (e.stopPropagation(), edit(c))}>
                      Edit
                    </button>
                    <button onClick={(e) => (e.stopPropagation(), props.onConnect(c))}>Connect</button>
                  </div>
                )}
              </div>
            ))}
          </div>
        )}
        <p className="faint" style={{ textAlign: "center", marginTop: 16, fontSize: 12 }}>
          Double-click or press <span className="kbd">↵</span> to connect · <span className="kbd">⌘N</span> new connection
        </p>
      </div>

      {editing && (
        <ConnectionForm
          initial={editing}
          pickTemplate={pickTemplate}
          onClose={() => setEditing(null)}
          onSaved={(cfg, connect) => {
            setEditing(null);
            reload();
            setSelected(cfg.id);
            if (connect) props.onConnect(cfg);
          }}
        />
      )}
      {importing && (
        <ImportUrl
          onClose={() => setImporting(false)}
          onParsed={(cfg) => {
            setImporting(false);
            edit({ ...cfg, color: templateFor(cfg).color });
          }}
        />
      )}
      <ContextMenu menu={menu} onClose={() => setMenu(null)} />
    </div>
  );
}

/** Where a connection points, without secrets. */
function connectionTarget(c: ConnectionConfig): string {
  switch (c.driver) {
    case "sqlite":
      return c.filePath;
    case "bigquery":
      return `project ${c.database}${c.host ? ` @ ${c.host}` : ""}`;
    case "redis":
      return `${c.host}:${c.port} · db${c.database || 0}`;
    case "mongodb":
      if (c.uri) return c.uri.replace(/\/\/[^@/]*@/, "//");
  }
  return `${c.user ? c.user + "@" : ""}${c.host}:${c.port}${c.database ? "/" + c.database : ""}`;
}

function ImportUrl(props: { onClose: () => void; onParsed: (cfg: ConnectionConfig) => void }) {
  const [url, setUrl] = useState("");
  const [error, setError] = useState("");
  const submit = () => {
    try {
      props.onParsed(parseConnectionUrl(url));
    } catch (e) {
      setError(errorMessage(e));
    }
  };
  return (
    <Modal
      title="Import from URL"
      onClose={props.onClose}
      footer={
        <>
          <div className="spacer" />
          <button onClick={props.onClose}>Cancel</button>
          <button className="primary" onClick={submit} disabled={!url.trim()}>
            Import
          </button>
        </>
      }
    >
      <input
        style={{ width: "100%" }}
        className="mono"
        autoFocus
        placeholder="postgresql://user:password@host:5432/database?sslmode=require"
        value={url}
        onChange={(e) => setUrl(e.target.value)}
        onKeyDown={(e) => e.key === "Enter" && submit()}
      />
      <p className="faint" style={{ fontSize: 12 }}>
        Supports postgres://, mysql://, mariadb://, sqlite://, redis://, rediss://, mongodb:// and mongodb+srv:// URLs.
      </p>
      {error && <div className="notice err">{error}</div>}
    </Modal>
  );
}
