# HuginnTables

A fast desktop database client for PostgreSQL, MySQL/MariaDB and SQLite, built with
Tauri 2 (Rust) and React.

## Features (Phase 1)

- **Connections** — saved connections with color tags, import from `postgres://` / `mysql://` /
  `sqlite://` URLs, test before saving. Passwords and SSH secrets are kept in the system keychain,
  never in `connections.json`.
- **SSH tunnels** — password or private-key auth (with passphrase). Unknown hosts are added to
  `~/.ssh/known_hosts`; a changed host key is refused.
- **TLS** — disable / prefer / require (no verification) / verify-full.
- **Browse** — database and schema switcher, searchable table/view list (⌘K), paged data grid
  (100–5000 rows), sort by clicking headers, filter rules or a raw `WHERE` (⌘F), structure view.
- **Staged edits** — edit cells inline or in the row detail panel, set NULL, insert (⌘I) and
  delete (⌫) rows. Nothing is written until you **Commit** (⌘S); changes run in one transaction
  and roll back entirely on error. **Preview SQL** (⇧⌘P) shows the exact statements. Tables
  without a primary key are matched on every column and only one row is touched per change.
- **SQL editor** — CodeMirror with dialect highlighting and schema-aware autocomplete. ⌘↵ runs the
  statement under the cursor (or the selection), ⇧⌘↵ runs everything, ⌘. cancels on the server.
  Multiple result sets, errors stop the batch, editor contents are remembered per connection.
- **Tabs** — ⌘T new query, ⌘W close, ⌘1…9 / ⌃Tab to switch; several connections open at once.

## Develop

```bash
npm install
npm run tauri dev
```

`npm run dev` alone serves the UI in a normal browser at http://localhost:1420 with an in-memory
mock backend (`src/lib/mockBackend.ts`), handy for UI work. It is never included in builds.

Build a release app: `npm run tauri build`.

## Layout

| Path | What |
| --- | --- |
| `src-tauri/src/drivers/` | One driver per engine (`tokio-postgres`, `mysql_async`, `rusqlite`): run SQL, transactions, cancel |
| `src-tauri/src/dialect.rs` | All engine-specific SQL: quoting, catalog queries, paging, filters, change statements |
| `src-tauri/src/session.rs` | A live connection (driver + optional SSH tunnel) and browsing operations |
| `src-tauri/src/ssh.rs` | Local port forwarding over SSH (`russh`) |
| `src-tauri/src/store.rs` | Saved connections + keychain secrets |
| `src/lib/staging.ts` | Staged grid edits → change sets |
| `src/lib/sqlSplit.ts` | Statement splitting for "run current statement" |
| `src/components/` | React UI (Glide Data Grid, CodeMirror 6) |

## Tests

```bash
cd src-tauri && cargo test                 # unit tests
../scripts/test-db.sh                      # start Postgres, MySQL and an SSH bastion in Docker
cargo test -- --ignored --test-threads=1   # end-to-end tests against them
../scripts/test-db.sh down
```
