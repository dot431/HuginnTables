import { useEffect, useRef } from "react";
import { EditorState, Compartment, Prec } from "@codemirror/state";
import { EditorView, keymap, lineNumbers, highlightActiveLine, highlightActiveLineGutter, drawSelection, placeholder } from "@codemirror/view";
import { defaultKeymap, history, historyKeymap, indentWithTab } from "@codemirror/commands";
import { autocompletion, closeBrackets, closeBracketsKeymap, completionKeymap } from "@codemirror/autocomplete";
import { bracketMatching, syntaxHighlighting, defaultHighlightStyle, indentOnInput } from "@codemirror/language";
import { sql, PostgreSQL, MySQL, SQLite, SQLConfig } from "@codemirror/lang-sql";
import { oneDark } from "@codemirror/theme-one-dark";
import { DriverKind } from "../lib/api";
import { useDarkMode } from "./ui";

export interface SqlEditorHandle {
  view: EditorView | null;
}

const DIALECTS = { postgres: PostgreSQL, mysql: MySQL, sqlite: SQLite };

const baseTheme = EditorView.theme({
  "&": { height: "100%" },
  ".cm-content": { padding: "8px 0" },
  ".cm-gutters": { borderRight: "none", background: "transparent" },
});

export function SqlEditor(props: {
  driver: DriverKind;
  initial: string;
  schema: Record<string, string[]>;
  defaultSchema: string;
  onChange: (text: string) => void;
  onRun: (view: EditorView, all: boolean) => void;
  handle: React.MutableRefObject<SqlEditorHandle>;
}) {
  const host = useRef<HTMLDivElement>(null);
  const dark = useDarkMode();
  const langConf = useRef(new Compartment());
  const themeConf = useRef(new Compartment());
  // Keep callbacks current without rebuilding the editor.
  const callbacks = useRef(props);
  callbacks.current = props;

  const language = () => {
    const config: SQLConfig = {
      dialect: DIALECTS[props.driver],
      schema: props.schema,
      upperCaseKeywords: true,
    };
    return sql(config);
  };

  useEffect(() => {
    const run = (all: boolean) => (view: EditorView) => {
      callbacks.current.onRun(view, all);
      return true;
    };
    const view = new EditorView({
      parent: host.current!,
      state: EditorState.create({
        doc: props.initial,
        extensions: [
          Prec.highest(
            keymap.of([
              { key: "Mod-Enter", run: run(false) },
              { key: "Shift-Mod-Enter", run: run(true) },
            ]),
          ),
          lineNumbers(),
          highlightActiveLineGutter(),
          highlightActiveLine(),
          drawSelection(),
          history(),
          indentOnInput(),
          bracketMatching(),
          closeBrackets(),
          autocompletion({ activateOnTyping: true }),
          syntaxHighlighting(defaultHighlightStyle, { fallback: true }),
          placeholder("Write SQL here.  ⌘↵ runs the statement under the cursor (or the selection), ⇧⌘↵ runs everything."),
          keymap.of([...closeBracketsKeymap, ...defaultKeymap, ...historyKeymap, ...completionKeymap, indentWithTab]),
          langConf.current.of(language()),
          themeConf.current.of(dark ? oneDark : []),
          baseTheme,
          EditorView.updateListener.of((u) => {
            if (u.docChanged) callbacks.current.onChange(u.state.doc.toString());
          }),
        ],
      }),
    });
    props.handle.current.view = view;
    view.focus();
    return () => {
      props.handle.current.view = null;
      view.destroy();
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  useEffect(() => {
    props.handle.current.view?.dispatch({ effects: langConf.current.reconfigure(language()) });
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [props.schema, props.driver]);

  useEffect(() => {
    props.handle.current.view?.dispatch({ effects: themeConf.current.reconfigure(dark ? oneDark : []) });
  }, [dark]);

  return <div ref={host} style={{ height: "100%", display: "flex", flexDirection: "column" }} />;
}
