import { useState } from "react";

/** Children shown per object or array before the rest is summed up. */
const MAX_CHILDREN = 500;

/** The parsed value when `text` holds a JSON object or array, otherwise undefined. */
export function parseJsonContainer(text: string | null | undefined): object | undefined {
  if (!text) return undefined;
  const first = text.trimStart()[0];
  if (first !== "{" && first !== "[") return undefined;
  try {
    const value: unknown = JSON.parse(text);
    return value !== null && typeof value === "object" ? value : undefined;
  } catch {
    return undefined;
  }
}

/** A read-only, collapsible view of a parsed JSON value. */
export function JsonTree({ value }: { value: unknown }) {
  return (
    <div className="json-tree">
      <JsonNode name={null} value={value} depth={0} />
    </div>
  );
}

function JsonNode(props: { name: string | null; value: unknown; depth: number }) {
  const { name, value, depth } = props;
  const [open, setOpen] = useState(depth < 2);
  const key = name !== null && <span className="jt-key">{name}: </span>;
  const indent = { paddingLeft: depth * 14 };

  if (value === null || typeof value !== "object") {
    return (
      <div className="jt-row" style={indent}>
        <span className="jt-caret" />
        {key}
        <Scalar value={value} />
      </div>
    );
  }

  const isArray = Array.isArray(value);
  const entries: [string, unknown][] = isArray ? value.map((v, i) => [String(i), v]) : Object.entries(value);
  return (
    <>
      <div className="jt-row jt-toggle" style={indent} onClick={() => setOpen((o) => !o)}>
        <span className="jt-caret">{open ? "▾" : "▸"}</span>
        {key}
        <span className="faint">{isArray ? `[${entries.length}]` : `{${entries.length}}`}</span>
      </div>
      {open &&
        entries.slice(0, MAX_CHILDREN).map(([k, v]) => <JsonNode key={k} name={k} value={v} depth={depth + 1} />)}
      {open && entries.length > MAX_CHILDREN && (
        <div className="jt-row faint" style={{ paddingLeft: (depth + 1) * 14 }}>
          <span className="jt-caret" />… {(entries.length - MAX_CHILDREN).toLocaleString()} more
        </div>
      )}
    </>
  );
}

function Scalar({ value }: { value: unknown }) {
  if (value === null) return <span className="jt-null">null</span>;
  if (typeof value === "string") return <span className="jt-str">"{value}"</span>;
  if (typeof value === "number" || typeof value === "boolean") return <span className="jt-num">{String(value)}</span>;
  return <span>{String(value)}</span>;
}
