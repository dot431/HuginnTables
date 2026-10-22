import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { api, DriverKind, errorMessage, SessionInfo, TableInfo } from "../lib/api";
import { QueryTab } from "./QueryTab";
import { TableTab } from "./TableTab";
import { ContextMenu, DriverBadge, MenuState, toast, useDragSize } from "./ui";

type Tab =
  | { id: number; kind: "table"; schema: string; table: string; readOnly: boolean }
  | { id: number; kind: "query"; title: string; initialSql?: string; slot: number };

const KIND_ICON: Record<TableInfo["kind"], string> = {
  table: "▦",
  view: "◫",
  materialized_view: "◩",
  foreign_table: "⇄",
  keyspace: "⚿",
  collection: "▤",
};

const EDITABLE_KINDS: TableInfo["kind"][] = ["table", "collection", "keyspace"];

/** Sidebar groups per object kind. */
const GROUPS: { label: string; kinds: TableInfo["kind"][] }[] = [
  { label: "Keys", kinds: ["keyspace"] },
  { label: "Collections", kinds: ["collection"] },
  { label: "Tables", kinds: ["table", "foreign_table"] },
  { label: "Views", kinds: ["view", "materialized_view"] },
];

const SCHEMA_LABEL: Record<DriverKind, string> = {
  postgres: "Schema", mysql: "Database", sqlite: "Schema", redis: "Database", mongodb: "Database", bigquery: "Dataset",
};

/** Redis "tables" are key patterns; "*" reads better as "All keys". */
const displayName = (t: { name: string }) => (t.name === "*" ? "All keys" : t.name);

export function Workspace(props: { session: SessionInfo; color: string; active: boolean; onClose: () => void }) {
  const [info, setInfo] = useState(props.session);
  const sessionId = info.sessionId;
  const [databases, setDatabases] = useState<string[]>([]);
  const [schemas, setSchemas] = useState<string[]>([]);
  const [schema, setSchema] = useState(info.schema || (info.driver === "sqlite" ? "main" : ""));
  const [tables, setTables] = useState<TableInfo[]>([]);
  const [tablesLoading, setTablesLoading] = useState(false);
  const [search, setSearch] = useState("");
  const [completion, setCompletion] = useState<Record<string, string[]>>({});
  const [tabs, setTabs] = useState<Tab[]>([]);
  const [activeTab, setActiveTab] = useState<number | null>(null);
  const [pending, setPending] = useState<Record<number, number>>({});
  const [menu, setMenu] = useState<MenuState | null>(null);
  const nextId = useRef(1);
  const sidebar = useDragSize(250, 180, 480, "x");
  const searchRef = useRef<HTMLInputElement>(null);

  const loadSchemas = useCallback(async () => {
    try {
      const [list, dbs] = await Promise.all([api.listSchemas(sessionId), api.listDatabases(sessionId)]);
      setSchemas(list);
      setDatabases(dbs);
      setSchema((s) => (s && list.includes(s) ? s : list[0] ?? ""));
    } catch (e) {
      toast(errorMessage(e), "error");
    }
  }, [sessionId]);

  const loadTables = useCallback(async () => {
    if (!schema) {
      setTables([]);
      return;
    }
    setTablesLoading(true);
    try {
      const [list, cols] = await Promise.all([api.listTables(sessionId, schema), api.schemaColumns(sessionId, schema)]);
      setTables(list);
      setCompletion(cols);
    } catch (e) {
      toast(errorMessage(e), "error");
      setTables([]);
    } finally {
      setTablesLoading(false);
    }
  }, [sessionId, schema]);

  useEffect(() => void loadSchemas(), [loadSchemas]);
  useEffect(() => void loadTables(), [loadTables]);

  const openTable = (t: TableInfo, newTab = false) => {
    const existing = tabs.find((x) => x.kind === "table" && x.schema === schema && x.table === t.name);
    if (existing && !newTab) {
      setActiveTab(existing.id);
      return;
    }
    const id = nextId.current++;
    setTabs((ts) => [...ts, { id, kind: "table", schema, table: t.name, readOnly: info.readOnly || !EDITABLE_KINDS.includes(t.kind) }]);
    setActiveTab(id);
  };

  const openQuery = (initialSql?: string) => {
    const used = new Set(tabs.filter((t) => t.kind === "query").map((t) => (t as { slot: number }).slot));
    let slot = 1;
    while (used.has(slot)) slot++;
    const id = nextId.current++;
    setTabs((ts) => [...ts, { id, kind: "query", title: `Query ${slot}`, initialSql, slot }]);
    setActiveTab(id);
  };

  const closeTab = (id: number) => {
    const n = pending[id] ?? 0;
    if (n > 0 && !window.confirm(`This tab has ${n} uncommitted change${n === 1 ? "" : "s"}. Close it anyway?`)) return;
    setTabs((ts) => {
      const idx = ts.findIndex((t) => t.id === id);
      const rest = ts.filter((t) => t.id !== id);
      if (activeTab === id) setActiveTab(rest[Math.min(idx, rest.length - 1)]?.id ?? null);
      return rest;
    });
    setPending(({ [id]: _, ...p }) => p);
  };

  const anyPending = Object.values(pending).some((n) => n > 0);

  const changeDatabase = async (db: string) => {
    if (anyPending && !window.confirm("Switching database closes table tabs and discards uncommitted changes. Continue?")) return;
    try {
      const next = await api.switchDatabase(sessionId, db);
      setInfo(next);
      setTabs((ts) => ts.filter((t) => t.kind === "query"));
      setPending({});
      setSchema(next.schema);
      await loadSchemas();
    } catch (e) {
      toast(errorMessage(e), "error");
    }
  };

  const changeSchema = async (s: string) => {
    try {
      await api.useSchema(sessionId, s);
      setSchema(s);
    } catch (e) {
      toast(errorMessage(e), "error");
    }
  };

  // Workspace-level shortcuts.
  useEffect(() => {
    if (!props.active) return;
    const onKey = (e: KeyboardEvent) => {
      if (!e.metaKey && !e.ctrlKey) return;
      const k = e.key.toLowerCase();
      if (k === "t") {
        e.preventDefault();
        openQuery();
      } else if (k === "w") {
        e.preventDefault();
        if (activeTab !== null) closeTab(activeTab);
      } else if (k === "k" && !e.shiftKey) {
        e.preventDefault();
        searchRef.current?.focus();
        searchRef.current?.select();
      } else if (/^[1-9]$/.test(e.key)) {
        e.preventDefault();
        const t = tabs[Number(e.key) - 1];
        if (t) setActiveTab(t.id);
      } else if (e.key === "Tab" && e.ctrlKey && tabs.length) {
        e.preventDefault();
        const idx = tabs.findIndex((t) => t.id === activeTab);
        setActiveTab(tabs[(idx + (e.shiftKey ? -1 : 1) + tabs.length) % tabs.length].id);
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  });

  const filtered = useMemo(() => {
    const q = search.toLowerCase();
    return tables.filter((t) => t.name.toLowerCase().includes(q));
  }, [tables, search]);
  const selectQuery = (t: string) => {
    switch (info.driver) {
      case "mysql":
        return `SELECT * FROM \`${t}\` LIMIT 100;`;
      case "bigquery":
        return `SELECT * FROM \`${schema}.${t}\` LIMIT 100;`;
      case "redis":
        return `SCAN 0 MATCH ${t} COUNT 100`;
      case "mongodb":
        return `db.getCollection(${JSON.stringify(t)}).find({}).limit(100)`;
      default:
        return schema && schema !== "public" && schema !== "main" ? `SELECT * FROM "${schema}"."${t}" LIMIT 100;` : `SELECT * FROM "${t}" LIMIT 100;`;
    }
  };

  const tableMenu = (e: React.MouseEvent, t: TableInfo) => {
    e.preventDefault();
    setMenu({
      x: e.clientX,
      y: e.clientY,
      items: [
        { label: "Open", action: () => openTable(t) },
        { label: "Open in new tab", action: () => openTable(t, true) },
        { label: "New query", action: () => openQuery(selectQuery(t.name)) },
        { separator: true, label: "" },
        { label: "Copy name", action: () => navigator.clipboard.writeText(t.name) },
      ],
    });
  };

  const renderItem = (t: TableInfo) => {
    const current = tabs.find((x) => x.id === activeTab);
    const isActive = current?.kind === "table" && current.schema === schema && current.table === t.name;
    return (
      <div
        key={t.name}
        className={`table-item ${isActive ? "active" : ""}`}
        onClick={() => openTable(t)}
        onContextMenu={(e) => tableMenu(e, t)}
        title={`${t.name} (${t.kind.replace("_", " ")})`}
      >
        <span className="ti-icon">{KIND_ICON[t.kind]}</span>
        <span className="ti-name">{displayName(t)}</span>
      </div>
    );
  };

  return (
    <div className="workspace">
      <div className="sidebar" style={{ width: sidebar.size }}>
        <div className="sidebar-head">
          <div className="row" style={{ gap: 8 }}>
            <DriverBadge driver={info.driver} color={props.color} size={26} />
            <div style={{ minWidth: 0 }}>
              <div style={{ fontWeight: 600, overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" }}>{info.name}</div>
              <div className="faint" style={{ fontSize: 11, overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" }}>
                {info.serverVersion}
              </div>
            </div>
          </div>
          {databases.length > 0 && (
            <select value={info.database} onChange={(e) => changeDatabase(e.target.value)} title="Database">
              {databases.map((d) => (
                <option key={d} value={d}>{d}</option>
              ))}
            </select>
          )}
          {schemas.length > 1 && (
            <select value={schema} onChange={(e) => changeSchema(e.target.value)} title={SCHEMA_LABEL[info.driver]}>
              {schemas.map((s) => (
                <option key={s} value={s}>{s}</option>
              ))}
            </select>
          )}
        </div>
        <input
          ref={searchRef}
          className="sidebar-search"
          placeholder={`Search ${info.driver === "redis" ? "key prefixes" : info.driver === "mongodb" ? "collections" : "tables"}  ⌘K`}
          value={search}
          onChange={(e) => setSearch(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter" && filtered[0]) openTable(filtered[0]);
            if (e.key === "Escape") setSearch("");
          }}
        />
        <div className="table-list">
          {tablesLoading && tables.length === 0 ? (
            <div className="row" style={{ padding: 10 }}>
              <div className="spinner" />
            </div>
          ) : (
            <>
              {GROUPS.map((g) => {
                const items = filtered.filter((t) => g.kinds.includes(t.kind));
                return items.length ? (
                  <div key={g.label}>
                    <div className="group-label">{g.label}</div>
                    {items.map(renderItem)}
                  </div>
                ) : null;
              })}
              {!tablesLoading && filtered.length === 0 && (
                <div className="faint" style={{ padding: "10px 8px" }}>{search ? "No matches" : "No tables"}</div>
              )}
            </>
          )}
        </div>
        <div className="sidebar-foot">
          <span>{tables.length} objects</span>
          <div className="spacer" />
          <button className="ghost icon" title="Refresh" onClick={() => (loadSchemas(), loadTables())}>↻</button>
          <button className="ghost icon" title="New query (⌘T)" onClick={() => openQuery()}>SQL</button>
        </div>
      </div>
      <div className={`resizer ${sidebar.dragging ? "dragging" : ""}`} onMouseDown={sidebar.onMouseDown} />

      <div className="main">
        {tabs.length > 0 && (
          <div className="tabbar">
            {tabs.map((t) => (
              <div
                key={t.id}
                className={`tab ${t.id === activeTab ? "active" : ""}`}
                onClick={() => setActiveTab(t.id)}
                onMouseDown={(e) => e.button === 1 && closeTab(t.id)}
                title={t.kind === "table" ? `${t.schema}.${t.table}` : t.title}
              >
                <span className="faint">{t.kind === "table" ? (t.readOnly ? "◫" : "▦") : "›_"}</span>
                <span className="tab-title">{t.kind === "table" ? displayName({ name: t.table }) : t.title}</span>
                {(pending[t.id] ?? 0) > 0 && <span className="tab-dirty" title="Uncommitted changes" />}
                <span
                  className="close"
                  onClick={(e) => {
                    e.stopPropagation();
                    closeTab(t.id);
                  }}
                >
                  ×
                </span>
              </div>
            ))}
            <div className="tab-add" onClick={() => openQuery()} title="New query (⌘T)">+</div>
          </div>
        )}
        {tabs.length === 0 && (
          <div className="welcome">
            <div>
              <p style={{ fontSize: 15, color: "var(--text)" }}>
                Pick {info.driver === "redis" ? "a key prefix" : info.driver === "mongodb" ? "a collection" : "a table"} on the left, or start a query.
              </p>
              <p>
                <span className="kbd">⌘T</span> new query · <span className="kbd">⌘K</span> find table ·{" "}
                <span className="kbd">⌘W</span> close tab
              </p>
              <button className="primary" onClick={() => openQuery()}>New Query</button>
            </div>
          </div>
        )}
        {tabs.map((t) => (
          <div key={t.id} style={{ display: t.id === activeTab ? "flex" : "none", flex: 1, minHeight: 0, flexDirection: "column" }}>
            {t.kind === "table" ? (
              <TableTab
                sessionId={sessionId}
                driver={info.driver}
                schema={t.schema}
                table={t.table}
                readOnly={t.readOnly}
                active={props.active && t.id === activeTab}
                onPendingChange={(n) => setPending((p) => (p[t.id] === n ? p : { ...p, [t.id]: n }))}
                onOpenQuery={openQuery}
              />
            ) : (
              <QueryTab
                sessionId={sessionId}
                driver={info.driver}
                storageKey={`huginn.query.${info.connectionId}.${t.slot}`}
                initialSql={t.initialSql}
                schema={completion}
                defaultSchema={schema}
                active={props.active && t.id === activeTab}
                onSchemaChanged={loadTables}
              />
            )}
          </div>
        ))}
      </div>
      <ContextMenu menu={menu} onClose={() => setMenu(null)} />
    </div>
  );
}
