export interface Statement {
  text: string;
  from: number;
  to: number;
}

/**
 * How scripts are split into statements:
 * - `sql`: top-level semicolons; '…', "…", `…`, -- and /* *\/ comments, Postgres $tag$ bodies.
 * - `mysql`: like sql, plus # comments and backslash escapes in strings.
 * - `mongo`: JavaScript-ish: // and /* *\/ comments, backslash escapes; a newline also ends a
 *   call when all brackets are closed and the next line doesn't continue it with `.`.
 * - `redis`: one command per line; # starts a comment line.
 */
export type SplitMode = "sql" | "mysql" | "mongo" | "redis";

export function splitStatements(sql: string, mode: SplitMode = "sql"): Statement[] {
  if (mode === "redis") return splitLines(sql);
  const mysql = mode === "mysql";
  const mongo = mode === "mongo";
  const out: Statement[] = [];
  let start = 0;
  let hasCode = false;
  let depth = 0;
  let i = 0;

  const push = (end: number) => {
    if (hasCode) {
      const raw = sql.slice(start, end);
      const lead = raw.length - raw.trimStart().length;
      const text = raw.trim();
      out.push({ text, from: start + lead, to: start + lead + text.length });
    }
    hasCode = false;
    depth = 0;
  };

  while (i < sql.length) {
    const ch = sql[i];
    const next = sql[i + 1];

    if ((ch === "-" && next === "-" && !mongo) || (mysql && ch === "#") || (mongo && ch === "/" && next === "/")) {
      const nl = sql.indexOf("\n", i);
      i = nl === -1 ? sql.length : nl;
      // Comments before any code aren't part of the statement.
      if (!hasCode) start = i;
      continue;
    }
    if (ch === "/" && next === "*") {
      const end = sql.indexOf("*/", i + 2);
      i = end === -1 ? sql.length : end + 2;
      if (!hasCode) start = i;
      continue;
    }
    if (ch === "'" || ch === '"' || ch === "`") {
      hasCode = true;
      i++;
      while (i < sql.length) {
        if ((mysql || mongo) && sql[i] === "\\" && ch !== "`") {
          i += 2;
          continue;
        }
        if (sql[i] === ch) {
          if (sql[i + 1] === ch && !mongo) {
            i += 2;
            continue;
          }
          break;
        }
        i++;
      }
      i++;
      continue;
    }
    if (ch === "$" && mode === "sql") {
      const tag = /^\$[A-Za-z_]*\$/.exec(sql.slice(i, i + 64));
      if (tag) {
        hasCode = true;
        const end = sql.indexOf(tag[0], i + tag[0].length);
        i = end === -1 ? sql.length : end + tag[0].length;
        continue;
      }
    }
    if (mongo) {
      if ("([{".includes(ch)) depth++;
      else if (")]}".includes(ch)) depth = Math.max(0, depth - 1);
      else if (ch === "\n" && depth === 0 && hasCode && !/^\s*\./.test(sql.slice(i + 1))) {
        push(i);
        start = i + 1;
        i++;
        continue;
      }
    }
    if (ch === ";") {
      push(i);
      start = i + 1;
      i++;
      continue;
    }
    if (!/\s/.test(ch)) hasCode = true;
    i++;
  }
  push(sql.length);
  return out;
}

function splitLines(text: string): Statement[] {
  const out: Statement[] = [];
  let offset = 0;
  for (const line of text.split("\n")) {
    const trimmed = line.trim();
    if (trimmed && !trimmed.startsWith("#")) {
      const from = offset + (line.length - line.trimStart().length);
      out.push({ text: trimmed, from, to: from + trimmed.length });
    }
    offset += line.length + 1;
  }
  return out;
}

/** The statement under the cursor, or the closest one before it. */
export function statementAt(sql: string, pos: number, mode: SplitMode = "sql"): Statement | undefined {
  const all = splitStatements(sql, mode);
  let best: Statement | undefined;
  for (const s of all) {
    if (s.from <= pos) best = s;
    if (pos <= s.to + 1) return s.from <= pos + 1 ? s : best ?? s;
  }
  return best;
}
