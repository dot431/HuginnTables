export interface Statement {
  text: string;
  from: number;
  to: number;
}

/**
 * Splits a script into statements on top-level semicolons, skipping quotes,
 * comments and Postgres dollar-quoted bodies. Comment-only chunks are dropped.
 */
export function splitStatements(sql: string, mysql = false): Statement[] {
  const out: Statement[] = [];
  let start = 0;
  let hasCode = false;
  let i = 0;

  const push = (end: number) => {
    if (hasCode) {
      const raw = sql.slice(start, end);
      const lead = raw.length - raw.trimStart().length;
      const text = raw.trim();
      out.push({ text, from: start + lead, to: start + lead + text.length });
    }
    hasCode = false;
  };

  while (i < sql.length) {
    const ch = sql[i];
    const next = sql[i + 1];

    if (ch === "-" && next === "-" || (mysql && ch === "#")) {
      const nl = sql.indexOf("\n", i);
      i = nl === -1 ? sql.length : nl + 1;
      continue;
    }
    if (ch === "/" && next === "*") {
      const end = sql.indexOf("*/", i + 2);
      i = end === -1 ? sql.length : end + 2;
      continue;
    }
    if (ch === "'" || ch === '"' || ch === "`") {
      hasCode = true;
      i++;
      while (i < sql.length) {
        if (mysql && sql[i] === "\\" && ch !== "`") {
          i += 2;
          continue;
        }
        if (sql[i] === ch) {
          if (sql[i + 1] === ch) {
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
    if (ch === "$" && !mysql) {
      const tag = /^\$[A-Za-z_]*\$/.exec(sql.slice(i, i + 64));
      if (tag) {
        hasCode = true;
        const end = sql.indexOf(tag[0], i + tag[0].length);
        i = end === -1 ? sql.length : end + tag[0].length;
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

/** The statement under the cursor, or the closest one before it. */
export function statementAt(sql: string, pos: number, mysql = false): Statement | undefined {
  const all = splitStatements(sql, mysql);
  let best: Statement | undefined;
  for (const s of all) {
    if (s.from <= pos) best = s;
    if (pos <= s.to + 1) return s.from <= pos + 1 ? s : best ?? s;
  }
  return best;
}
