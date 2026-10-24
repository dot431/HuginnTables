import { useEffect, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { api, ConnectionConfig, errorMessage, SessionInfo } from "./lib/api";
import { ConnectionsHome } from "./components/ConnectionsHome";
import { EnvBadges, ProductionBar } from "./components/Safety";
import { SettingsModal } from "./components/Settings";
import { Workspace } from "./components/Workspace";
import { Toasts, toast } from "./components/ui";

interface OpenSession {
  info: SessionInfo;
  color: string;
}

export default function App() {
  const [sessions, setSessions] = useState<OpenSession[]>([]);
  const [active, setActive] = useState<string | null>(null);
  const [connecting, setConnecting] = useState<string | null>(null);
  const [settingsOpen, setSettingsOpen] = useState(false);

  // ⌘, opens Settings: the native menu item in the app, a key handler in the browser dev mode.
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.metaKey && e.key === ",") {
        e.preventDefault();
        setSettingsOpen(true);
      }
    };
    window.addEventListener("keydown", onKey);
    // The browser mock backend has no event plugin; there the key handler is enough.
    const unlisten = listen("open-settings", () => setSettingsOpen(true)).catch(() => undefined);
    return () => {
      window.removeEventListener("keydown", onKey);
      unlisten.then((f) => f?.());
    };
  }, []);

  const connect = async (cfg: ConnectionConfig) => {
    setConnecting(cfg.id);
    try {
      const info = await api.connect(cfg);
      setSessions((s) => [...s, { info, color: cfg.color }]);
      setActive(info.sessionId);
    } catch (e) {
      toast(`Could not connect to ${cfg.name}\n${errorMessage(e)}`, "error");
    } finally {
      setConnecting(null);
    }
  };

  const close = async (id: string) => {
    await api.disconnect(id).catch(() => {});
    setSessions((s) => s.filter((x) => x.info.sessionId !== id));
    if (active === id) setActive(null);
  };

  return (
    <div className="app">
      {sessions.length > 0 && (
        <div className="session-strip">
          <div className={`session-chip ${active === null ? "active" : ""}`} onClick={() => setActive(null)}>
            <span>Connections</span>
          </div>
          {sessions.map(({ info, color }) => (
            <div
              key={info.sessionId}
              className={`session-chip ${active === info.sessionId ? "active" : ""} ${info.production ? "production" : ""}`}
              onClick={() => setActive(info.sessionId)}
              title={`${info.name} — ${info.serverVersion}`}
            >
              <span className="dot" style={{ background: color }} />
              <span className="name">{info.name}</span>
              <EnvBadges production={info.production} short />
              <span
                className="close"
                onClick={(e) => {
                  e.stopPropagation();
                  close(info.sessionId);
                }}
              >
                ×
              </span>
            </div>
          ))}
        </div>
      )}
      {active === null && <ConnectionsHome connecting={connecting} onConnect={connect} onOpenSettings={() => setSettingsOpen(true)} />}
      {sessions.map(({ info, color }) => (
        <div
          key={info.sessionId}
          style={{ display: active === info.sessionId ? "flex" : "none", flexDirection: "column", flex: 1, minHeight: 0 }}
        >
          {info.production && <ProductionBar info={info} />}
          <Workspace session={info} color={color} active={active === info.sessionId} onClose={() => close(info.sessionId)} />
        </div>
      ))}
      {settingsOpen && <SettingsModal onClose={() => setSettingsOpen(false)} />}
      <Toasts />
    </div>
  );
}
