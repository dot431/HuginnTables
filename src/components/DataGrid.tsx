import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import DataEditor, {
  CompactSelection,
  DataEditorRef,
  EditableGridCell,
  GridCell,
  GridCellKind,
  GridColumn,
  GridSelection,
  Item,
  Theme,
} from "@glideapps/glide-data-grid";
import { Cell } from "../lib/api";
import { useDarkMode } from "./ui";

export type CellState = "edited" | "inserted" | "deleted" | undefined;

export interface GridValue {
  /** `undefined` means "not set" (a column left to its DEFAULT on a new row). */
  value: Cell | undefined;
  state?: CellState;
}

export interface GridColumnDef {
  name: string;
  badge?: string;
}

const LIGHT: Partial<Theme> = {
  accentColor: "#3f73e8",
  accentLight: "rgba(63, 115, 232, 0.14)",
  textDark: "#1d1d22",
  textMedium: "#55555f",
  textLight: "#a0a0aa",
  textHeader: "#55555f",
  bgCell: "#ffffff",
  bgCellMedium: "#f7f7f9",
  bgHeader: "#f4f4f6",
  bgHeaderHasFocus: "#e6e8ef",
  bgHeaderHovered: "#ebebf0",
  borderColor: "rgba(0, 0, 0, 0.08)",
  horizontalBorderColor: "rgba(0, 0, 0, 0.06)",
  headerFontStyle: "600 12px",
  baseFontStyle: "12.5px",
  fontFamily: '-apple-system, BlinkMacSystemFont, "Segoe UI", Inter, sans-serif',
  editorFontSize: "12.5px",
  cellHorizontalPadding: 8,
};

const DARK: Partial<Theme> = {
  ...LIGHT,
  accentColor: "#5b8def",
  accentLight: "rgba(91, 141, 239, 0.22)",
  textDark: "#e6e6ea",
  textMedium: "#b0b0ba",
  textLight: "#686872",
  textHeader: "#b0b0ba",
  textBubble: "#e6e6ea",
  bgIconHeader: "#b0b0ba",
  fgIconHeader: "#1e1f22",
  bgCell: "#1e1f22",
  bgCellMedium: "#26272b",
  bgHeader: "#26272b",
  bgHeaderHasFocus: "#33343a",
  bgHeaderHovered: "#2f3035",
  bgBubble: "#33343a",
  bgBubbleSelected: "#3b3f4c",
  bgSearchResult: "#5b4a1a",
  borderColor: "rgba(255, 255, 255, 0.08)",
  horizontalBorderColor: "rgba(255, 255, 255, 0.05)",
  drilldownBorder: "rgba(255, 255, 255, 0.2)",
  linkColor: "#8fb1f5",
};

const STATE_BG = {
  light: { edited: "#fff3c4", inserted: "#dcf2e4", deleted: "#f8dcda" },
  dark: { edited: "#4a4224", inserted: "#24402f", deleted: "#4d2a28" },
};

function display(value: Cell | undefined): string {
  if (value === null) return "NULL";
  if (value === undefined) return "DEFAULT";
  const firstLine = value.length > 300 ? value.slice(0, 300) + "…" : value;
  return firstLine.replace(/\r?\n/g, " ↵ ");
}

export function autoWidth(title: string, sample: (Cell | undefined)[]): number {
  let chars = title.length + 3;
  for (const v of sample) {
    const len = v == null ? 4 : Math.min(v.length, 60);
    if (len > chars) chars = len;
  }
  return Math.max(70, Math.min(360, chars * 7.4 + 22));
}

export function DataGrid(props: {
  columns: GridColumnDef[];
  rowCount: number;
  getValue: (col: number, row: number) => GridValue;
  editable?: boolean;
  sort?: { column: string; desc: boolean } | null;
  onEdit?: (edits: { col: number; row: number; value: string }[]) => void;
  onDeleteRows?: (rows: number[]) => void;
  onHeaderClick?: (col: number) => void;
  onContextMenu?: (col: number, row: number, x: number, y: number, selectedRows: number[]) => void;
  onSelectRow?: (row: number | undefined) => void;
  /** Bump to reset widths, e.g. when a different table is loaded. */
  layoutKey?: string;
  /** Bump to clear the selection, e.g. after the rows were reloaded. */
  selectionKey?: number;
  /** Scrolls this row into view whenever the object changes. */
  reveal?: { row: number };
}) {
  const ref = useRef<DataEditorRef>(null);
  const dark = useDarkMode();
  const theme = dark ? DARK : LIGHT;
  const stateBg = dark ? STATE_BG.dark : STATE_BG.light;
  const [widths, setWidths] = useState<Record<string, number>>({});
  const [selection, setSelection] = useState<GridSelection>({
    columns: CompactSelection.empty(),
    rows: CompactSelection.empty(),
  });

  useEffect(() => {
    setWidths({});
    setSelection({ columns: CompactSelection.empty(), rows: CompactSelection.empty() });
  }, [props.layoutKey]);

  useEffect(() => {
    setSelection({ columns: CompactSelection.empty(), rows: CompactSelection.empty() });
    props.onSelectRow?.(undefined);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [props.selectionKey]);

  useEffect(() => {
    if (!props.reveal) return;
    const row = props.reveal.row;
    // Wait a frame so the grid knows about the new row count.
    requestAnimationFrame(() => {
      ref.current?.scrollTo(0, row, "vertical", 0, 0, { vAlign: "end" });
      setSelection({
        columns: CompactSelection.empty(),
        rows: CompactSelection.empty(),
        current: { cell: [0, row], range: { x: 0, y: row, width: 1, height: 1 }, rangeStack: [] },
      });
      props.onSelectRow?.(row);
    });
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [props.reveal]);

  const { getValue, rowCount } = props;
  const gridColumns: GridColumn[] = useMemo(
    () =>
      props.columns.map((c, i) => {
        const sortMark = props.sort?.column === c.name ? (props.sort.desc ? " ↓" : " ↑") : "";
        const title = `${c.badge ? c.badge + " " : ""}${c.name}${sortMark}`;
        const sample: (Cell | undefined)[] = [];
        for (let r = 0; r < Math.min(rowCount, 40); r++) sample.push(getValue(i, r).value);
        return { id: c.name, title, width: widths[c.name] ?? autoWidth(title, sample) };
      }),
    // Widths are sampled once per layout; re-sampling on every edit would make columns jump.
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [props.columns, props.sort, widths, props.layoutKey, rowCount > 0],
  );

  const getCellContent = useCallback(
    ([col, row]: Item): GridCell => {
      const { value, state } = getValue(col, row);
      const isPlaceholder = value === null || value === undefined;
      const bg = state ? stateBg[state] : undefined;
      return {
        kind: GridCellKind.Text,
        data: value ?? "",
        displayData: display(value),
        allowOverlay: true,
        readonly: !props.editable || state === "deleted",
        themeOverride:
          isPlaceholder || bg
            ? {
                ...(isPlaceholder ? { textDark: theme.textLight } : {}),
                ...(bg ? { bgCell: bg } : {}),
              }
            : undefined,
      };
    },
    [getValue, props.editable, stateBg, theme.textLight],
  );

  const selectedRows = (sel: GridSelection): number[] => {
    const rows = new Set<number>(sel.rows.toArray());
    if (sel.current) {
      const { y, height } = sel.current.range;
      for (let r = y; r < y + height; r++) rows.add(r);
    }
    return [...rows].sort((a, b) => a - b);
  };

  const onCellsEdited = useCallback(
    (cells: readonly { location: Item; value: EditableGridCell }[]) => {
      props.onEdit?.(
        cells
          .filter((c) => c.value.kind === GridCellKind.Text)
          .map((c) => ({ col: c.location[0], row: c.location[1], value: (c.value as { data: string }).data })),
      );
      return true;
    },
    [props.onEdit],
  );

  return (
    <DataEditor
      ref={ref}
      theme={theme}
      columns={gridColumns}
      rows={rowCount}
      getCellContent={getCellContent}
      getCellsForSelection
      rowMarkers="clickable-number"
      smoothScrollX
      smoothScrollY
      rowHeight={26}
      headerHeight={28}
      width="100%"
      height="100%"
      gridSelection={selection}
      onGridSelectionChange={(sel) => {
        setSelection(sel);
        const rows = selectedRows(sel);
        props.onSelectRow?.(rows.length === 1 ? rows[0] : sel.current ? sel.current.cell[1] : undefined);
      }}
      onCellsEdited={props.editable ? onCellsEdited : undefined}
      onPaste={props.editable}
      onDelete={(sel) => {
        if (props.editable) props.onDeleteRows?.(selectedRows(sel));
        return false;
      }}
      onColumnResize={(col, width) => setWidths((w) => ({ ...w, [col.id as string]: width }))}
      onHeaderClicked={(col) => props.onHeaderClick?.(col)}
      onCellContextMenu={([col, row], e) => {
        e.preventDefault();
        const inSelection =
          selection.rows.hasIndex(row) ||
          (selection.current &&
            row >= selection.current.range.y &&
            row < selection.current.range.y + selection.current.range.height);
        const rows = inSelection ? selectedRows(selection) : [row];
        if (!inSelection) {
          setSelection({
            columns: CompactSelection.empty(),
            rows: CompactSelection.empty(),
            current: { cell: [col, row], range: { x: col, y: row, width: 1, height: 1 }, rangeStack: [] },
          });
          props.onSelectRow?.(row);
        }
        props.onContextMenu?.(col, row, e.bounds.x + e.localEventX, e.bounds.y + e.localEventY, rows);
      }}
    />
  );
}
