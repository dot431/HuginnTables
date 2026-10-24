import { useState } from "react";
import { setTheme, ThemeId, THEMES, useTheme } from "../lib/theme";
import { reopenTabsEnabled, setReopenTabs } from "../lib/workspaceState";
import { Modal } from "./ui";

// Miniature of each theme: chrome, content, accent, highlight. Classic shows its light and dark halves.
const PREVIEW: Record<ThemeId, { chrome: string; content: string; line: string; accent: string; highlight: string }[]> = {
  classic: [
    { chrome: "#f4f4f6", content: "#ffffff", line: "#dedee3", accent: "#3f73e8", highlight: "#3f73e8" },
    { chrome: "#26272b", content: "#1e1f22", line: "#3a3b41", accent: "#5b8def", highlight: "#5b8def" },
  ],
  "huginn-light": [{ chrome: "#eeebe5", content: "#fbfaf7", line: "#ddd8cf", accent: "#2e6e74", highlight: "#f0705a" }],
  "huginn-dark": [{ chrome: "#172826", content: "#12201f", line: "#2a3e3c", accent: "#2e6e74", highlight: "#f0705a" }],
};

export function SettingsModal(props: { onClose: () => void }) {
  const { theme } = useTheme();
  const [reopen, setReopen] = useState(reopenTabsEnabled);
  return (
    <Modal
      title="Settings"
      onClose={props.onClose}
      footer={
        <>
          <div className="spacer" />
          <button className="primary" onClick={props.onClose}>
            Done
          </button>
        </>
      }
    >
      <div className="form-section" style={{ marginTop: 0 }}>Theme</div>
      <div className="theme-options" role="radiogroup" aria-label="Theme">
        {THEMES.map((t) => (
          <button
            key={t.id}
            role="radio"
            aria-checked={theme === t.id}
            className={`theme-option ${theme === t.id ? "on" : ""}`}
            onClick={() => setTheme(t.id)}
          >
            <div className="theme-preview">
              {PREVIEW[t.id].map((p, i) => (
                <div key={i} style={{ background: p.content }}>
                  <div style={{ background: p.chrome, borderRight: `1px solid ${p.line}` }}>
                    <i style={{ background: p.accent }} />
                  </div>
                  <div>
                    <i style={{ background: p.line }} />
                    <i style={{ background: p.line, width: "70%" }} />
                    <i style={{ background: p.highlight, width: "35%" }} />
                  </div>
                </div>
              ))}
            </div>
            <div className="theme-label">
              <div className="title">{t.name}</div>
              <div className="sub">{t.description}</div>
            </div>
          </button>
        ))}
      </div>
      <div className="form-section">Workspace</div>
      <div className="row">
        <input
          type="checkbox"
          id="reopen-tabs"
          checked={reopen}
          onChange={(e) => {
            setReopen(e.target.checked);
            setReopenTabs(e.target.checked);
          }}
        />
        <label htmlFor="reopen-tabs">Reopen tabs when connecting</label>
      </div>
      <div className="faint" style={{ marginTop: 4 }}>
        Brings back the database, schema, open tabs with their filters and sorting, and query text from the last time.
      </div>
    </Modal>
  );
}
