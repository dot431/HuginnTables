import { ReactNode, useCallback, useEffect, useRef, useState } from "react";
import { DriverKind } from "../lib/api";

// ---- toasts --------------------------------------------------------------

type ToastKind = "info" | "error" | "success";
interface ToastItem {
  id: number;
  kind: ToastKind;
  text: string;
}

let nextToast = 1;
const listeners = new Set<(items: ToastItem[]) => void>();
let toastItems: ToastItem[] = [];

export function toast(text: string, kind: ToastKind = "info") {
  const id = nextToast++;
  toastItems = [...toastItems, { id, kind, text }];
  listeners.forEach((l) => l(toastItems));
  setTimeout(() => {
    toastItems = toastItems.filter((t) => t.id !== id);
    listeners.forEach((l) => l(toastItems));
  }, kind === "error" ? 8000 : 3500);
}

export function Toasts() {
  const [items, setItems] = useState(toastItems);
  useEffect(() => {
    listeners.add(setItems);
    return () => void listeners.delete(setItems);
  }, []);
  return (
    <div className="toasts">
      {items.map((t) => (
        <div key={t.id} className={`toast ${t.kind}`}>
          {t.text}
        </div>
      ))}
    </div>
  );
}

// ---- modal ---------------------------------------------------------------

export function Modal(props: {
  title: ReactNode;
  children: ReactNode;
  footer?: ReactNode;
  onClose: () => void;
  wide?: boolean;
}) {
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") props.onClose();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [props.onClose]);
  return (
    <div className="modal-backdrop" onMouseDown={(e) => e.target === e.currentTarget && props.onClose()}>
      <div className={`modal ${props.wide ? "wide" : ""}`} role="dialog">
        <div className="modal-header">{props.title}</div>
        <div className="modal-body">{props.children}</div>
        {props.footer && <div className="modal-footer">{props.footer}</div>}
      </div>
    </div>
  );
}

// ---- context menu --------------------------------------------------------

export interface MenuItem {
  label: string;
  shortcut?: string;
  disabled?: boolean;
  action?: () => void;
  separator?: boolean;
}

export interface MenuState {
  x: number;
  y: number;
  items: MenuItem[];
}

export function ContextMenu({ menu, onClose }: { menu: MenuState | null; onClose: () => void }) {
  const ref = useRef<HTMLDivElement>(null);
  useEffect(() => {
    if (!menu) return;
    const close = (e: MouseEvent) => {
      if (!ref.current?.contains(e.target as Node)) onClose();
    };
    const key = (e: KeyboardEvent) => e.key === "Escape" && onClose();
    window.addEventListener("mousedown", close, true);
    window.addEventListener("keydown", key);
    window.addEventListener("blur", onClose);
    return () => {
      window.removeEventListener("mousedown", close, true);
      window.removeEventListener("keydown", key);
      window.removeEventListener("blur", onClose);
    };
  }, [menu, onClose]);
  if (!menu) return null;
  const x = Math.min(menu.x, window.innerWidth - 200);
  const y = Math.min(menu.y, window.innerHeight - menu.items.length * 28 - 12);
  return (
    <div ref={ref} className="context-menu" style={{ left: x, top: y }}>
      {menu.items.map((item, i) =>
        item.separator ? (
          <div key={i} className="sep" />
        ) : (
          <div
            key={i}
            className={`item ${item.disabled ? "disabled" : ""}`}
            onClick={() => {
              onClose();
              item.action?.();
            }}
          >
            <span>{item.label}</span>
            {item.shortcut && <span className="faint">{item.shortcut}</span>}
          </div>
        ),
      )}
    </div>
  );
}

// ---- layout helpers ------------------------------------------------------

/** Drag handle state for a resizable pane. `axis` is the direction it grows. */
export function useDragSize(initial: number, min: number, max: number, axis: "x" | "y", invert = false) {
  const [size, setSize] = useState(initial);
  const [dragging, setDragging] = useState(false);
  const onMouseDown = useCallback(
    (e: React.MouseEvent) => {
      e.preventDefault();
      const start = axis === "x" ? e.clientX : e.clientY;
      const startSize = size;
      setDragging(true);
      const move = (ev: MouseEvent) => {
        const delta = (axis === "x" ? ev.clientX : ev.clientY) - start;
        setSize(Math.max(min, Math.min(max, startSize + (invert ? -delta : delta))));
      };
      const up = () => {
        setDragging(false);
        window.removeEventListener("mousemove", move);
        window.removeEventListener("mouseup", up);
      };
      window.addEventListener("mousemove", move);
      window.addEventListener("mouseup", up);
    },
    [size, min, max, axis, invert],
  );
  return { size, dragging, onMouseDown };
}

const BADGE: Record<DriverKind, string> = { postgres: "Pg", mysql: "My", sqlite: "Lt", redis: "Rd", mongodb: "Mg", bigquery: "BQ" };

export function DriverBadge(props: { driver: DriverKind; color: string; mark?: string; size?: number }) {
  const size = props.size ?? 36;
  const text = props.mark ?? BADGE[props.driver];
  return (
    <div
      className="conn-badge"
      style={{ background: props.color || "#888", width: size, height: size, fontSize: size * (text.length > 2 ? 0.28 : 0.34) }}
    >
      {text}
    </div>
  );
}

/** The Rune H mark. Below 48 px it switches to the small cut: heavier bars and a joined diagonal. */
export function HuginnMark({ size = 48 }: { size?: number }) {
  const ink = { fill: "var(--mark-ink)" };
  return size < 48 ? (
    <svg width={size} height={size} viewBox="80 80 352 352" aria-hidden>
      <rect x="120" y="88" width="88" height="336" rx="20" style={ink} />
      <rect x="304" y="88" width="88" height="336" rx="20" fill="#3A8D93" />
      <polygon points="208,184 304,240 304,328 208,272" fill="#F0705A" />
    </svg>
  ) : (
    <svg width={size} height={size} viewBox="88 88 336 336" aria-hidden>
      <rect x="136" y="96" width="64" height="320" rx="16" style={ink} />
      <rect x="312" y="96" width="64" height="320" rx="16" fill="#3A8D93" />
      <polygon
        points="216,199.4 296,246.6 296,312.6 216,265.4"
        fill="#F0705A"
        stroke="#F0705A"
        strokeWidth="8"
        strokeLinejoin="round"
      />
    </svg>
  );
}
