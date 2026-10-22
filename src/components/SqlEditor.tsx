import { useEffect, useRef } from "react";
import { EditorState, Compartment, Prec } from "@codemirror/state";
import { EditorView, keymap, lineNumbers, highlightActiveLine, highlightActiveLineGutter, drawSelection, placeholder } from "@codemirror/view";
import { defaultKeymap, history, historyKeymap, indentWithTab } from "@codemirror/commands";
import { autocompletion, closeBrackets, closeBracketsKeymap, completionKeymap, CompletionContext, CompletionResult } from "@codemirror/autocomplete";
import { bracketMatching, syntaxHighlighting, defaultHighlightStyle, indentOnInput } from "@codemirror/language";
import { sql, PostgreSQL, MySQL, SQLite, SQLDialect, StandardSQL } from "@codemirror/lang-sql";
import { javascript } from "@codemirror/lang-javascript";
import { oneDark } from "@codemirror/theme-one-dark";
import { DriverKind } from "../lib/api";
import { useDarkMode } from "./ui";

export interface SqlEditorHandle {
  view: EditorView | null;
}

const BigQuerySQL = SQLDialect.define({
  ...StandardSQL.spec,
  identifierQuotes: "`",
  keywords: `${StandardSQL.spec.keywords} qualify struct array unnest except replace safe_cast window partition cluster`,
  types: "int64 float64 numeric bignumeric bool string bytes date datetime time timestamp geography json interval struct array",
});

const SQL_DIALECTS = { postgres: PostgreSQL, mysql: MySQL, sqlite: SQLite, bigquery: BigQuerySQL };

const REDIS_COMMANDS = (
  "GET SET MGET MSET DEL EXISTS EXPIRE PERSIST TTL PTTL TYPE KEYS SCAN RENAME INCR INCRBY DECR APPEND STRLEN " +
  "HGET HSET HGETALL HDEL HKEYS HVALS HLEN HINCRBY HSCAN LPUSH RPUSH LPOP RPOP LRANGE LLEN LREM LSET " +
  "SADD SREM SMEMBERS SISMEMBER SCARD SSCAN ZADD ZREM ZRANGE ZREVRANGE ZSCORE ZCARD ZINCRBY ZSCAN " +
  "XADD XRANGE XLEN INFO DBSIZE SELECT FLUSHDB PING CONFIG CLIENT MEMORY OBJECT UNLINK COPY"
).split(" ");

const MONGO_METHODS = [
  "find", "findOne", "aggregate", "countDocuments", "estimatedDocumentCount", "distinct", "insertOne", "insertMany",
  "updateOne", "updateMany", "replaceOne", "deleteOne", "deleteMany", "drop", "getIndexes", "createIndex",
];

function redisCompletions(ctx: CompletionContext): CompletionResult | null {
  const word = ctx.matchBefore(/^\s*\w*/);
  if (!word || (word.from === word.to && !ctx.explicit)) return null;
  return { from: word.from + (word.text.length - word.text.trimStart().length), options: REDIS_COMMANDS.map((c) => ({ label: c, type: "keyword" })) };
}

function mongoCompletions(collections: string[]) {
  return (ctx: CompletionContext): CompletionResult | null => {
    const method = ctx.matchBefore(/db\.[\w$.-]+\.\w*/);
    if (method) {
      const dot = method.text.lastIndexOf(".");
      if (dot > 2) {
        return { from: method.from + dot + 1, options: MONGO_METHODS.map((m) => ({ label: m, type: "method", apply: `${m}()` })) };
      }
    }
    const coll = ctx.matchBefore(/db\.[\w$-]*/);
    if (coll) return { from: coll.from + 3, options: collections.map((c) => ({ label: c, type: "class" })) };
    return null;
  };
}

const PLACEHOLDERS: Record<DriverKind, string> = {
  postgres: "Write SQL here.  ⌘↵ runs the statement under the cursor (or the selection), ⇧⌘↵ runs everything.",
  mysql: "Write SQL here.  ⌘↵ runs the statement under the cursor (or the selection), ⇧⌘↵ runs everything.",
  sqlite: "Write SQL here.  ⌘↵ runs the statement under the cursor (or the selection), ⇧⌘↵ runs everything.",
  bigquery: "GoogleSQL, e.g. SELECT * FROM `dataset.table` LIMIT 100.  Queries are billed by bytes scanned.",
  redis: "One command per line, e.g. HGETALL user:1  ·  ⌘↵ runs the current line, ⇧⌘↵ runs all",
  mongodb: "db.users.find({ age: { $gt: 30 } }).sort({ name: 1 }).limit(20)  ·  also: show collections, use <db>, db.runCommand({...})",
};

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
    switch (props.driver) {
      case "redis":
        return autocompletion({ override: [redisCompletions] });
      case "mongodb":
        return [javascript(), autocompletion({ override: [mongoCompletions(Object.keys(props.schema))] })];
      default:
        return sql({ dialect: SQL_DIALECTS[props.driver], schema: props.schema, upperCaseKeywords: true });
    }
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
          ...(props.driver === "redis" || props.driver === "mongodb" ? [] : [autocompletion({ activateOnTyping: true })]),
          syntaxHighlighting(defaultHighlightStyle, { fallback: true }),
          placeholder(PLACEHOLDERS[props.driver]),
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
