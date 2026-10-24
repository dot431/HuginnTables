import { useEffect, useState } from "react";
import { api, Cell, errorMessage, ForeignKeyInfo } from "../lib/api";
import { FilterState } from "./FilterBar";
import { Modal } from "./ui";

/**
 * Rules selecting the rows whose `columns` hold `values`, or null when a value
 * is NULL or unknown: a key with a NULL in it references nothing.
 */
export function keyFilter(columns: string[], values: (Cell | undefined)[]): FilterState | null {
  if (values.length !== columns.length || values.some((v) => v === null || v === undefined)) return null;
  return { filters: columns.map((column, i) => ({ column, op: "=", value: values[i] as string })), rawWhere: "" };
}

const short = (v: Cell | undefined) =>
  v === null || v === undefined ? "NULL" : v.length > 40 ? v.slice(0, 40) + "…" : v;

/** "Open orders where id = 7" for following `fk` from a row holding `values` in its columns. */
export function openParentLabel(fk: ForeignKeyInfo, values: (Cell | undefined)[]): string {
  const cond = fk.parentColumns.map((c, i) => `${c} = ${short(values[i])}`).join(" and ");
  return `Open ${fk.parentTable} where ${cond}`;
}

/**
 * Lists the foreign keys pointing at `table` with how many rows reference the
 * row whose values `valueOf` gives; clicking one opens those rows.
 */
export function ReferencedBy(props: {
  sessionId: string;
  table: string;
  incoming: ForeignKeyInfo[];
  valueOf: (column: string) => Cell | undefined;
  onOpen: (fk: ForeignKeyInfo, filter: FilterState) => void;
  onClose: () => void;
}) {
  const filters = props.incoming.map((fk) => keyFilter(fk.columns, fk.parentColumns.map(props.valueOf)));
  // Per key: the row count, an error, or undefined while counting.
  const [counts, setCounts] = useState<(number | string | undefined)[]>([]);

  useEffect(() => {
    let live = true;
    const set = (i: number, v: number | string) =>
      live && setCounts((c) => {
        const next = [...c];
        next[i] = v;
        return next;
      });
    props.incoming.forEach((fk, i) => {
      const filter = filters[i];
      if (!filter) {
        set(i, 0);
        return;
      }
      api
        .countRows(props.sessionId, {
          schema: fk.schema, table: fk.table, limit: 0, offset: 0,
          filters: filter.filters, rawWhere: "", sort: null, keyOrder: [],
        })
        .then((n) => set(i, n))
        .catch((e) => set(i, errorMessage(e)));
    });
    return () => void (live = false);
    // Counted once, for the row the dialog was opened on.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  return (
    <Modal
      title="Referenced by"
      wide
      onClose={props.onClose}
      footer={
        <>
          <div className="spacer" />
          <button onClick={props.onClose}>Close</button>
        </>
      }
    >
      {props.incoming.length === 0 ? (
        <p className="muted" style={{ margin: 0 }}>No foreign key points at <b>{props.table}</b>.</p>
      ) : (
        <>
          <p style={{ marginTop: 0 }}>
            Rows in other tables whose foreign keys point at this row of <b>{props.table}</b>. Click one to open them.
          </p>
          <table className="plain">
            <thead>
              <tr>
                <th>Table</th>
                <th>Columns</th>
                <th>On delete</th>
                <th style={{ textAlign: "right" }}>Rows</th>
              </tr>
            </thead>
            <tbody>
              {props.incoming.map((fk, i) => {
                const n = counts[i];
                const filter = filters[i];
                const open = filter && n !== 0 ? () => props.onOpen(fk, filter) : undefined;
                return (
                  <tr key={`${fk.schema}.${fk.table}.${fk.name}`} className={open ? "clickable" : "muted"} onClick={open} title={fk.name}>
                    <td><b>{fk.table}</b></td>
                    <td className="mono muted">{fk.columns.join(", ")} → {fk.parentColumns.join(", ")}</td>
                    <td className="mono muted">{fk.onDelete}</td>
                    <td style={{ textAlign: "right" }}>
                      {n === undefined ? "…" : typeof n === "string" ? <span className="error-text" title={n}>error</span> : n.toLocaleString()}
                    </td>
                  </tr>
                );
              })}
            </tbody>
          </table>
        </>
      )}
    </Modal>
  );
}
