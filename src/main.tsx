import React from "react";
import ReactDOM from "react-dom/client";
import "@glideapps/glide-data-grid/dist/index.css";
import "./styles.css";
import App from "./App";

// Database values must arrive exactly as typed: no smart quotes, dashes or autocorrect,
// including in editors created by libraries (the grid's cell editor).
document.addEventListener("focusin", (e) => {
  const el = e.target;
  if (el instanceof HTMLInputElement || el instanceof HTMLTextAreaElement) {
    el.setAttribute("autocorrect", "off");
    el.setAttribute("autocapitalize", "off");
    el.spellcheck = false;
  }
});

if (import.meta.env.DEV && !("__TAURI_INTERNALS__" in window)) {
  const { installMockBackend } = await import("./lib/mockBackend");
  installMockBackend();
}

ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
  <React.StrictMode>
    <App />
  </React.StrictMode>,
);
