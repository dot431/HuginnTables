import { DriverKind } from "./api";

/** Statements that change data or schema start with one of these. */
const SQL_WRITES = new Set([
  "INSERT", "UPDATE", "DELETE", "MERGE", "UPSERT", "REPLACE", "CREATE", "ALTER", "DROP", "TRUNCATE",
  "RENAME", "GRANT", "REVOKE", "COMMENT", "CALL", "DO", "COPY", "LOAD", "IMPORT", "REFRESH", "REINDEX",
  "VACUUM", "CLUSTER", "EXPORT",
]);

/** Redis commands that change keys or server state. */
const REDIS_WRITES = new Set([
  "SET", "SETNX", "SETEX", "PSETEX", "MSET", "MSETNX", "GETSET", "GETDEL", "GETEX", "APPEND", "SETRANGE",
  "INCR", "INCRBY", "INCRBYFLOAT", "DECR", "DECRBY", "DEL", "UNLINK", "EXPIRE", "EXPIREAT", "PEXPIRE",
  "PEXPIREAT", "PERSIST", "RENAME", "RENAMENX", "MOVE", "COPY", "RESTORE", "HSET", "HSETNX", "HMSET", "HDEL",
  "HINCRBY", "HINCRBYFLOAT", "LPUSH", "LPUSHX", "RPUSH", "RPUSHX", "LPOP", "RPOP", "LSET", "LREM", "LTRIM",
  "LINSERT", "LMOVE", "RPOPLPUSH", "BLPOP", "BRPOP", "BLMOVE", "SADD", "SREM", "SPOP", "SMOVE", "SINTERSTORE",
  "SUNIONSTORE", "SDIFFSTORE", "ZADD", "ZREM", "ZINCRBY", "ZPOPMIN", "ZPOPMAX", "ZREMRANGEBYRANK",
  "ZREMRANGEBYSCORE", "ZREMRANGEBYLEX", "ZUNIONSTORE", "ZINTERSTORE", "ZDIFFSTORE", "ZRANGESTORE", "XADD",
  "XDEL", "XTRIM", "XGROUP", "XACK", "XCLAIM", "XAUTOCLAIM", "PFADD", "PFMERGE", "SETBIT", "BITOP", "BITFIELD",
  "GEOADD", "JSON.SET", "JSON.DEL", "FLUSHDB", "FLUSHALL", "EVAL", "EVALSHA", "FCALL", "CONFIG", "SWAPDB",
]);

/** MongoDB shell methods that change documents, indexes or collections. */
const MONGO_WRITES =
  /\.\s*(insert|insertOne|insertMany|update|updateOne|updateMany|replaceOne|delete|deleteOne|deleteMany|remove|findOneAndUpdate|findOneAndReplace|findOneAndDelete|findAndModify|bulkWrite|drop|dropDatabase|dropIndex|dropIndexes|createIndex|createIndexes|createCollection|renameCollection)\s*\(/;

/**
 * The statement without comments and with every quoted string or identifier
 * blanked, so only keywords and names are left. MySQL and BigQuery also have
 * # comments and backslash escapes.
 */
export function stripSql(sql: string, mysql = false): string {
  let out = "";
  let i = 0;
  while (i < sql.length) {
    const ch = sql[i];
    const next = sql[i + 1];
    if ((ch === "-" && next === "-") || (mysql && ch === "#")) {
      const nl = sql.indexOf("\n", i);
      i = nl === -1 ? sql.length : nl;
      out += " ";
      continue;
    }
    if (ch === "/" && next === "*") {
      const end = sql.indexOf("*/", i + 2);
      i = end === -1 ? sql.length : end + 2;
      out += " ";
      continue;
    }
    if (ch === "'" || ch === '"' || ch === "`") {
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
      out += " _ ";
      continue;
    }
    if (ch === "$" && !mysql) {
      const tag = /^\$[A-Za-z_]*\$/.exec(sql.slice(i, i + 64));
      if (tag) {
        const end = sql.indexOf(tag[0], i + tag[0].length);
        i = end === -1 ? sql.length : end + tag[0].length;
        out += " _ ";
        continue;
      }
    }
    out += ch;
    i++;
  }
  return out;
}

const usesMysqlSyntax = (driver: DriverKind) => driver === "mysql" || driver === "bigquery";

/** Upper-cased words of the statement, with parenthesis depth. */
function words(sql: string, driver: DriverKind): { word: string; depth: number }[] {
  const code = stripSql(sql, usesMysqlSyntax(driver)).toUpperCase();
  const out: { word: string; depth: number }[] = [];
  let depth = 0;
  for (const m of code.matchAll(/[A-Z0-9_]+|[()]/g)) {
    if (m[0] === "(") depth++;
    else if (m[0] === ")") depth = Math.max(0, depth - 1);
    else out.push({ word: m[0], depth });
  }
  return out;
}

/** Whether running `sql` may change data or schema. Unknown statements count as reads. */
export function isWriteStatement(sql: string, driver: DriverKind): boolean {
  if (driver === "redis") {
    const [name = "", sub = ""] = sql.trim().toUpperCase().split(/\s+/);
    return REDIS_WRITES.has(name) && !(name === "CONFIG" && sub === "GET");
  }
  if (driver === "mongodb") return MONGO_WRITES.test(sql);
  const ws = words(sql, driver);
  const first = ws[0]?.word ?? "";
  if (SQL_WRITES.has(first)) return true;
  // WITH … DELETE/UPDATE/INSERT, and EXPLAIN ANALYZE, which runs the statement.
  if (first === "WITH" || (first === "EXPLAIN" && ws.some((w) => w.word === "ANALYZE")))
    return ws.some((w) => w.word === "INSERT" || w.word === "UPDATE" || w.word === "DELETE" || w.word === "MERGE");
  return false;
}

/** An UPDATE or DELETE without a top-level WHERE: it touches every row of the table. */
export function lacksWhere(sql: string, driver: DriverKind): boolean {
  if (driver === "redis" || driver === "mongodb") return false;
  const ws = words(sql, driver);
  const first = ws[0]?.word;
  if (first !== "UPDATE" && first !== "DELETE") return false;
  return !ws.some((w) => w.word === "WHERE" && w.depth === 0);
}
