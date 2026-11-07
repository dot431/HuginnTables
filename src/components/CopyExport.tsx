import { useState } from "react";
import { errorMessage, ExportFormat, SaveTarget } from "../lib/api";
import { COPY_FORMATS, CopyFormat, formatRows, RowData, SqlTarget } from "../lib/formats";
import { MenuItem, Modal, toast } from "./ui";

/**
 * "Copy as …" context-menu items for the given rows. UPDATE statements are
 * only offered when `withUpdate` is set, and need a primary key.
 */
export function copyMenuItems(count: number, data: () => RowData, target: SqlTarget, withUpdate = false): MenuItem[] {
  const many = count > 1 ? ` (${count} rows)` : "";
  return COPY_FORMATS.filter((f) => withUpdate || f.id !== "update").map((f) => ({
    label: `Copy as ${f.label}${many}`,
    disabled: f.id === "update" && !target.primaryKey?.length,
    action: () => copyRows(f.id, data(), target),
  }));
}

async function copyRows(format: CopyFormat, data: RowData, target: SqlTarget) {
  try {
    await navigator.clipboard.writeText(formatRows(format, data, target));
  } catch (e) {
    toast(errorMessage(e), "error");
  }
}

const EXPORT_FORMATS: { id: ExportFormat; label: string; ext: string; name: string }[] = [
  { id: "csv", label: "CSV", ext: "csv", name: "CSV file" },
  { id: "json", label: "JSON", ext: "json", name: "JSON file" },
  { id: "sql", label: "SQL INSERT", ext: "sql", name: "SQL script" },
];

/**
 * Picks a format (and, for tables, which rows), then runs `onExport`, which
 * asks where to save and returns the file name and rows written, or null when
 * the save dialog was cancelled.
 */
export function ExportDialog(props: {
  title: string;
  fileName: string;
  scopes?: { id: string; label: string }[];
  initialScope?: string;
  note?: string;
  onExport: (format: ExportFormat, scope: string, target: SaveTarget) => Promise<{ name: string; rows: number } | null>;
  onClose: () => void;
}) {
  const [format, setFormat] = useState<ExportFormat>("csv");
  const [scope, setScope] = useState(props.initialScope ?? props.scopes?.[0]?.id ?? "");
  const [busy, setBusy] = useState(false);

  const run = async () => {
    const f = EXPORT_FORMATS.find((x) => x.id === format)!;
    setBusy(true);
    try {
      const saved = await props.onExport(format, scope, { fileName: `${props.fileName}.${f.ext}`, filterName: f.name, extension: f.ext });
      if (!saved) return;
      const n = saved.rows;
      toast(`Exported ${n.toLocaleString()} row${n === 1 ? "" : "s"} to ${saved.name}`, "success");
      props.onClose();
    } catch (e) {
      toast(`Export failed.\n${errorMessage(e)}`, "error");
    } finally {
      setBusy(false);
    }
  };

  return (
    <Modal
      title={props.title}
      onClose={() => !busy && props.onClose()}
      footer={
        <>
          {busy && <div className="spinner" />}
          <div className="spacer" />
          <button onClick={props.onClose} disabled={busy}>Cancel</button>
          <button className="primary" onClick={run} disabled={busy}>
            {busy ? "Exporting…" : "Export…"}
          </button>
        </>
      }
    >
      <div className="form-grid">
        <label>Format</label>
        <div>
          <div className="segmented">
            {EXPORT_FORMATS.map((f) => (
              <button key={f.id} className={format === f.id ? "on" : ""} onClick={() => setFormat(f.id)} disabled={busy}>
                {f.label}
              </button>
            ))}
          </div>
        </div>
        {props.scopes && (
          <>
            <label>Rows</label>
            <div>
              <div className="segmented">
                {props.scopes.map((s) => (
                  <button key={s.id} className={scope === s.id ? "on" : ""} onClick={() => setScope(s.id)} disabled={busy}>
                    {s.label}
                  </button>
                ))}
              </div>
            </div>
          </>
        )}
        {props.note && <div className="full muted">{props.note}</div>}
      </div>
    </Modal>
  );
}

/**
 * Fallback when the clipboard can't be read directly: the user pastes into a
 * text box instead.
 */
export function PasteDialog(props: { onPaste: (text: string) => void; onClose: () => void }) {
  const [text, setText] = useState("");
  return (
    <Modal
      title="Paste rows"
      wide
      onClose={props.onClose}
      footer={
        <>
          <span className="muted">Tab- or comma-separated; a header line maps columns by name.</span>
          <div className="spacer" />
          <button onClick={props.onClose}>Cancel</button>
          <button className="primary" disabled={!text.trim()} onClick={() => (props.onClose(), props.onPaste(text))}>
            Add rows
          </button>
        </>
      }
    >
      <textarea
        className="mono"
        autoFocus
        rows={12}
        style={{ width: "100%", resize: "vertical" }}
        placeholder="Paste here (⌘V)"
        value={text}
        onChange={(e) => setText(e.target.value)}
      />
    </Modal>
  );
}
