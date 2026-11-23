//! Import from DBeaver (K41, D-114): the connections, folders and scripts of
//! a DBeaver project export (File ▸ Export ▸ Project, a `.dbp` zip) or of a
//! bare `data-sources.json`, mapped onto Kerno's connections.
//!
//! Ported from DBeaver Community (Apache-2.0, see THIRD_PARTY_NOTICES.md):
//! the file layout from `tools/project/ProjectExportWizard.java`, the
//! connection format from `registry/DataSourceSerializerModern.java` and
//! `registry/DataSourceParser.java`, the credentials file's cipher and key
//! from `model/impl/app/DefaultValueEncryptor.java` and `BaseProjectImpl.java`,
//! the script bindings from `BaseProjectImpl.loadMetadata`. Lenient: unknown
//! fields are ignored, missing ones defaulted.
//!
//! The archive is untrusted: its entries are read in memory, never written
//! to disk, each one and all of them together under a cap, and an entry
//! whose path goes up or is absolute is ignored.

use std::collections::{BTreeMap, BTreeSet};
use std::io::{Cursor, Read};
use std::path::PathBuf;

use serde_json::{Map, Value};

use crate::config::{
    ConnectionConfig, Engine, Secret, Secrets, SshAuth, SshConfig, TlsMode, folder_path,
};
use crate::environment::EnvironmentDef;

/// The largest file read: a `.dbp` or a `data-sources.json`.
pub const MAX_FILE: u64 = 64 * 1024 * 1024;
/// The largest entry of an archive read.
pub const MAX_ENTRY: u64 = 16 * 1024 * 1024;
/// All the entries read from one archive, together.
pub const MAX_TOTAL: u64 = 64 * 1024 * 1024;
/// The most entries an archive may have.
pub const MAX_ENTRIES: usize = 20_000;
/// The largest script imported as a saved query; a larger one is skipped.
pub const MAX_SCRIPT: u64 = 1024 * 1024;

/// What DBeaver's file holds, mapped.
#[derive(Debug, Default)]
pub struct Export {
    /// Every folder, its parents included, `/`-separated, sorted.
    pub folders: Vec<String>,
    pub connections: Vec<Connection>,
    pub scripts: Vec<Script>,
    /// Scripts left out, with why: `Scripts/big.sql (over 1 MB)`.
    pub skipped_scripts: Vec<String>,
    pub credentials: Credentials,
}

/// The saved user names and passwords (`credentials-config.json`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Credentials {
    /// The file has none.
    #[default]
    Absent,
    /// Read.
    Read,
    /// Protected by a project password, or damaged: not read.
    Locked,
}

/// One DBeaver connection.
#[derive(Debug)]
pub struct Connection {
    /// DBeaver's id, which scripts are bound to.
    pub id: String,
    pub name: String,
    pub folder: Option<String>,
    /// The engine as shown: "PostgreSQL", or DBeaver's name for one Kerno
    /// does not have.
    pub engine: String,
    pub host: String,
    pub port: String,
    pub database: String,
    pub user: String,
    /// Kerno's connection, or why there is none.
    pub config: Result<ConnectionConfig, String>,
    /// What DBeaver set that Kerno left at its default.
    pub not_mapped: Vec<String>,
    /// The saved password and SSH secret; empty unless asked for.
    pub secrets: Secrets,
}

/// A `.sql` file of the project.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Script {
    /// DBeaver's id of the connection it is bound to.
    pub connection: Option<String>,
    /// Its folder under `Scripts/`, `/`-separated; empty at the top.
    pub folder: String,
    /// The file name without `.sql`.
    pub name: String,
    pub sql: String,
}

/// Reads a `.dbp` or a `data-sources.json`. The passwords are kept only
/// when `with_passwords`; otherwise they are dropped as they are read.
/// `palette` is the connection colours (key, light, dark).
pub fn read(
    bytes: &[u8],
    with_passwords: bool,
    environments: &[EnvironmentDef],
    palette: &[(&str, u32, u32)],
) -> Result<Export, String> {
    let projects = if bytes.starts_with(b"PK") {
        unzip(bytes)?
    } else if bytes.trim_ascii_start().starts_with(b"<") {
        return Err(
            "This is the connections file of a DBeaver older than 6.1.3, which Kerno does not \
             read. Open it in a newer DBeaver, then export the project (File ▸ Export ▸ Project)."
                .into(),
        );
    } else if parse_json(bytes).is_none() {
        return Err(not_an_export());
    } else {
        vec![Project {
            data_sources: vec![(String::new(), bytes.to_vec())],
            ..Project::default()
        }]
    };
    let mut export = Export::default();
    let mut folders = BTreeSet::new();
    for project in projects {
        if project.data_sources.is_empty() {
            continue;
        }
        let bound = bindings(project.metadata.as_deref());
        for (sub_id, json) in &project.data_sources {
            let config = parse_json(json).ok_or_else(|| {
                "Its connections cannot be read: the project is encrypted with a password, or \
                 the file is damaged. In DBeaver, remove the project's password and export it \
                 again."
                    .to_string()
            })?;
            let credentials = match project.credentials.get(sub_id) {
                None => None,
                Some(encrypted) => match decrypt(encrypted).and_then(|plain| {
                    let parsed = serde_json::from_slice::<Value>(&plain).ok();
                    let mut plain = plain;
                    plain.fill(0);
                    parsed
                }) {
                    Some(Value::Object(map)) => {
                        if export.credentials != Credentials::Locked {
                            export.credentials = Credentials::Read;
                        }
                        Some(map)
                    }
                    _ => {
                        export.credentials = Credentials::Locked;
                        None
                    }
                },
            };
            let types = object(&config, "connection-types");
            for (path, folder) in object(&config, "folders") {
                // An old file names the parent apart.
                let path = match folder.get("parent").and_then(Value::as_str) {
                    Some(parent) => format!("{parent}/{path}"),
                    None => path.clone(),
                };
                folders.extend(folder_path(&path));
            }
            for (id, connection) in object(&config, "connections") {
                let mut connection = map_connection(
                    id,
                    connection,
                    credentials.as_ref(),
                    &types,
                    environments,
                    palette,
                );
                if !with_passwords {
                    connection.secrets = Secrets::default();
                }
                folders.extend(connection.folder.clone());
                export.connections.push(connection);
            }
        }
        for (path, bytes) in project.scripts {
            let connection = bound.get(&path).cloned();
            let relative = path.strip_prefix("Scripts/").unwrap_or(&path);
            let (folder, file) = relative.rsplit_once('/').unwrap_or(("", relative));
            let name = file
                .strip_suffix(".sql")
                .or_else(|| file.strip_suffix(".SQL"))
                .unwrap_or(file);
            if bytes.len() as u64 > MAX_SCRIPT {
                export.skipped_scripts.push(format!("{path} (over 1 MB)"));
                continue;
            }
            export.scripts.push(Script {
                connection,
                folder: folder_path(folder).unwrap_or_default(),
                name: name.to_string(),
                sql: String::from_utf8_lossy(&bytes).into_owned(),
            });
        }
        export.skipped_scripts.extend(project.too_big);
    }
    if export.connections.is_empty() && export.scripts.is_empty() && folders.is_empty() {
        return Err(
            "This is not a DBeaver export: it has no connections. Choose the .dbp file DBeaver \
             wrote (File ▸ Export ▸ Project), or a data-sources.json."
                .into(),
        );
    }
    // Every parent of a folder is a folder.
    let mut all = BTreeSet::new();
    for folder in folders {
        let mut current = Some(folder);
        while let Some(path) = current {
            current = crate::config::parent_folder(&path).map(str::to_string);
            all.insert(path);
        }
    }
    export.folders = all.into_iter().collect();
    Ok(export)
}

/// One project of an archive: its configuration files and scripts, by
/// path relative to the project.
#[derive(Default)]
struct Project {
    /// `data-sources*.json`, by storage sub-id (`""`, `"-2"`).
    data_sources: Vec<(String, Vec<u8>)>,
    /// `credentials-config*.json`, by the same sub-id.
    credentials: BTreeMap<String, Vec<u8>>,
    metadata: Option<Vec<u8>>,
    scripts: Vec<(String, Vec<u8>)>,
    /// Scripts over the cap, not read.
    too_big: Vec<String>,
}

fn not_an_export() -> String {
    "This file is not a DBeaver export, or it is damaged. Export the project again from \
     DBeaver (File ▸ Export ▸ Project) and choose the .dbp file it writes."
        .into()
}

/// Reads the projects of a `.dbp`.
fn unzip(bytes: &[u8]) -> Result<Vec<Project>, String> {
    let mut archive = zip::ZipArchive::new(Cursor::new(bytes)).map_err(|_| not_an_export())?;
    if archive.len() > MAX_ENTRIES {
        return Err(format!(
            "This archive has more than {MAX_ENTRIES} files: it is not a DBeaver export Kerno \
             reads."
        ));
    }
    let mut projects: BTreeMap<String, Project> = BTreeMap::new();
    let mut total = 0u64;
    for index in 0..archive.len() {
        let mut entry = archive.by_index(index).map_err(|_| not_an_export())?;
        if entry.is_dir() {
            continue;
        }
        // `..` or an absolute path: ignored.
        let Some(path) = entry.enclosed_name() else {
            continue;
        };
        let path: Vec<String> = path
            .components()
            .map(|part| part.as_os_str().to_string_lossy().into_owned())
            .collect();
        let [top, project, rest @ ..] = path.as_slice() else {
            continue;
        };
        if top != "projects" || rest.is_empty() {
            continue;
        }
        let relative = rest.join("/");
        let file = rest.last().map(String::as_str).unwrap_or_default();
        enum Kind {
            DataSources(String),
            Credentials(String),
            Metadata,
            Script,
        }
        let kind = if rest.len() == 2 && rest[0] == ".dbeaver" {
            if let Some(sub) = json_sub_id(file, "data-sources") {
                Kind::DataSources(sub)
            } else if let Some(sub) = json_sub_id(file, "credentials-config") {
                Kind::Credentials(sub)
            } else if file == "project-metadata.json" {
                Kind::Metadata
            } else {
                continue;
            }
        } else if rest[0] != ".dbeaver" && file.to_ascii_lowercase().ends_with(".sql") {
            Kind::Script
        } else {
            continue;
        };
        let project = projects.entry(project.clone()).or_default();
        if matches!(kind, Kind::Script) && entry.size() > MAX_SCRIPT {
            project.too_big.push(format!("{relative} (over 1 MB)"));
            continue;
        }
        // The declared size can lie: the read itself is capped.
        let mut data = Vec::new();
        (&mut entry)
            .take(MAX_ENTRY + 1)
            .read_to_end(&mut data)
            .map_err(|_| not_an_export())?;
        if data.len() as u64 > MAX_ENTRY {
            return Err(format!(
                "“{relative}” is larger than {} MB: this is not a DBeaver export Kerno reads.",
                MAX_ENTRY / 1024 / 1024
            ));
        }
        total += data.len() as u64;
        if total > MAX_TOTAL {
            return Err(format!(
                "This archive holds more than {} MB of connections and scripts: Kerno does not \
                 read it.",
                MAX_TOTAL / 1024 / 1024
            ));
        }
        match kind {
            Kind::DataSources(sub) => project.data_sources.push((sub, data)),
            Kind::Credentials(sub) => {
                project.credentials.insert(sub, data);
            }
            Kind::Metadata => project.metadata = Some(data),
            Kind::Script => project.scripts.push((relative, data)),
        }
    }
    let projects: Vec<Project> = projects.into_values().collect();
    if projects
        .iter()
        .all(|project| project.data_sources.is_empty())
    {
        return Err(not_an_export());
    }
    Ok(projects)
}

/// `data-sources.json` → `""`, `data-sources-2.json` → `"-2"`.
fn json_sub_id(file: &str, prefix: &str) -> Option<String> {
    file.strip_prefix(prefix)?
        .strip_suffix(".json")
        .map(str::to_string)
}

/// The script → connection bindings of `project-metadata.json`.
fn bindings(metadata: Option<&[u8]>) -> BTreeMap<String, String> {
    let Some(root) = metadata.and_then(parse_json) else {
        return BTreeMap::new();
    };
    object(&root, "resources")
        .iter()
        .filter_map(|(path, props)| {
            let id = props.get("default-datasource")?.as_str()?;
            Some((path.trim_start_matches('/').to_string(), id.to_string()))
        })
        .collect()
}

fn parse_json(bytes: &[u8]) -> Option<Map<String, Value>> {
    match serde_json::from_slice(bytes).ok()? {
        Value::Object(map) => Some(map),
        _ => None,
    }
}

/// The objects under `key`, by name; none when it is missing or not an
/// object.
fn object<'a>(map: &'a Map<String, Value>, key: &str) -> Vec<(&'a String, &'a Map<String, Value>)> {
    match map.get(key) {
        Some(Value::Object(inner)) => inner
            .iter()
            .filter_map(|(name, value)| value.as_object().map(|value| (name, value)))
            .collect(),
        _ => Vec::new(),
    }
}

/// A string, number or boolean as text; `None` when missing or empty.
fn text(map: &Map<String, Value>, key: &str) -> Option<String> {
    let text = match map.get(key)? {
        Value::String(text) => text.clone(),
        Value::Number(number) => number.to_string(),
        Value::Bool(flag) => flag.to_string(),
        _ => return None,
    };
    (!text.trim().is_empty()).then(|| text.trim().to_string())
}

fn flag(map: &Map<String, Value>, key: &str) -> Option<bool> {
    match map.get(key)? {
        Value::Bool(flag) => Some(*flag),
        Value::String(text) => text.parse().ok(),
        _ => None,
    }
}

/// The engine of a DBeaver provider and driver, or what to say instead.
fn engine(provider: &str, driver: &str) -> Result<Engine, String> {
    match (provider, driver) {
        ("postgresql", "postgres-jdbc" | "postgresql") => Ok(Engine::Postgres),
        ("mysql", "mysql8" | "mysql5" | "mysql3") => Ok(Engine::MySql),
        ("mysql", "mariaDB") => Ok(Engine::MariaDb),
        ("sqlite" | "generic", "sqlite_jdbc") => Ok(Engine::Sqlite),
        ("postgresql" | "mysql", driver) => Err(format!(
            "The driver “{driver}” is not one Kerno knows: Kerno speaks to PostgreSQL, MySQL, \
             MariaDB and SQLite."
        )),
        (provider, _) => Err(format!(
            "{}: not an engine Kerno has.",
            provider_label(provider)
        )),
    }
}

/// The engine as shown: Kerno's name, else DBeaver's.
fn engine_label(provider: &str, driver: &str) -> String {
    match engine(provider, driver) {
        Ok(engine) => engine.label().to_string(),
        Err(_) if matches!(provider, "postgresql" | "mysql") => driver.to_string(),
        Err(_) => provider_label(provider),
    }
}

/// A DBeaver provider Kerno does not have, as shown.
fn provider_label(provider: &str) -> String {
    match provider {
        "oracle" => "Oracle".into(),
        "sqlserver" | "mssql" => "SQL Server".into(),
        "db2" | "db2_zos" | "db2_i" => "Db2".into(),
        "clickhouse" => "ClickHouse".into(),
        "mongodb" => "MongoDB".into(),
        "snowflake" => "Snowflake".into(),
        "" => "An unknown engine".into(),
        other => other.to_string(),
    }
}

fn map_connection(
    id: &str,
    json: &Map<String, Value>,
    credentials: Option<&Map<String, Value>>,
    types: &[(&String, &Map<String, Value>)],
    environments: &[EnvironmentDef],
    palette: &[(&str, u32, u32)],
) -> Connection {
    let provider = text(json, "original-provider")
        .or_else(|| text(json, "provider"))
        .unwrap_or_default();
    let driver = text(json, "original-driver")
        .or_else(|| text(json, "driver"))
        .unwrap_or_default();
    let empty = Map::new();
    let cfg = json
        .get("configuration")
        .and_then(Value::as_object)
        .unwrap_or(&empty);
    // The saved credentials of the connection and of its tunnel.
    let saved = |node: &str| -> Map<String, Value> {
        credentials
            .and_then(|all| all.get(id))
            .and_then(|by_node| by_node.get(node))
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default()
    };
    let creds = saved("#connection");
    let host = text(cfg, "host").unwrap_or_default();
    let port = text(cfg, "port").unwrap_or_default();
    let database = text(cfg, "database").unwrap_or_default();
    let user = text(&creds, "user")
        .or_else(|| text(cfg, "user"))
        .unwrap_or_default();
    let password = text(&creds, "password").or_else(|| text(cfg, "password"));
    let folder = text(json, "folder").and_then(|path| folder_path(&path));
    let mut connection = Connection {
        id: id.to_string(),
        name: text(json, "name").unwrap_or_else(|| "?".into()),
        folder,
        engine: engine_label(&provider, &driver),
        host,
        port,
        database,
        user,
        config: Err(String::new()),
        not_mapped: Vec::new(),
        secrets: Secrets::default(),
    };
    let engine = match engine(&provider, &driver) {
        Ok(engine) => engine,
        Err(reason) => {
            connection.config = Err(reason);
            return connection;
        }
    };
    let mut config = ConnectionConfig::new(engine);
    config.name = connection.name.clone();
    config.group = connection.folder.clone();
    config.read_only = flag(json, "read-only").unwrap_or(false);
    config.save_password = flag(json, "save-password").unwrap_or(true);
    let not_mapped = &mut connection.not_mapped;
    if engine.is_file() {
        let file = (!connection.database.is_empty())
            .then(|| connection.database.clone())
            .or_else(|| {
                text(cfg, "url")
                    .and_then(|url| url.strip_prefix("jdbc:sqlite:").map(str::to_string))
            });
        config.file = file.map(PathBuf::from);
    } else {
        if !connection.host.is_empty() {
            config.host = connection.host.clone();
        }
        match connection.port.parse::<u16>() {
            Ok(port) => config.port = port,
            Err(_) if connection.port.is_empty() => {}
            Err(_) => not_mapped.push(format!("port “{}”", connection.port)),
        }
        // Not in the file (a project password): the engine's default user.
        if connection.user.is_empty() {
            not_mapped.push(format!(
                "user name (not in the file; set to {})",
                config.user
            ));
        } else {
            config.user = connection.user.clone();
        }
        config.database = (!connection.database.is_empty()).then(|| connection.database.clone());
        connection.secrets.password = password.map(Secret::new);
    }
    // The connection type: DBeaver's three built-ins, else by its name.
    let kind = text(cfg, "type").unwrap_or_else(|| "dev".into());
    config.environment = match kind.as_str() {
        "dev" => "local".into(),
        "test" => "staging".into(),
        "prod" => "production".into(),
        other => {
            let name = types
                .iter()
                .find(|(type_id, _)| type_id.as_str() == other)
                .and_then(|(_, def)| text(def, "name"))
                .unwrap_or_else(|| other.to_string());
            match environments
                .iter()
                .find(|def| def.name.eq_ignore_ascii_case(&name) || def.id == other)
            {
                Some(def) => def.id.clone(),
                None => {
                    not_mapped.push(format!("connection type “{name}” (put in local)"));
                    "local".into()
                }
            }
        }
    };
    if let Some(color) = text(cfg, "color") {
        match palette_color(&color, palette) {
            Some(key) => config.color = Some(key.to_string()),
            None => not_mapped.push(format!("colour {color}")),
        }
    }
    let handlers = object(cfg, "handlers");
    for (handler, settings) in &handlers {
        if !flag(settings, "enabled").unwrap_or(false) {
            continue;
        }
        let props = settings
            .get("properties")
            .and_then(Value::as_object)
            .unwrap_or(&empty);
        match handler.as_str() {
            "ssh_tunnel" if !engine.is_file() => {
                let creds = saved("network/ssh_tunnel");
                let secret = text(&creds, "password").or_else(|| text(settings, "password"));
                let auth = match text(props, "authType").as_deref() {
                    Some("PUBLIC_KEY") => match text(props, "keyPath") {
                        Some(path) => {
                            connection.secrets.ssh_key_passphrase = secret.map(Secret::new);
                            SshAuth::KeyFile {
                                path: PathBuf::from(path),
                            }
                        }
                        None => {
                            not_mapped.push("SSH key kept in DBeaver (no key file)".into());
                            SshAuth::default()
                        }
                    },
                    Some("AGENT") => SshAuth::Agent {
                        socket: text(props, "agentAuthSockPath").map(PathBuf::from),
                    },
                    _ => {
                        connection.secrets.ssh_password = secret.map(Secret::new);
                        SshAuth::Password
                    }
                };
                if text(props, "jumpServerSettings").is_some()
                    || props.keys().any(|key| key.starts_with("jumpServer"))
                {
                    not_mapped.push("SSH jump server".into());
                }
                config.ssh = Some(SshConfig {
                    host: text(props, "host").unwrap_or_default(),
                    port: text(props, "port")
                        .and_then(|port| port.parse().ok())
                        .unwrap_or(22),
                    // Without one, DBeaver logs in as the local user.
                    user: text(&creds, "user")
                        .or_else(|| text(settings, "user"))
                        .unwrap_or_else(|| {
                            not_mapped.push("SSH user (not in the file; set to yours)".into());
                            std::env::var("USER").unwrap_or_default()
                        }),
                    auth,
                    keepalive_secs: 30,
                    client: Default::default(),
                    jump: None,
                });
            }
            "postgre_ssl" | "mysql_ssl" if !engine.is_file() => {
                let path = |keys: &[&str]| {
                    keys.iter()
                        .find_map(|key| text(props, key))
                        .map(PathBuf::from)
                };
                config.tls.ca_file = path(&["ssl.ca.cert", "rootCert"]);
                config.tls.cert_file = path(&["ssl.client.cert", "clientCert"]);
                config.tls.key_file = path(&["ssl.client.key", "clientKey"]);
                if props.keys().any(|key| key.ends_with(".value")) {
                    not_mapped.push("certificates pasted in DBeaver (not files)".into());
                }
                config.tls.mode = if handler.as_str() == "postgre_ssl" {
                    match text(props, "sslMode").as_deref().map(TlsMode::parse) {
                        Some(Some(mode)) => mode,
                        Some(None) => {
                            not_mapped.push("SSL mode".into());
                            TlsMode::Require
                        }
                        None => TlsMode::Require,
                    }
                } else {
                    match (
                        flag(props, "ssl.require").unwrap_or(false),
                        flag(props, "ssl.verify.server").unwrap_or(true),
                    ) {
                        (_, true) => TlsMode::VerifyCa,
                        (true, false) => TlsMode::Require,
                        (false, false) => TlsMode::Prefer,
                    }
                };
            }
            other => not_mapped.push(format!("network handler “{other}”")),
        }
    }
    connection.config = match config.validate() {
        Ok(()) => Ok(config),
        Err(crate::Error::Config(reason)) => Err(format!("Its settings are incomplete: {reason}.")),
        Err(error) => Err(format!("Its settings are incomplete: {error}.")),
    };
    connection
}

/// The palette colour within a short distance of DBeaver's `r,g,b`.
fn palette_color<'a>(color: &str, palette: &[(&'a str, u32, u32)]) -> Option<&'a str> {
    let parts: Vec<i64> = color
        .split(',')
        .map(|part| part.trim().parse::<i64>())
        .collect::<Result<_, _>>()
        .ok()?;
    let [r, g, b] = parts.as_slice() else {
        return None;
    };
    let distance = |rgb: u32| {
        let (pr, pg, pb) = (
            i64::from(rgb >> 16 & 0xFF),
            i64::from(rgb >> 8 & 0xFF),
            i64::from(rgb & 0xFF),
        );
        (r - pr).pow(2) + (g - pg).pow(2) + (b - pb).pow(2)
    };
    // ponytail: a fixed Euclidean threshold (48 per channel at most), not a
    // perceptual one; a colour further from every tag is reported instead.
    palette
        .iter()
        .map(|(key, light, dark)| (*key, distance(*light).min(distance(*dark))))
        .filter(|(_, d)| *d <= 48 * 48)
        .min_by_key(|(_, d)| *d)
        .map(|(key, _)| key)
}

/// DBeaver's local key (`BaseProjectImpl.LOCAL_KEY_CACHE`, Java's signed
/// bytes as unsigned).
const LOCAL_KEY: [u8; 16] = [
    0xBA, 0xBB, 0x4A, 0x9F, 0x77, 0x4A, 0xB8, 0x53, 0xC9, 0x6C, 0x2D, 0x65, 0x3D, 0xFE, 0x54, 0x4A,
];

/// `DefaultValueEncryptor.decryptValue`: AES-128-CBC, PKCS#5 padding, the
/// IV in the first 16 bytes. `None` when it does not decrypt (another key).
fn decrypt(data: &[u8]) -> Option<Vec<u8>> {
    use cbc::cipher::{BlockModeDecrypt as _, KeyIvInit as _, block_padding::Pkcs7};
    if data.len() < 32 {
        return None;
    }
    let (iv, body) = data.split_at(16);
    let mut buffer = body.to_vec();
    let decryptor = cbc::Decryptor::<aes::Aes128>::new_from_slices(&LOCAL_KEY, iv).ok()?;
    let length = decryptor.decrypt_padded::<Pkcs7>(&mut buffer).ok()?.len();
    buffer.truncate(length);
    Some(buffer)
}

/// `DefaultValueEncryptor.encryptValue`, for test fixtures.
#[cfg(any(test, feature = "test-support"))]
pub fn encrypt(plain: &[u8], iv: [u8; 16]) -> Vec<u8> {
    use cbc::cipher::{BlockModeEncrypt as _, KeyIvInit as _, block_padding::Pkcs7};
    let mut buffer = plain.to_vec();
    buffer.resize(plain.len() + 16, 0);
    let encryptor = cbc::Encryptor::<aes::Aes128>::new_from_slices(&LOCAL_KEY, &iv).unwrap();
    let length = encryptor
        .encrypt_padded::<Pkcs7>(&mut buffer, plain.len())
        .unwrap()
        .len();
    buffer.truncate(length);
    [iv.to_vec(), buffer].concat()
}

/// A `.dbp` as DBeaver writes it, for tests: `meta.xml` and, under
/// `projects/<project>/`, the files given by path.
#[cfg(any(test, feature = "test-support"))]
pub fn archive(project: &str, files: &[(&str, &[u8])]) -> Vec<u8> {
    use std::io::Write as _;
    let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
    let options = zip::write::SimpleFileOptions::default();
    zip.start_file("meta.xml", options).unwrap();
    zip.write_all(b"<archive version=\"1\"/>").unwrap();
    for (path, data) in files {
        zip.start_file(format!("projects/{project}/{path}"), options)
            .unwrap();
        zip.write_all(data).unwrap();
    }
    zip.finish().unwrap().into_inner()
}

/// The `data-sources.json`, `credentials-config.json` and scripts of a
/// project with two folders (one nested), PostgreSQL, MySQL, MariaDB,
/// SQLite and an Oracle connection, an SSH tunnel and connection types;
/// for tests. The credentials hold `pg-secret` and `ssh-secret`.
#[cfg(any(test, feature = "test-support"))]
pub fn sample_project() -> Vec<u8> {
    let sources = serde_json::json!({
        "folders": {
            "Work": {},
            "Work/Clients": {"description": "nested"},
            "Empty": {}
        },
        "connection-types": {
            "qa": {"name": "Staging", "color": "0,0,255"}
        },
        "connections": {
            "postgres-jdbc-1": {
                "provider": "postgresql", "driver": "postgres-jdbc",
                "name": "Shop", "folder": "Work/Clients", "save-password": true,
                "configuration": {
                    "host": "db.example.com", "port": "5433", "database": "shop",
                    "type": "prod", "color": "198,47,61",
                    "handlers": {
                        "ssh_tunnel": {
                            "enabled": true, "save-password": true,
                            "properties": {"host": "bastion.example.com", "port": "2222",
                                "authType": "PUBLIC_KEY", "keyPath": "/home/me/.ssh/id_ed25519"}
                        },
                        "postgre_ssl": {
                            "enabled": true,
                            "properties": {"sslMode": "verify-full", "ssl.ca.cert": "/certs/ca.pem"}
                        }
                    }
                }
            },
            "mysql8-1": {
                "provider": "mysql", "driver": "mysql8", "name": "Blog", "folder": "Work",
                "read-only": true,
                "configuration": {"host": "mysql.example.com", "port": 3307, "database": "blog",
                    "user": "blogger", "type": "qa"}
            },
            "mariaDB-1": {
                "provider": "mysql", "driver": "mariaDB", "name": "Legacy",
                "configuration": {"host": "maria.example.com", "port": "3306", "type": "test",
                    "color": "12,34,56"}
            },
            "sqlite-1": {
                "provider": "sqlite", "driver": "sqlite_jdbc", "name": "Notes",
                "configuration": {"database": "/data/notes.db", "url": "jdbc:sqlite:/data/notes.db"}
            },
            "oracle-1": {
                "provider": "oracle", "driver": "oracle_thin", "name": "ERP",
                "configuration": {"host": "erp.example.com", "port": "1521", "database": "ORCL"}
            }
        }
    });
    let credentials = serde_json::json!({
        "postgres-jdbc-1": {
            "#connection": {"user": "shop_owner", "password": "pg-secret"},
            "network/ssh_tunnel": {"user": "ops", "password": "ssh-secret"}
        },
        "mariaDB-1": {"#connection": {"user": "maria"}}
    });
    let metadata = serde_json::json!({
        "resources": {
            "Scripts/reports/monthly.sql": {"default-datasource": "postgres-jdbc-1"},
            "Scripts/oracle.sql": {"default-datasource": "oracle-1"}
        }
    });
    let credentials = encrypt(credentials.to_string().as_bytes(), [7; 16]);
    archive(
        "General",
        &[
            (".dbeaver/data-sources.json", sources.to_string().as_bytes()),
            (".dbeaver/credentials-config.json", &credentials),
            (
                ".dbeaver/project-metadata.json",
                metadata.to_string().as_bytes(),
            ),
            ("Scripts/reports/monthly.sql", b"select 1;"),
            ("Scripts/oracle.sql", b"select 2 from dual;"),
            ("Scripts/scratch.sql", b"select 3;"),
        ],
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::environment::builtin_environments;

    const PALETTE: [(&str, u32, u32); 2] =
        [("red", 0xC62F3D, 0xF26D78), ("blue", 0x1D6FB8, 0x5AA9F0)];

    fn envs() -> Vec<EnvironmentDef> {
        builtin_environments()
    }

    fn find<'a>(export: &'a Export, name: &str) -> &'a Connection {
        export
            .connections
            .iter()
            .find(|connection| connection.name == name)
            .unwrap()
    }

    #[test]
    fn a_project_export_maps_folders_engines_tunnels_types_and_scripts() {
        let export = read(&sample_project(), false, &envs(), &PALETTE).unwrap();
        assert_eq!(export.folders, ["Empty", "Work", "Work/Clients"]);
        assert_eq!(export.credentials, Credentials::Read);

        let shop = find(&export, "Shop");
        let config = shop.config.as_ref().unwrap();
        assert_eq!(config.engine, Engine::Postgres);
        assert_eq!(
            (config.host.as_str(), config.port, config.user.as_str()),
            ("db.example.com", 5433, "shop_owner")
        );
        assert_eq!(config.database.as_deref(), Some("shop"));
        assert_eq!(config.group.as_deref(), Some("Work/Clients"));
        assert_eq!(config.environment, "production");
        assert_eq!(config.color.as_deref(), Some("red"));
        assert_eq!(config.tls.mode, TlsMode::VerifyFull);
        assert_eq!(config.tls.ca_file, Some(PathBuf::from("/certs/ca.pem")));
        let ssh = config.ssh.as_ref().unwrap();
        assert_eq!(
            (ssh.host.as_str(), ssh.port, ssh.user.as_str()),
            ("bastion.example.com", 2222, "ops")
        );
        assert_eq!(
            ssh.auth,
            SshAuth::KeyFile {
                path: "/home/me/.ssh/id_ed25519".into()
            }
        );
        // Not asked for: no password kept.
        assert_eq!(shop.secrets, Secrets::default());

        let blog = find(&export, "Blog").config.as_ref().unwrap();
        assert_eq!(
            (blog.engine, blog.port, blog.user.as_str(), blog.read_only),
            (Engine::MySql, 3307, "blogger", true)
        );
        // A type of the file, found by its name.
        assert_eq!(blog.environment, "staging");

        let legacy = find(&export, "Legacy");
        let config = legacy.config.as_ref().unwrap();
        assert_eq!(
            (
                config.engine,
                config.user.as_str(),
                config.environment.as_str()
            ),
            (Engine::MariaDb, "maria", "staging")
        );
        assert_eq!(config.color, None);
        assert_eq!(legacy.not_mapped, ["colour 12,34,56"]);

        let notes = find(&export, "Notes").config.as_ref().unwrap();
        assert_eq!(notes.engine, Engine::Sqlite);
        assert_eq!(notes.file, Some(PathBuf::from("/data/notes.db")));

        let erp = find(&export, "ERP");
        assert_eq!(erp.engine, "Oracle");
        assert_eq!(
            erp.config.as_ref().unwrap_err(),
            "Oracle: not an engine Kerno has."
        );

        let mut scripts: Vec<_> = export
            .scripts
            .iter()
            .map(|s| (s.folder.as_str(), s.name.as_str(), s.connection.as_deref()))
            .collect();
        scripts.sort();
        assert_eq!(
            scripts,
            [
                ("", "oracle", Some("oracle-1")),
                ("", "scratch", None),
                ("reports", "monthly", Some("postgres-jdbc-1")),
            ]
        );
    }

    #[test]
    fn the_saved_passwords_are_kept_only_when_asked() {
        let export = read(&sample_project(), true, &envs(), &PALETTE).unwrap();
        let shop = find(&export, "Shop");
        assert_eq!(
            shop.secrets.password.as_ref().map(Secret::expose),
            Some("pg-secret")
        );
        assert_eq!(
            shop.secrets.ssh_key_passphrase.as_ref().map(Secret::expose),
            Some("ssh-secret")
        );
        assert_eq!(shop.secrets.ssh_password, None);
        // Debug never shows them.
        assert!(!format!("{export:?}").contains("pg-secret"));
    }

    #[test]
    fn a_bare_data_sources_file_is_read() {
        let json = br#"{"folders": {}, "connections": {"c1": {"provider": "postgresql",
            "driver": "postgres-jdbc", "name": "Bare", "configuration": {"host": "h", "port": "5432",
            "user": "u", "unknown": [1, 2]}, "something-new": {}}}}"#;
        let export = read(json, true, &envs(), &PALETTE).unwrap();
        assert_eq!(export.credentials, Credentials::Absent);
        let bare = find(&export, "Bare").config.as_ref().unwrap();
        assert_eq!((bare.host.as_str(), bare.user.as_str()), ("h", "u"));
        assert_eq!(bare.environment, "local");
    }

    #[test]
    fn a_project_with_a_master_password_imports_without_credentials() {
        let sources = br#"{"connections": {"c1": {"provider": "mysql", "driver": "mysql8",
            "name": "Locked", "configuration": {"host": "h"}}}}"#;
        // Encrypted with another key: DBeaver's project password.
        let garbage = [9u8; 48];
        let dbp = archive(
            "P",
            &[
                (".dbeaver/data-sources.json", sources),
                (".dbeaver/credentials-config.json", &garbage),
            ],
        );
        let export = read(&dbp, true, &envs(), &PALETTE).unwrap();
        assert_eq!(export.credentials, Credentials::Locked);
        let locked = find(&export, "Locked");
        assert_eq!(locked.secrets, Secrets::default());
        // Still imported, with the engine's default user, said.
        assert_eq!(locked.config.as_ref().unwrap().user, "root");
        assert_eq!(
            locked.not_mapped,
            ["user name (not in the file; set to root)"]
        );
        // An encrypted configuration cannot be read at all.
        let dbp = archive("P", &[(".dbeaver/data-sources.json", &garbage)]);
        assert!(
            read(&dbp, false, &envs(), &PALETTE)
                .unwrap_err()
                .contains("encrypted with a password")
        );
    }

    #[test]
    fn damaged_hostile_and_oversized_archives_are_refused_or_trimmed() {
        // Damaged.
        let mut damaged = sample_project();
        damaged.truncate(damaged.len() / 2);
        assert!(
            read(&damaged, false, &envs(), &PALETTE)
                .unwrap_err()
                .contains("not a DBeaver export")
        );
        // Not JSON, not a zip; the old XML format.
        assert!(
            read(b"hello", false, &envs(), &PALETTE)
                .unwrap_err()
                .contains("not a DBeaver export")
        );
        assert!(
            read(
                b"\n<?xml version=\"1.0\"?><data-sources/>",
                false,
                &envs(),
                &PALETTE
            )
            .unwrap_err()
            .contains("older than 6.1.3")
        );
        // A path that goes up is ignored: its connections are not read.
        let sources = br#"{"connections": {"c": {"provider": "mysql", "driver": "mysql8",
            "name": "Up", "configuration": {"host": "h"}}}}"#;
        let up = archive(
            "P",
            &[
                (".dbeaver/data-sources.json", sources),
                ("../../../outside.sql", b"select 1;"),
            ],
        );
        let export = read(&up, false, &envs(), &PALETTE).unwrap();
        assert!(export.scripts.is_empty() && export.skipped_scripts.is_empty());
        // An entry over the cap.
        let big = vec![b' '; MAX_ENTRY as usize + 1];
        let over = archive("P", &[(".dbeaver/data-sources.json", &big)]);
        assert!(
            read(&over, false, &envs(), &PALETTE)
                .unwrap_err()
                .contains("larger than 16 MB")
        );
        // A script over its cap is listed, not imported.
        let script = vec![b'-'; MAX_SCRIPT as usize + 1];
        let dbp = archive(
            "P",
            &[
                (".dbeaver/data-sources.json", sources),
                ("Scripts/huge.sql", &script),
            ],
        );
        let export = read(&dbp, false, &envs(), &PALETTE).unwrap();
        assert!(export.scripts.is_empty());
        assert_eq!(export.skipped_scripts, ["Scripts/huge.sql (over 1 MB)"]);
    }

    #[test]
    fn the_credentials_cipher_is_dbeavers() {
        // AES-128-CBC with the IV first, PKCS#5 padding: 16 bytes of text
        // take a whole block of padding.
        let sealed = encrypt(b"0123456789abcdef", [1; 16]);
        assert_eq!(sealed.len(), 16 + 32);
        assert_eq!(&sealed[..16], &[1; 16]);
        assert_eq!(decrypt(&sealed).unwrap(), b"0123456789abcdef");
        assert_eq!(decrypt(&[0; 32]), None);
        // Made apart from this code, with DBeaver's key and an IV of 0..15:
        // `openssl enc -aes-128-cbc -K babb4a9f774ab853c96c2d653dfe544a
        // -iv 000102030405060708090a0b0c0d0e0f`.
        let body = "f67ebc2bb37d4234d2b864d561c5645aba852ea5f4e33764d4c3d0174a98886a\
                    ca44a3aed12dbc840601589ff4c1da5a63ca2701f61e77d1074fb0b5f57cee4a";
        let mut sealed: Vec<u8> = (0..16).collect();
        sealed.extend(
            (0..body.len())
                .step_by(2)
                .map(|i| u8::from_str_radix(&body[i..i + 2], 16).unwrap()),
        );
        assert_eq!(
            decrypt(&sealed).unwrap(),
            br##"{"c1":{"#connection":{"user":"u","password":"p"}}}"##
        );
    }
}
