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
import { ThemeId, useTheme } from "../lib/theme";

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

const HUGINN_LIGHT: Partial<Theme> = {
  ...LIGHT,
  accentColor: "#2e6e74",
  accentFg: "#fbfaf7",
  accentLight: "rgba(58, 141, 147, 0.16)",
  textDark: "#12201f",
  textMedium: "#4e5b5c",
  textLight: "#566262",
  textHeader: "#4e5b5c",
  textHeaderSelected: "#fbfaf7",
  textBubble: "#12201f",
  bgIconHeader: "#4e5b5c",
  fgIconHeader: "#fbfaf7",
  bgCell: "#fbfaf7",
  bgCellMedium: "#f5f3ee",
  bgHeader: "#eeebe5",
  bgHeaderHasFocus: "#dfe3df",
  bgHeaderHovered: "#e5e1da",
  bgBubble: "#eeebe5",
  bgBubbleSelected: "#d3dedb",
  bgSearchResult: "rgba(240, 112, 90, 0.3)",
  borderColor: "rgba(18, 32, 31, 0.09)",
  horizontalBorderColor: "rgba(18, 32, 31, 0.06)",
  linkColor: "#2e6e74",
};

const HUGINN_DARK: Partial<Theme> = {
  ...LIGHT,
  // Fjord Teal lifted slightly so Raven Ink text on a selected header stays above 4.5:1.
  accentColor: "#3f959b",
  accentFg: "#12201f",
  accentLight: "rgba(58, 141, 147, 0.26)",
  textDark: "#fbfaf7",
  textMedium: "#b4c0be",
  textLight: "#93a3a1",
  textHeader: "#b4c0be",
  textHeaderSelected: "#12201f",
  textBubble: "#fbfaf7",
  bgIconHeader: "#b4c0be",
  fgIconHeader: "#12201f",
  bgCell: "#12201f",
  bgCellMedium: "#172826",
  bgHeader: "#172826",
  bgHeaderHasFocus: "#24474a",
  bgHeaderHovered: "#1f3331",
  bgBubble: "#243937",
  bgBubbleSelected: "#24474a",
  bgSearchResult: "rgba(240, 112, 90, 0.32)",
  borderColor: "rgba(251, 250, 247, 0.08)",
  horizontalBorderColor: "rgba(251, 250, 247, 0.05)",
  drilldownBorder: "rgba(251, 250, 247, 0.2)",
  linkColor: "#8ccbcf",
};

const IS_MAC = navigator.platform.startsWith("Mac");

const STATE_BG = {
  light: { edited: "#fff3c4", inserted: "#dcf2e4", deleted: "#f8dcda" },
  dark: { edited: "#4a4224", inserted: "#24402f", deleted: "#4d2a28" },
  "huginn-light": { edited: "#f8ebcc", inserted: "#dcebe9", deleted: "#f9dfd8" },
  "huginn-dark": { edited: "#3d3b25", inserted: "#1d3d3f", deleted: "#45302b" },
};

function gridTheme(theme: ThemeId, dark: boolean) {
  if (theme === "huginn-light") return { theme: HUGINN_LIGHT, stateBg: STATE_BG["huginn-light"] };
  if (theme === "huginn-dark") return { theme: HUGINN_DARK, stateBg: STATE_BG["huginn-dark"] };
  return dark ? { theme: DARK, stateBg: STATE_BG.dark } : { theme: LIGHT, stateBg: STATE_BG.light };
}

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
  /** ⌘-click on a cell (Ctrl-click off macOS, where Ctrl-click is a right-click). */
  onCellMetaClick?: (col: number, row: number) => void;
  /** Bump to reset widths, e.g. when a different table is loaded. */
  layoutKey?: string;
  /** Bump to clear the selection, e.g. after the rows were reloaded. */
  selectionKey?: number;
  /** Scrolls this row into view whenever the object changes. */
  reveal?: { row: number };
}) {
  const ref = useRef<DataEditorRef>(null);
  const appearance = useTheme();
  const { theme, stateBg } = gridTheme(appearance.theme, appearance.dark);
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
      onCellClicked={([col, row], e) => {
        if (IS_MAC ? e.metaKey : e.ctrlKey) props.onCellMetaClick?.(col, row);
      }}
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
