import { useEffect, useRef, useState } from "react";
import { Cell } from "../lib/api";
import { Modal } from "./ui";

// ---- columns popover -----------------------------------------------------

/** Toolbar button with a popover for hiding columns and freezing the first few. */
export function ColumnsButton(props: {
  columns: string[];
  hidden: string[];
  frozen: number;
  onHidden: (hidden: string[]) => void;
  onFrozen: (frozen: number) => void;
}) {
  const [open, setOpen] = useState(false);
  const ref = useRef<HTMLDivElement>(null);

  useEffect(() => {
    if (!open) return;
    const close = (e: MouseEvent) => {
      if (!ref.current?.contains(e.target as Node)) setOpen(false);
    };
    const key = (e: KeyboardEvent) => e.key === "Escape" && setOpen(false);
    window.addEventListener("mousedown", close, true);
    window.addEventListener("keydown", key);
    return () => {
      window.removeEventListener("mousedown", close, true);
      window.removeEventListener("keydown", key);
    };
  }, [open]);

  const hiddenHere = props.columns.filter((c) => props.hidden.includes(c));
  const visibleCount = props.columns.length - hiddenHere.length;
  const maxFrozen = Math.max(props.frozen, Math.min(10, visibleCount - 1));
  const toggle = (name: string) =>
    props.onHidden(props.hidden.includes(name) ? props.hidden.filter((h) => h !== name) : [...props.hidden, name]);

  return (
    <div className="popover-anchor" ref={ref}>
      <button
        className={hiddenHere.length || props.frozen ? "primary" : ""}
        onClick={() => setOpen((v) => !v)}
        title="Show, hide and freeze columns"
      >
        Columns{hiddenHere.length ? ` (${visibleCount}/${props.columns.length})` : ""}
      </button>
      {open && (
        <div className="popover columns-popover">
          <div className="row">
            <span className="muted">Freeze first</span>
            <select value={props.frozen} onChange={(e) => props.onFrozen(Number(e.target.value))}>
              {Array.from({ length: maxFrozen + 1 }, (_, n) => (
                <option key={n} value={n}>{n}</option>
              ))}
            </select>
            <span className="muted">column{props.frozen === 1 ? "" : "s"}</span>
          </div>
          <div className="sep" />
          <div className="pop-list">
            {props.columns.map((c) => {
              const shown = !props.hidden.includes(c);
              return (
                <label key={c} className="check-row">
                  <input type="checkbox" checked={shown} disabled={shown && visibleCount <= 1} onChange={() => toggle(c)} />
                  <span>{c}</span>
                </label>
              );
            })}
          </div>
          <div className="sep" />
          <div className="row">
            <button disabled={!hiddenHere.length} onClick={() => props.onHidden([])}>Show all</button>
            <div className="spacer" />
          </div>
        </div>
      )}
    </div>
  );
}

// ---- bulk set ------------------------------------------------------------

/** Asks for one value to stage in a column across several rows. */
export function BulkSetDialog(props: {
  column: string;
  rows: number;
  onApply: (value: Cell) => void;
  onClose: () => void;
}) {
  const [text, setText] = useState("");
  const [isNull, setIsNull] = useState(false);
  const apply = () => props.onApply(isNull ? null : text);
  return (
    <Modal
      title={`Set ${props.column} for ${props.rows} rows`}
      onClose={props.onClose}
      footer={
        <>
          <div className="spacer" />
          <button onClick={props.onClose}>Cancel</button>
          <button className="primary" onClick={apply}>Set {props.rows} rows</button>
        </>
      }
    >
      <div className="col" style={{ gap: 10 }}>
        <input
          autoFocus
          className="mono"
          value={isNull ? "" : text}
          disabled={isNull}
          placeholder={isNull ? "NULL" : "Value (empty means an empty string)"}
          onChange={(e) => setText(e.target.value)}
          onKeyDown={(e) => e.key === "Enter" && apply()}
        />
        <label className="row">
          <input type="checkbox" checked={isNull} onChange={(e) => setIsNull(e.target.checked)} />
          NULL
        </label>
      </div>
    </Modal>
  );
}

// ---- selection summary ---------------------------------------------------

const NUMBER = /^[-+]?(\d+\.?\d*|\.\d+)([eE][-+]?\d+)?$/;

const fmt = (n: number) => n.toLocaleString(undefined, { maximumFractionDigits: 6 });

/** "12 cells · Sum 40 · Avg 3.33 · Min 1 · Max 9" for a multi-cell selection. */
export function summarize(values: (Cell | undefined)[]): string | null {
  if (values.length < 2) return null;
  let count = 0;
  let sum = 0;
  let min = Infinity;
  let max = -Infinity;
  for (const v of values) {
    if (v == null) continue;
    const t = v.trim();
    if (!NUMBER.test(t)) continue;
    const n = Number(t);
    if (!Number.isFinite(n)) continue;
    count++;
    sum += n;
    if (n < min) min = n;
    if (n > max) max = n;
  }
  const cells = `${values.length.toLocaleString()} cells`;
  if (!count) return cells;
  const numeric = count < values.length ? ` · ${count.toLocaleString()} numeric` : "";
  return `${cells}${numeric} · Sum ${fmt(sum)} · Avg ${fmt(sum / count)} · Min ${fmt(min)} · Max ${fmt(max)}`;
}
