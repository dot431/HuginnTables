//! Turns a Laravel project's `.env` into a connection the user can review.
//!
//! Covers what Laravel itself reads (`DB_CONNECTION`, `DB_HOST`, …, `DB_URL`)
//! and the two common local setups: Sail, where `DB_HOST` names a Docker
//! Compose service published on `FORWARD_DB_PORT`, and Herd, where it is
//! already 127.0.0.1.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

use crate::model::{ConnectionConfig, DriverKind, SshConfig, SslMode};

/// Reads `<folder>/.env` (or the given file) and builds a connection from it.
pub fn read_env_connection(path: &str) -> Result<ConnectionConfig> {
    let path = expand_home(path.trim());
    let (file, project) = if path.is_dir() {
        (path.join(".env"), path.clone())
    } else {
        let parent = path.parent().map(Path::to_path_buf).unwrap_or_default();
        (path.clone(), parent)
    };
    if !file.exists() && path.is_dir() && path.join(".env.example").exists() {
        bail!(
            "{} has no .env yet. Copy .env.example to .env first, or choose the example file itself.",
            path.display()
        );
    }
    let text = std::fs::read_to_string(&file).with_context(|| format!("Could not read {}", file.display()))?;
    let file_name = file.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    connection_from_env(&parse(&text), &project, &file_name)
}

fn expand_home(path: &str) -> PathBuf {
    match (path.strip_prefix("~/"), dirs::home_dir()) {
        (Some(rest), Some(home)) => home.join(rest),
        _ => PathBuf::from(path),
    }
}

// ---- dotenv syntax --------------------------------------------------------

/// Parses dotenv text: `#` comments, an optional `export`, single quotes
/// (literal), double quotes (escapes, may span lines) and unquoted values with
/// trailing ` # comments`. `${VAR}` expands to a variable defined above it.
pub fn parse(text: &str) -> HashMap<String, String> {
    let mut vars = HashMap::new();
    let mut lines = text.trim_start_matches('\u{feff}').lines();
    while let Some(line) = lines.next() {
        let line = line.trim_start();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let line = line.strip_prefix("export ").map(str::trim_start).unwrap_or(line);
        let Some((key, rest)) = line.split_once('=') else { continue };
        let key = key.trim();
        if key.is_empty() || !key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.') {
            continue;
        }
        let rest = rest.trim_start();
        let value = match rest.chars().next() {
            Some(quote @ ('"' | '\'')) => {
                let mut raw = rest[1..].to_string();
                // An unclosed quote continues on the following lines.
                loop {
                    if let Some(end) = closing_quote(&raw, quote) {
                        raw.truncate(end);
                        break;
                    }
                    match lines.next() {
                        Some(next) => {
                            raw.push('\n');
                            raw.push_str(next);
                        }
                        None => break,
                    }
                }
                if quote == '"' { expand(&raw, &vars, true) } else { raw }
            }
            _ => expand(strip_comment(rest).trim_end(), &vars, false),
        };
        vars.insert(key.to_string(), value);
    }
    vars
}

/// Byte offset of the quote that ends a value, skipping `\"` inside double quotes.
fn closing_quote(raw: &str, quote: char) -> Option<usize> {
    let mut escaped = false;
    for (i, c) in raw.char_indices() {
        if escaped {
            escaped = false;
        } else if c == '\\' && quote == '"' {
            escaped = true;
        } else if c == quote {
            return Some(i);
        }
    }
    None
}

/// An unquoted value ends at a `#` that follows whitespace (or starts it).
fn strip_comment(value: &str) -> &str {
    let mut prev_space = true;
    for (i, c) in value.char_indices() {
        if c == '#' && prev_space {
            return &value[..i];
        }
        prev_space = c.is_whitespace();
    }
    value
}

/// Expands `${VAR}` (unknown names become empty) and, inside double quotes,
/// backslash escapes.
fn expand(raw: &str, vars: &HashMap<String, String>, escapes: bool) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut chars = raw.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\\' if escapes => match chars.next() {
                Some('n') => out.push('\n'),
                Some('r') => out.push('\r'),
                Some('t') => out.push('\t'),
                Some(e @ ('"' | '\\' | '$')) => out.push(e),
                Some(other) => {
                    out.push('\\');
                    out.push(other);
                }
                None => out.push('\\'),
            },
            '$' if chars.peek() == Some(&'{') => {
                chars.next();
                let name: String = chars.by_ref().take_while(|&c| c != '}').collect();
                if let Some(v) = vars.get(name.trim()) {
                    out.push_str(v);
                }
            }
            _ => out.push(c),
        }
    }
    out
}

// ---- Laravel's database settings -------------------------------------------

fn driver_for(name: &str) -> Option<DriverKind> {
    match name.trim().to_ascii_lowercase().as_str() {
        "pgsql" | "postgres" | "postgresql" => Some(DriverKind::Postgres),
        "mysql" | "mariadb" => Some(DriverKind::Mysql),
        "sqlite" | "sqlite3" => Some(DriverKind::Sqlite),
        _ => None,
    }
}

fn ssl_mode_for(value: &str) -> Option<SslMode> {
    match value.trim().to_ascii_lowercase().as_str() {
        "disable" | "disabled" => Some(SslMode::Disable),
        "allow" | "prefer" | "preferred" => Some(SslMode::Prefer),
        "require" | "required" => Some(SslMode::Require),
        "verify-ca" | "verify-full" | "verify_identity" => Some(SslMode::VerifyFull),
        _ => None,
    }
}

/// The parts of a `DB_URL` such as `mysql://user:secret@host:3306/app?sslmode=require`.
#[derive(Default, Debug)]
struct DbUrl {
    driver: Option<DriverKind>,
    scheme: String,
    host: String,
    port: Option<u16>,
    user: String,
    password: Option<String>,
    database: String,
    ssl_mode: Option<SslMode>,
}

fn parse_db_url(url: &str) -> Result<DbUrl> {
    let url = url.trim();
    let Some((scheme, rest)) = url.split_once(':') else { bail!("DB_URL \"{url}\" has no scheme") };
    let mut out = DbUrl { driver: driver_for(scheme), scheme: scheme.to_string(), ..Default::default() };
    if out.driver == Some(DriverKind::Sqlite) {
        // sqlite:///abs/path or sqlite:relative/path
        out.database = percent_decode(rest.strip_prefix("//").unwrap_or(rest));
        return Ok(out);
    }
    let rest = rest.strip_prefix("//").unwrap_or(rest);
    let (main, query) = rest.split_once('?').unwrap_or((rest, ""));
    let (authority, path) = main.split_once('/').unwrap_or((main, ""));
    let host_port = match authority.rsplit_once('@') {
        Some((userinfo, host_port)) => {
            let (user, password) = userinfo.split_once(':').map_or((userinfo, None), |(u, p)| (u, Some(p)));
            out.user = percent_decode(user);
            out.password = password.map(percent_decode);
            host_port
        }
        None => authority,
    };
    let (host, port) = if let Some(v6) = host_port.strip_prefix('[') {
        let (h, after) = v6.split_once(']').unwrap_or((v6, ""));
        (h, after.strip_prefix(':'))
    } else {
        host_port.rsplit_once(':').map_or((host_port, None), |(h, p)| (h, Some(p)))
    };
    out.host = percent_decode(host);
    out.port = port.filter(|p| !p.is_empty()).map(|p| p.parse()).transpose().context("DB_URL has an invalid port")?;
    out.database = percent_decode(path);
    for pair in query.split('&') {
        if let Some(("sslmode" | "ssl-mode", v)) = pair.split_once('=') {
            out.ssl_mode = ssl_mode_for(&percent_decode(v));
        }
    }
    Ok(out)
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).ok();
            if let Some(b) = hex.and_then(|h| u8::from_str_radix(h, 16).ok()) {
                out.push(b);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Under Sail, `DB_HOST` is a Compose service name that only resolves inside
/// Docker; from the Mac the database is published on 127.0.0.1.
fn is_docker_service(host: &str, project: &Path) -> bool {
    const SERVICES: &[&str] = &["mysql", "mariadb", "pgsql", "postgres", "postgresql", "db", "database"];
    if host.is_empty() || host.eq_ignore_ascii_case("localhost") || host.contains(['.', ':']) {
        return false;
    }
    SERVICES.contains(&host.to_ascii_lowercase().as_str())
        || ["docker-compose.yml", "docker-compose.yaml", "compose.yml", "compose.yaml"]
            .iter()
            .any(|f| project.join(f).exists())
}

/// "my-shop_api" → "My Shop Api".
fn humanize(folder: &str) -> String {
    folder
        .split(['-', '_', ' '])
        .filter(|w| !w.is_empty())
        .map(|w| {
            let mut c = w.chars();
            c.next().map(|f| f.to_uppercase().chain(c).collect::<String>()).unwrap_or_default()
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// "Breakroom (local)": APP_NAME unless it is still Laravel's default, then
/// the folder name; plus APP_ENV or the `.env.<suffix>` of the file.
fn connection_name(vars: &HashMap<String, String>, project: &Path, file_name: &str) -> String {
    let get = |k: &str| vars.get(k).map(|v| v.trim()).filter(|v| !v.is_empty());
    let base = get("APP_NAME")
        .filter(|n| *n != "Laravel")
        .map(str::to_string)
        .or_else(|| project.file_name().map(|f| humanize(&f.to_string_lossy())))
        .unwrap_or_else(|| "Laravel".into());
    let env = get("APP_ENV").or_else(|| file_name.strip_prefix(".env.").filter(|s| *s != "example"));
    match env {
        Some(env) => format!("{base} ({env})"),
        None => base,
    }
}

/// Builds the connection Laravel would open with these variables.
pub fn connection_from_env(vars: &HashMap<String, String>, project: &Path, file_name: &str) -> Result<ConnectionConfig> {
    let get = |k: &str| vars.get(k).map(|v| v.trim()).filter(|v| !v.is_empty());
    let url = get("DB_URL").or_else(|| get("DATABASE_URL")).map(parse_db_url).transpose()?;

    let driver = match (url.as_ref().map(|u| (u.driver, &u.scheme)), get("DB_CONNECTION")) {
        (Some((Some(d), _)), _) => d,
        (Some((None, scheme)), _) => bail!("DB_URL uses \"{scheme}\", which HuginnTables can't open"),
        (None, Some(name)) => driver_for(name).with_context(|| {
            format!("DB_CONNECTION={name} isn't supported; HuginnTables opens MySQL, MariaDB, PostgreSQL and SQLite")
        })?,
        // Laravel 11+ defaults to SQLite; older .env files always set DB_CONNECTION.
        (None, None) if get("DB_HOST").is_some() => DriverKind::Mysql,
        (None, None) => DriverKind::Sqlite,
    };
    let url = url.unwrap_or_default();
    let pick = |from_url: &str, key: &str| {
        if from_url.is_empty() { get(key).map(str::to_string) } else { Some(from_url.to_string()) }
    };

    let mut cfg = ConnectionConfig {
        id: String::new(),
        name: connection_name(vars, project, file_name),
        driver,
        color: String::new(),
        template: match driver {
            DriverKind::Postgres => "postgres",
            DriverKind::Mysql => "mysql",
            _ => "sqlite",
        }
        .into(),
        host: String::new(),
        port: 0,
        user: String::new(),
        database: String::new(),
        ssl_mode: SslMode::Prefer,
        file_path: String::new(),
        uri: String::new(),
        ssh: SshConfig { port: 22, key_path: "~/.ssh/id_ed25519".into(), ..Default::default() },
        production: false,
        read_only: false,
        password: url.password.clone().or_else(|| get("DB_PASSWORD").map(str::to_string)),
        ssh_password: None,
        ssh_passphrase: None,
    };

    if driver == DriverKind::Sqlite {
        let db = pick(&url.database, "DB_DATABASE").unwrap_or_else(|| "database/database.sqlite".into());
        let path = Path::new(&db);
        cfg.file_path = if path.is_absolute() || db == ":memory:" {
            db
        } else {
            project.join(path.strip_prefix("./").unwrap_or(path)).to_string_lossy().into_owned()
        };
        cfg.password = None;
        return Ok(cfg);
    }

    let host = pick(&url.host, "DB_HOST").unwrap_or_else(|| "127.0.0.1".into());
    let port = |key: &str| get(key).and_then(|p| p.parse::<u16>().ok());
    cfg.port = url.port.or_else(|| port("DB_PORT")).unwrap_or(driver.default_port());
    if is_docker_service(&host, project) {
        cfg.host = "127.0.0.1".into();
        cfg.port = port("FORWARD_DB_PORT").unwrap_or(cfg.port);
    } else {
        cfg.host = host;
    }
    cfg.user = pick(&url.user, "DB_USERNAME").unwrap_or_else(|| "root".into());
    cfg.database = pick(&url.database, "DB_DATABASE").unwrap_or_else(|| "laravel".into());
    cfg.ssl_mode = url.ssl_mode.or_else(|| get("DB_SSLMODE").and_then(ssl_mode_for)).unwrap_or_default();
    Ok(cfg)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(text: &str) -> HashMap<String, String> {
        parse(text)
    }

    #[test]
    fn dotenv_syntax() {
        let vars = env(concat!(
            "\u{feff}# comment\n",
            "APP_NAME=\"Break Room\"\n",
            "export APP_ENV=local\n",
            "  SPACED = value with spaces   # trailing comment\n",
            "HASH=pass#word\n",
            "SINGLE='literal ${APP_ENV} \\n'\n",
            "DOUBLE=\"tab\\there \\\"quoted\\\" \\${APP_ENV}\"\n",
            "MULTI=\"line one\n",
            "line two\"\n",
            "EMPTY=\n",
            "URL=${APP_ENV}.test/${MISSING}x\n",
            "not a variable\n",
            "CRLF=yes\r\n",
        ));
        assert_eq!(vars["APP_NAME"], "Break Room");
        assert_eq!(vars["APP_ENV"], "local");
        assert_eq!(vars["SPACED"], "value with spaces");
        assert_eq!(vars["HASH"], "pass#word");
        assert_eq!(vars["SINGLE"], "literal ${APP_ENV} \\n");
        assert_eq!(vars["DOUBLE"], "tab\there \"quoted\" ${APP_ENV}");
        assert_eq!(vars["MULTI"], "line one\nline two");
        assert_eq!(vars["EMPTY"], "");
        assert_eq!(vars["URL"], "local.test/x");
        assert_eq!(vars["CRLF"], "yes");
        assert_eq!(vars.len(), 10);
    }

    #[test]
    fn sail_mysql() {
        let vars = env(concat!(
            "APP_NAME=Laravel\nAPP_ENV=local\n",
            "DB_CONNECTION=mysql\nDB_HOST=mysql\nDB_PORT=3306\n",
            "DB_DATABASE=breakroom\nDB_USERNAME=sail\nDB_PASSWORD=password\nFORWARD_DB_PORT=33060\n",
        ));
        let cfg = connection_from_env(&vars, Path::new("/nonexistent/breakroom"), ".env").unwrap();
        assert_eq!(cfg.name, "Breakroom (local)");
        assert_eq!(cfg.driver, DriverKind::Mysql);
        assert_eq!(cfg.template, "mysql");
        assert_eq!((cfg.host.as_str(), cfg.port), ("127.0.0.1", 33060));
        assert_eq!((cfg.user.as_str(), cfg.database.as_str()), ("sail", "breakroom"));
        assert_eq!(cfg.password.as_deref(), Some("password"));
    }

    #[test]
    fn herd_postgres() {
        let vars = env("APP_NAME=Shop\nDB_CONNECTION=pgsql\nDB_HOST=127.0.0.1\nDB_DATABASE=shop\nDB_USERNAME=root\nDB_PASSWORD=\n");
        let cfg = connection_from_env(&vars, Path::new("/nonexistent/shop"), ".env").unwrap();
        assert_eq!(cfg.name, "Shop");
        assert_eq!(cfg.driver, DriverKind::Postgres);
        assert_eq!((cfg.host.as_str(), cfg.port), ("127.0.0.1", 5432));
        assert_eq!(cfg.password, None);
    }

    #[test]
    fn sqlite_paths() {
        let project = Path::new("/nonexistent/my-app");
        let cfg = connection_from_env(&env("DB_CONNECTION=sqlite\n"), project, ".env.testing").unwrap();
        assert_eq!(cfg.name, "My App (testing)");
        assert_eq!(cfg.file_path, "/nonexistent/my-app/database/database.sqlite");
        let cfg = connection_from_env(&env("DB_CONNECTION=sqlite\nDB_DATABASE=./storage/app.db\n"), project, ".env").unwrap();
        assert_eq!(cfg.file_path, "/nonexistent/my-app/storage/app.db");
        let cfg = connection_from_env(&env("DB_CONNECTION=sqlite\nDB_DATABASE=/var/db.sqlite\n"), project, ".env").unwrap();
        assert_eq!(cfg.file_path, "/var/db.sqlite");
        // No DB_CONNECTION at all: Laravel 11's SQLite default.
        let cfg = connection_from_env(&env("APP_NAME=Laravel\n"), project, ".env").unwrap();
        assert_eq!(cfg.driver, DriverKind::Sqlite);
    }

    #[test]
    fn db_url_wins() {
        let vars = env(concat!(
            "DB_CONNECTION=mysql\nDB_HOST=10.0.0.1\nDB_PASSWORD=ignored\n",
            "DB_URL=postgres://app%40corp:s%3Acret@db.example.com:6543/main?sslmode=require\n",
        ));
        let cfg = connection_from_env(&vars, Path::new("/nonexistent/app"), ".env").unwrap();
        assert_eq!(cfg.driver, DriverKind::Postgres);
        assert_eq!((cfg.host.as_str(), cfg.port), ("db.example.com", 6543));
        assert_eq!((cfg.user.as_str(), cfg.database.as_str()), ("app@corp", "main"));
        assert_eq!(cfg.password.as_deref(), Some("s:cret"));
        assert_eq!(cfg.ssl_mode, SslMode::Require);
    }

    #[test]
    fn unsupported_connection() {
        let err = connection_from_env(&env("DB_CONNECTION=sqlsrv\n"), Path::new("/x"), ".env").unwrap_err();
        assert!(err.to_string().contains("sqlsrv"));
    }
}
