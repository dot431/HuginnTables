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

export function useDarkMode() {
  const query = "(prefers-color-scheme: dark)";
  const [dark, setDark] = useState(() => window.matchMedia(query).matches);
  useEffect(() => {
    const mq = window.matchMedia(query);
    const on = () => setDark(mq.matches);
    mq.addEventListener("change", on);
    return () => mq.removeEventListener("change", on);
  }, []);
  return dark;
}

const BADGE: Record<DriverKind, string> = { postgres: "Pg", mysql: "My", sqlite: "Lt" };

export function DriverBadge({ driver, color, size = 36 }: { driver: DriverKind; color: string; size?: number }) {
  return (
    <div
      className="conn-badge"
      style={{ background: color || "#888", width: size, height: size, fontSize: size * 0.34 }}
    >
      {BADGE[driver]}
    </div>
  );
}

export function RavenMark({ size = 44 }: { size?: number }) {
  return (
    <svg width={size} height={size} viewBox="0 0 64 64" aria-hidden>
      <rect width="64" height="64" rx="14" fill="#1f2433" />
      <path
        d="M14 40c6-1 10-5 13-10 3-6 8-11 15-11 4 0 7 2 8 5l5 2-5 2c-1 6-6 11-13 13l3 7h-4l-3-6c-5 1-12 1-19-2z"
        fill="#e8ecf6"
      />
      <circle cx="44" cy="24" r="1.8" fill="#1f2433" />
      <path d="M14 46h36" stroke="#5b8def" strokeWidth="3" strokeLinecap="round" />
    </svg>
  );
}
