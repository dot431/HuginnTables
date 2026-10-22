import { useState } from "react";
import { Filter, FilterOp } from "../lib/api";

const OPS: { op: FilterOp; label: string }[] = [
  { op: "=", label: "=" },
  { op: "<>", label: "≠" },
  { op: "<", label: "<" },
  { op: ">", label: ">" },
  { op: "<=", label: "≤" },
  { op: ">=", label: "≥" },
  { op: "contains", label: "contains" },
  { op: "notContains", label: "not contains" },
  { op: "like", label: "LIKE" },
  { op: "in", label: "IN (a, b)" },
  { op: "isNull", label: "IS NULL" },
  { op: "isNotNull", label: "IS NOT NULL" },
];

export interface FilterState {
  filters: Filter[];
  rawWhere: string;
}

export function FilterBar(props: {
  columns: string[];
  initial: FilterState;
  onApply: (state: FilterState) => void;
}) {
  const [filters, setFilters] = useState<Filter[]>(
    props.initial.filters.length ? props.initial.filters : [{ column: props.columns[0] ?? "", op: "=", value: "" }],
  );
  const [rawMode, setRawMode] = useState(!!props.initial.rawWhere);
  const [rawWhere, setRawWhere] = useState(props.initial.rawWhere);

  const update = (i: number, patch: Partial<Filter>) =>
    setFilters((fs) => fs.map((f, j) => (j === i ? { ...f, ...patch } : f)));

  const apply = () =>
    props.onApply(
      rawMode
        ? { filters: [], rawWhere }
        : {
            filters: filters.filter((f) => f.column && (f.value !== "" || f.op === "isNull" || f.op === "isNotNull")),
            rawWhere: "",
          },
    );

  const onKey = (e: React.KeyboardEvent) => {
    if (e.key === "Enter") {
      e.preventDefault();
      apply();
    }
  };

  return (
    <div className="filters">
      {rawMode ? (
        <div className="filter-row">
          <span className="muted mono">WHERE</span>
          <input
            className="mono"
            autoFocus
            placeholder="price > 10 AND status = 'active'"
            value={rawWhere}
            onChange={(e) => setRawWhere(e.target.value)}
            onKeyDown={onKey}
          />
          <button onClick={apply}>Apply</button>
          <button className="ghost" onClick={() => setRawMode(false)}>Rules</button>
        </div>
      ) : (
        filters.map((f, i) => (
          <div className="filter-row" key={i}>
            <select value={f.column} onChange={(e) => update(i, { column: e.target.value })}>
              {props.columns.map((c) => (
                <option key={c} value={c}>{c}</option>
              ))}
            </select>
            <select value={f.op} onChange={(e) => update(i, { op: e.target.value as FilterOp })}>
              {OPS.map((o) => (
                <option key={o.op} value={o.op}>{o.label}</option>
              ))}
            </select>
            <input
              autoFocus={i === filters.length - 1}
              disabled={f.op === "isNull" || f.op === "isNotNull"}
              value={f.value}
              placeholder="value"
              onChange={(e) => update(i, { value: e.target.value })}
              onKeyDown={onKey}
            />
            <button className="icon" title="Remove rule" onClick={() => setFilters((fs) => (fs.length > 1 ? fs.filter((_, j) => j !== i) : [{ ...fs[0], value: "" }]))}>−</button>
            <button className="icon" title="Add rule" onClick={() => setFilters((fs) => [...fs.slice(0, i + 1), { column: f.column, op: "=", value: "" }, ...fs.slice(i + 1)])}>+</button>
            {i === 0 && (
              <>
                <button onClick={apply}>Apply</button>
                <button className="ghost" onClick={() => setRawMode(true)}>SQL</button>
                <button
                  className="ghost"
                  onClick={() => {
                    setFilters([{ column: props.columns[0] ?? "", op: "=", value: "" }]);
                    props.onApply({ filters: [], rawWhere: "" });
                  }}
                >
                  Clear
                </button>
              </>
            )}
          </div>
        ))
      )}
    </div>
  );
}
