//! Import from DBeaver (K41, D-114): the connections, folders and scripts of
//! a DBeaver project export (File ▸ Export ▸ Project, a `.dbp` zip) or of a
//! bare `data-sources.json`, mapped onto Kerno's connections.
//!
//! Ported from DBeaver Community (Apache-2.0, see THIRD_PARTY_NOTICES.md):
//! the file layout from `tools/project/ProjectExportWizard.java`, the
//! connection format from `registry/DataSourceSerializerModern.java` and
//! `registry/DataSourceParser.java`, the credentials file's cipher and key
//! from `model/impl/app/DefaultValueEncryptor.java` and `BaseProjectImpl.java`,
//! the script bindings from `BaseProjectImpl.loadMetadata`, an SSH tunnel's
//! jump servers from `net/ssh/SSHUtils.loadHostConfigurations` (K58), the
//! databases a PostgreSQL connection shows from `ext/postgresql/PostgreConstants`
//! and `model/PostgreDataSource.prepareReadDatabaseListStatement` (K62).
//! Lenient: unknown fields are ignored, missing ones defaulted.
//!
//! The archive is untrusted: its entries are read in memory, never written
//! to disk, each one and all of them together under a cap, and an entry
//! whose path goes up or is absolute is ignored.

use std::collections::{BTreeMap, BTreeSet};
use std::io::{Cursor, Read};
use std::path::PathBuf;

use serde_json::{Map, Value};

use crate::config::{
    ConnectionConfig, DatabasesShown, Engine, Secret, Secrets, SshAuth, SshClient, SshConfig,
    TlsMode, folder_path,
};
use crate::environment::{self, EnvironmentDef, EnvironmentRules, Tint};

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
    /// DBeaver's own connection types its connections use that match no
    /// Kerno environment by name (K46): the environments to create, each
    /// with an id unused here.
    pub new_environments: Vec<EnvironmentDef>,
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
    /// What DBeaver set that Kerno left out or at its default, each said
    /// with what to check, in plain words.
    pub not_mapped: Vec<String>,
    /// The saved password and SSH secret; empty unless asked for.
    pub secrets: Secrets,
    /// The id, in [`Export::new_environments`], of the environment made of
    /// its DBeaver type; its `config` stays in local until that is created.
    pub new_environment: Option<String>,
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
                    &mut export.new_environments,
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
        // DBeaver reads a properties map back as doubles and writes it so
        // (Gson): a port saved once comes out as `2222.0`. A whole number
        // is its integer, whatever way it was written.
        Value::Number(number) => match number.as_f64() {
            Some(value) if number.as_i64().is_none() && value.fract() == 0.0 => {
                format!("{value:.0}")
            }
            _ => number.to_string(),
        },
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

/// What a DBeaver PostgreSQL connection shows of the server's databases
/// (K62): its "Show all databases" off, DBeaver's default, lists the
/// default database alone; on, those that take connections and are no
/// template, unless "Show unavailable databases" or "Show template
/// databases" is on too. The switches are provider properties, as text.
fn databases_shown(cfg: &Map<String, Value>) -> DatabasesShown {
    let empty = Map::new();
    let properties = cfg
        .get("provider-properties")
        .and_then(Value::as_object)
        .unwrap_or(&empty);
    let on = |key: &str| flag(properties, key).unwrap_or(false);
    if !on("@dbeaver-show-non-default-db@") {
        DatabasesShown::DefaultOnly
    } else if on("@dbeaver-show-unavailable-db@") || on("@dbeaver-show-template-db@") {
        DatabasesShown::All
    } else {
        DatabasesShown::Available
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
    new_environments: &mut Vec<EnvironmentDef>,
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
        new_environment: None,
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
    if engine == Engine::Postgres {
        config.databases_shown = databases_shown(cfg);
    }
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
            Err(_) => not_mapped.push(format!(
                "port “{}” is not a number: set to {}, check it",
                connection.port, config.port
            )),
        }
        // Not in the file (a project password): the engine's default user.
        if connection.user.is_empty() {
            not_mapped.push(format!(
                "no user name in the file (a project password?): set to {}, enter yours",
                config.user
            ));
        } else {
            config.user = connection.user.clone();
        }
        config.database = (!connection.database.is_empty()).then(|| connection.database.clone());
        connection.secrets.password = password.map(Secret::new);
    }
    // The connection type: DBeaver's three built-ins, else by its name.
    let mut new_environment = None;
    let kind = text(cfg, "type").unwrap_or_else(|| "dev".into());
    config.environment = match kind.as_str() {
        "dev" => "local".into(),
        "test" => "staging".into(),
        "prod" => "production".into(),
        other => {
            let def = types
                .iter()
                .find(|(type_id, _)| type_id.as_str() == other)
                .map(|(_, def)| *def);
            let name = def
                .and_then(|def| text(def, "name"))
                .unwrap_or_else(|| other.to_string());
            let same = |def: &&EnvironmentDef| def.name.eq_ignore_ascii_case(&name);
            match environments.iter().find(|d| same(d) || d.id == other) {
                Some(def) => def.id.clone(),
                // Defined in the file: an environment to create (K46).
                None if def.is_some() => {
                    let id = match new_environments.iter().find(same) {
                        Some(made) => made.id.clone(),
                        None => {
                            let taken: Vec<EnvironmentDef> = (environments.iter())
                                .chain(new_environments.iter())
                                .cloned()
                                .collect();
                            let id = environment::unused_id(&taken, &name);
                            new_environments.push(environment_of(id.clone(), name, def));
                            id
                        }
                    };
                    new_environment = Some(id);
                    environment::LOCAL.into()
                }
                // Named but not defined: DBeaver itself takes its default.
                None => {
                    not_mapped.push(format!(
                        "connection type “{name}” is not defined in the file: put in Local"
                    ));
                    environment::LOCAL.into()
                }
            }
        }
    };
    if let Some(color) = text(cfg, "color") {
        match palette_color(&color, palette) {
            Some(key) => config.color = Some(key.to_string()),
            None => not_mapped.push(format!(
                "colour {color} is close to no Kerno colour: none set, choose one"
            )),
        }
    }
    let handlers = object(cfg, "handlers");
    let mut jumps = None;
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
                            not_mapped.push(
                                "SSH key kept inside DBeaver, not in a file: the tunnel tries \
                                 your SSH agent; save the key to a file and choose it on the \
                                 SSH tab"
                                    .into(),
                            );
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
                jumps = Some(jump_servers(props));
                config.ssh = Some(SshConfig {
                    host: text(props, "host").unwrap_or_default(),
                    port: text(props, "port")
                        .and_then(|port| port.parse().ok())
                        .unwrap_or(22),
                    // Without one, DBeaver logs in as the local user.
                    user: text(&creds, "user")
                        .or_else(|| text(settings, "user"))
                        .unwrap_or_else(|| {
                            not_mapped
                                .push("no SSH user in the file: set to yours, check it".into());
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
                    not_mapped.push(
                        "certificate pasted in DBeaver, not in a file: save it to a file and \
                         choose it on the SSL tab"
                            .into(),
                    );
                }
                config.tls.mode = if handler.as_str() == "postgre_ssl" {
                    match text(props, "sslMode").as_deref().map(TlsMode::parse) {
                        Some(Some(mode)) => mode,
                        Some(None) => {
                            not_mapped
                                .push("SSL mode unknown to Kerno: set to require, check it".into());
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
            other => not_mapped.push(format!(
                "network setting “{other}” left out: Kerno has no such setting"
            )),
        }
    }
    if let Some(hops) = jumps {
        apply_jumps(&mut config, hops, not_mapped);
    }
    // Windows: a file on another computer is never opened (K69, D-138).
    not_mapped.extend(crate::local_path::notes(&config));
    connection.new_environment = new_environment;
    connection.config = match config.validate() {
        Ok(()) => Ok(config),
        Err(crate::Error::Config(reason)) => Err(format!("Its settings are incomplete: {reason}.")),
        Err(error) => Err(format!("Its settings are incomplete: {error}.")),
    };
    connection
}

/// A jump server of an SSH tunnel (K58, D-124).
struct Hop {
    /// As `ssh -J` takes it, which is how the connection dialog saves it:
    /// `[user@]host:port`.
    spec: String,
    /// What it logs in with that Kerno cannot give it, said.
    note: Option<String>,
}

/// The jump servers of a tunnel's `properties`, as DBeaver's
/// `SSHUtils.loadHostConfigurations` reads them: `jumpServer<i>.` keys from
/// the first hop on, while `enabled`, at most `jumpServer.count` (5 when
/// absent). A hop with no host refuses the chain, as DBeaver does.
fn jump_servers(props: &Map<String, Value>) -> Result<Vec<Hop>, String> {
    let count = text(props, "jumpServer.count")
        .and_then(|count| count.parse::<usize>().ok())
        .unwrap_or(5);
    let mut hops = Vec::new();
    for index in 0..count {
        let key = |name: &str| format!("jumpServer{index}.{name}");
        if !flag(props, &key("enabled")).unwrap_or(false) {
            break;
        }
        let Some(host) = text(props, &key("host")) else {
            return Err(format!(
                "SSH jump server {} has no host in the file",
                index + 1
            ));
        };
        let port = text(props, &key("port"))
            .and_then(|port| port.parse::<u16>().ok())
            .filter(|port| *port > 0)
            .unwrap_or(22);
        let address = if host.contains(':') {
            format!("[{host}]")
        } else {
            host.clone()
        };
        // Without one, OpenSSH logs in as the local user, as DBeaver does.
        let user = text(props, &key("name"))
            .map(|user| format!("{user}@"))
            .unwrap_or_default();
        // `-J` hands a hop Kerno's `-F` file only: no key file, agent socket,
        // password or passphrase of its own (`openssh::tunnel_args`).
        let hop = format!("jump server “{host}”");
        let note = match text(props, &key("authType")).as_deref() {
            Some("AGENT") => text(props, &key("agentAuthSockPath")).map(|socket| {
                format!("{hop} logs in with your default SSH agent, not the one at {socket}")
            }),
            Some("PUBLIC_KEY") => Some(match text(props, &key("keyPath")) {
                // The path is the file's: said as what DBeaver had, never inside a
                // command a user could paste into a terminal.
                Some(path) => format!(
                    "{hop}: its key file is not passed to it (DBeaver used “{path}”): load that \
                     key with ssh-add, or name it for that host in ~/.ssh/config"
                ),
                None => format!(
                    "{hop}: its SSH key is kept inside DBeaver: save it to a file, then load it \
                     with ssh-add or name it for that host in ~/.ssh/config"
                ),
            }),
            _ => Some(format!(
                "{hop} logs in with a password, which Kerno cannot give it: set up a key for it \
                 (your SSH agent or ~/.ssh/config)"
            )),
        };
        hops.push(Hop {
            spec: format!("{user}{address}:{port}"),
            note,
        });
    }
    Ok(hops)
}

/// Puts a tunnel's jump servers on Kerno's (K58, D-124): the system's
/// OpenSSH and its `-J` list, which holds a chain, as the connection dialog
/// saves them. Left out, and said, when the tunnel logs in with a password
/// (OpenSSH cannot) or when OpenSSH could not take the result.
fn apply_jumps(
    config: &mut ConnectionConfig,
    hops: Result<Vec<Hop>, String>,
    not_mapped: &mut Vec<String>,
) {
    let hops = match hops {
        Ok(hops) => hops,
        Err(why) => {
            not_mapped.push(format!(
                "{why}: no jump server imported, add them on the SSH tab"
            ));
            return;
        }
    };
    let Some(ssh) = config.ssh.as_ref().filter(|_| !hops.is_empty()) else {
        return;
    };
    let chain: Vec<&str> = hops.iter().map(|hop| hop.spec.as_str()).collect();
    let named = format!(
        "SSH jump {} {}",
        if hops.len() == 1 { "server" } else { "servers" },
        chain.join(", ")
    );
    if ssh.auth == SshAuth::Password {
        not_mapped.push(format!(
            "{named} left out: Kerno goes through jump servers with the system's OpenSSH, which \
             does not log in to the tunnel with a password: choose a key file or your SSH agent \
             on the SSH tab, then add them as jump hosts"
        ));
        return;
    }
    let mut with = config.clone();
    if let Some(ssh) = with.ssh.as_mut() {
        ssh.client = SshClient::OpenSsh;
        ssh.jump = Some(chain.join(","));
    }
    match with.validate() {
        Err(error) if config.validate().is_ok() => {
            let why = match error {
                crate::Error::Config(why) => why,
                other => other.to_string(),
            };
            not_mapped.push(format!("{named} left out ({why}): add them on the SSH tab"));
        }
        _ => {
            *config = with;
            not_mapped.extend(hops.into_iter().filter_map(|hop| hop.note));
        }
    }
}

/// A Kerno environment made of a DBeaver connection type (the keys
/// `DataSourceSerializerModern` reads into a `DBPConnectionType`): its name,
/// colours and the rules Kerno has. A flag missing from the file is false,
/// as DBeaver reads it; smart commit and closing idle connections have no
/// Kerno rule and are left out.
fn environment_of(id: String, name: String, def: Option<&Map<String, Value>>) -> EnvironmentDef {
    let empty = Map::new();
    let def = def.unwrap_or(&empty);
    let on = |key: &str| flag(def, key).unwrap_or(false);
    let color_light = text(def, "color").and_then(|rgb| hex_color(&rgb));
    let color_dark = text(def, "colorDark")
        .and_then(|rgb| hex_color(&rgb))
        .or_else(|| color_light.clone());
    let period = text(def, "close-transactions-period").and_then(|p| p.parse::<u32>().ok());
    EnvironmentDef {
        id,
        name,
        tint: if color_light.is_some() {
            Tint::Soft
        } else {
            Tint::None
        },
        color_light,
        color_dark,
        rules: EnvironmentRules {
            auto_commit: on("auto-commit"),
            confirm_execute: on("confirm-execute"),
            confirm_edits: on("confirm-data-change"),
            confirm_writes_twice: false,
            read_only: false,
            close_idle_transactions: period
                .filter(|period| on("auto-close-transactions") && *period > 0),
        },
    }
}

/// DBeaver's `r,g,b` as `#RRGGBB`; white, DBeaver's "no colour", is none.
fn hex_color(color: &str) -> Option<String> {
    let parts: Vec<u8> = color
        .split(',')
        .map(|part| part.trim().parse::<u8>())
        .collect::<Result<_, _>>()
        .ok()?;
    match parts.as_slice() {
        [255, 255, 255] => None,
        [r, g, b] => Some(format!("#{r:02X}{g:02X}{b:02X}")),
        _ => None,
    }
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
                    "provider-properties": {"@dbeaver-show-non-default-db@": "true"},
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
        // "Show all databases" on, nothing else (K62).
        assert_eq!(config.databases_shown, DatabasesShown::Available);
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
        // MySQL lists what the account sees: nothing to choose (K62).
        assert_eq!(blog.databases_shown, DatabasesShown::All);

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
        assert_eq!(
            legacy.not_mapped,
            ["colour 12,34,56 is close to no Kerno colour: none set, choose one"]
        );

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

    /// K62: an imported PostgreSQL connection shows what DBeaver showed:
    /// "Show all databases" off (or absent, DBeaver's default) is the
    /// default database alone; on, those this account can open, or all
    /// with unavailable or template databases shown too.
    #[test]
    fn the_databases_a_postgres_connection_shows_follow_dbeaver() {
        let shown = |properties: Value| {
            let cfg = serde_json::json!({ "provider-properties": properties });
            databases_shown(cfg.as_object().unwrap())
        };
        let non_default = "@dbeaver-show-non-default-db@";
        let unavailable = "@dbeaver-show-unavailable-db@";
        let template = "@dbeaver-show-template-db@";
        assert_eq!(shown(serde_json::json!({})), DatabasesShown::DefaultOnly);
        assert_eq!(
            shown(serde_json::json!({ non_default: "false", template: "true" })),
            DatabasesShown::DefaultOnly
        );
        assert_eq!(
            shown(serde_json::json!({ non_default: "true" })),
            DatabasesShown::Available
        );
        assert_eq!(
            shown(serde_json::json!({ non_default: "true", unavailable: "true" })),
            DatabasesShown::All
        );
        assert_eq!(
            shown(serde_json::json!({ non_default: "true", template: "true" })),
            DatabasesShown::All
        );
        let none = serde_json::json!({});
        assert_eq!(
            databases_shown(none.as_object().unwrap()),
            DatabasesShown::DefaultOnly
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

    /// A user's report of 2026-11-23: a tunnel on port 2222 was imported
    /// on 22. DBeaver writes a handler's numbers as it read them, doubles;
    /// a jump server's port and count too (K58).
    #[test]
    fn a_port_written_as_a_double_is_read() {
        for (written, port) in [
            (serde_json::json!(2222.0), 2222),
            (serde_json::json!(2222), 2222),
            (serde_json::json!("2222"), 2222),
        ] {
            let file = serde_json::json!({
                "connections": {"postgres-jdbc-1": {
                    "provider": "postgresql", "driver": "postgres-jdbc", "name": "Shop",
                    "configuration": {
                        "host": "db.example.com", "port": 5433.0, "database": "shop",
                        "handlers": {"ssh_tunnel": {
                            "enabled": true,
                            "properties": {"host": "bastion.example.com", "port": written,
                                "authType": "AGENT", "jumpServer.count": 1.0,
                                "jumpServer0.enabled": true,
                                "jumpServer0.host": "jump.example.com",
                                "jumpServer0.port": written, "jumpServer0.authType": "AGENT"}
                        }}
                    }
                }}
            });
            let export = read(file.to_string().as_bytes(), false, &envs(), &PALETTE).unwrap();
            let config = find(&export, "Shop").config.as_ref().unwrap();
            assert_eq!(config.port, 5433, "{file}");
            assert_eq!(config.ssh.as_ref().unwrap().port, port, "{file}");
            assert_eq!(
                config.ssh.as_ref().unwrap().jump.as_deref(),
                Some(format!("jump.example.com:{port}").as_str()),
                "{file}"
            );
        }
    }

    /// A data-sources file with one PostgreSQL connection whose tunnel has
    /// these `properties`, and the tunnel's saved `credentials`.
    fn tunnel(properties: Value, credentials: Value) -> Vec<u8> {
        let sources = serde_json::json!({"connections": {"pg": {
            "provider": "postgresql", "driver": "postgres-jdbc", "name": "Shop",
            "configuration": {"host": "db.internal", "port": "5432", "user": "u",
                "handlers": {"ssh_tunnel": {"enabled": true, "save-password": true,
                    "properties": properties}}}
        }}});
        let credentials = serde_json::json!({"pg": {"network/ssh_tunnel": credentials}});
        let credentials = encrypt(credentials.to_string().as_bytes(), [3; 16]);
        archive(
            "P",
            &[
                (".dbeaver/data-sources.json", sources.to_string().as_bytes()),
                (".dbeaver/credentials-config.json", &credentials),
            ],
        )
    }

    /// K58 (D-124): the jump servers of a tunnel, written with the keys of
    /// DBeaver's `SSHUtils.saveHostConfiguration`, become Kerno's jump hosts
    /// through the system's OpenSSH, in their order, as the connection dialog
    /// saves them; what a hop logs in with that Kerno cannot give it is said.
    #[test]
    fn a_tunnels_jump_servers_become_openssh_jump_hosts() {
        let read_one = |properties: Value, credentials: Value| {
            let file = tunnel(properties, credentials);
            let mut export = read(&file, true, &envs(), &PALETTE).unwrap();
            export.connections.remove(0)
        };
        // One hop with its key file, behind a tunnel with a key file.
        let shop = read_one(
            serde_json::json!({
                "host": "bastion.example.com", "port": 22.0, "authType": "PUBLIC_KEY",
                "keyPath": "/home/me/.ssh/id_ed25519",
                "jumpServer.count": 1.0,
                "jumpServer0.enabled": true, "jumpServer0.host": "jump.example.com",
                "jumpServer0.port": 2222.0, "jumpServer0.name": "hop",
                "jumpServer0.authType": "PUBLIC_KEY", "jumpServer0.keyPath": "/home/me/.ssh/hop"
            }),
            serde_json::json!({"user": "ops", "password": "key-secret",
                "jumpServer0.password": "hop-secret"}),
        );
        let config = shop.config.as_ref().unwrap();
        let ssh = config.ssh.as_ref().unwrap();
        assert_eq!(
            (ssh.client, ssh.jump.as_deref(), ssh.host.as_str(), ssh.port),
            (
                SshClient::OpenSsh,
                Some("hop@jump.example.com:2222"),
                "bastion.example.com",
                22
            )
        );
        assert_eq!(
            ssh.auth,
            SshAuth::KeyFile {
                path: "/home/me/.ssh/id_ed25519".into()
            }
        );
        let mut expected = vec![
            "jump server “jump.example.com”: its key file is not passed to it (DBeaver used \
             “/home/me/.ssh/hop”): load that key with ssh-add, or name it for that host in \
             ~/.ssh/config"
                .to_string(),
        ];
        // Windows opens a drive's path only (K69, D-138).
        if cfg!(windows) {
            expected.push(format!(
                "the SSH key /home/me/.ssh/id_ed25519 is not a file of this computer: it will \
                 not be opened; choose {}",
                crate::local_path::LOCAL_FILE
            ));
        }
        assert_eq!(shop.not_mapped, expected);
        // The hop's saved passphrase has no place in Kerno: never in the tunnel's.
        assert_eq!(
            shop.secrets.ssh_key_passphrase.as_ref().map(Secret::expose),
            Some("key-secret")
        );
        assert_eq!(shop.secrets.ssh_password, None);
        assert!(!format!("{:?}", shop.secrets).contains("hop-secret"));

        // Two hops through the agent, the second without a user, on IPv6, its own agent
        // socket said; the count absent (DBeaver reads up to five); a third hop not enabled
        // ends the chain, a fourth after it is not read.
        let chain = read_one(
            serde_json::json!({
                "host": "bastion.example.com", "port": "22", "authType": "AGENT",
                "jumpServer0.enabled": true, "jumpServer0.host": "first.example.com",
                "jumpServer0.name": "ops", "jumpServer0.authType": "AGENT",
                "jumpServer1.enabled": "true", "jumpServer1.host": "fd00::5",
                "jumpServer1.port": 0, "jumpServer1.authType": "AGENT",
                "jumpServer1.agentAuthSockPath": "/tmp/agent.sock",
                "jumpServer2.enabled": false, "jumpServer2.host": "off.example.com",
                "jumpServer3.enabled": true, "jumpServer3.host": "after.example.com"
            }),
            serde_json::json!({"user": "ops"}),
        );
        let ssh = chain.config.as_ref().unwrap().ssh.as_ref().unwrap();
        assert_eq!(
            (ssh.client, ssh.jump.as_deref()),
            (
                SshClient::OpenSsh,
                Some("ops@first.example.com:22,[fd00::5]:22")
            )
        );
        assert_eq!(
            chain.not_mapped,
            [
                "jump server “fd00::5” logs in with your default SSH agent, not the one at \
                 /tmp/agent.sock"
            ]
        );

        // A tunnel that logs in with a password: OpenSSH cannot, the hop is left out, said;
        // the tunnel and its password as before.
        let password = read_one(
            serde_json::json!({
                "host": "bastion.example.com", "port": 22,
                "jumpServer.count": 1, "jumpServer0.enabled": true,
                "jumpServer0.host": "jump.example.com", "jumpServer0.port": 22,
                "jumpServer0.name": "hop"
            }),
            serde_json::json!({"user": "ops", "password": "ssh-secret",
                "jumpServer0.password": "hop-secret"}),
        );
        let ssh = password.config.as_ref().unwrap().ssh.as_ref().unwrap();
        assert_eq!(
            (ssh.client, ssh.jump.as_deref(), &ssh.auth),
            (SshClient::Builtin, None, &SshAuth::Password)
        );
        assert_eq!(
            password.secrets.ssh_password.as_ref().map(Secret::expose),
            Some("ssh-secret")
        );
        assert_eq!(
            password.not_mapped,
            [
                "SSH jump server hop@jump.example.com:22 left out: Kerno goes through jump servers \
                 with the system's OpenSSH, which does not log in to the tunnel with a password: \
                 choose a key file or your SSH agent on the SSH tab, then add them as jump hosts"
            ]
        );

        // A hop OpenSSH could not take, a hop with no host: left out, said, the tunnel kept.
        for (hop, said) in [
            (
                serde_json::json!({"jumpServer0.enabled": true, "jumpServer0.host": "jump;id",
                    "jumpServer0.authType": "AGENT"}),
                "SSH jump server jump;id:22 left out (the jump host “jump;id:22” has characters \
                 OpenSSH cannot take: use letters, digits and . - _ @ : [ ] (and , between \
                 hosts)): add them on the SSH tab",
            ),
            (
                serde_json::json!({"jumpServer0.enabled": true, "jumpServer0.authType": "AGENT"}),
                "SSH jump server 1 has no host in the file: no jump server imported, add them on \
                 the SSH tab",
            ),
        ] {
            let mut properties = serde_json::json!({"host": "bastion.example.com",
                "authType": "AGENT"});
            properties
                .as_object_mut()
                .unwrap()
                .extend(hop.as_object().unwrap().clone());
            let left = read_one(properties, serde_json::json!({"user": "ops"}));
            let ssh = left.config.as_ref().unwrap().ssh.as_ref().unwrap();
            assert_eq!(
                (ssh.client, ssh.jump.as_deref()),
                (SshClient::Builtin, None)
            );
            assert_eq!(left.not_mapped, [said]);
        }

        // A hop with a password, one with its key kept inside DBeaver: imported, said.
        let said = read_one(
            serde_json::json!({
                "host": "bastion.example.com", "authType": "AGENT",
                "jumpServer0.enabled": true, "jumpServer0.host": "a.example.com",
                "jumpServer1.enabled": true, "jumpServer1.host": "b.example.com",
                "jumpServer1.authType": "PUBLIC_KEY"
            }),
            serde_json::json!({"user": "ops", "jumpServer1.keyValue": "-----BEGIN"}),
        );
        let ssh = said.config.as_ref().unwrap().ssh.as_ref().unwrap();
        assert_eq!(
            ssh.jump.as_deref(),
            Some("a.example.com:22,b.example.com:22")
        );
        assert_eq!(
            said.not_mapped,
            [
                "jump server “a.example.com” logs in with a password, which Kerno cannot give it: \
                 set up a key for it (your SSH agent or ~/.ssh/config)",
                "jump server “b.example.com”: its SSH key is kept inside DBeaver: save it to a \
                 file, then load it with ssh-add or name it for that host in ~/.ssh/config",
            ]
        );
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

    /// K46: a DBeaver type of its own becomes one environment to create,
    /// with its colour and rules; a type Kerno has by name is used; a type
    /// named but not defined stays local.
    #[test]
    fn dbeavers_own_types_become_environments_to_create() {
        let json = br#"{"connection-types": {
                "pp": {"name": "Preprod", "color": "214,250,207", "colorDark": "64,89,66",
                    "auto-commit": false, "confirm-execute": true, "confirm-data-change": true,
                    "auto-close-transactions": true, "close-transactions-period": 900},
                "qa": {"name": "staging", "color": "255,255,255"},
                "dev": {"name": "Development", "color": "255,255,255", "auto-commit": true}
            },
            "connections": {
                "a": {"provider": "postgresql", "driver": "postgres-jdbc", "name": "A",
                    "configuration": {"host": "a", "user": "u", "type": "pp"}},
                "b": {"provider": "mysql", "driver": "mysql8", "name": "B",
                    "configuration": {"host": "b", "user": "u", "type": "pp"}},
                "c": {"provider": "mysql", "driver": "mysql8", "name": "C",
                    "configuration": {"host": "c", "user": "u", "type": "qa"}},
                "d": {"provider": "mysql", "driver": "mysql8", "name": "D",
                    "configuration": {"host": "d", "user": "u", "type": "gone"}},
                "e": {"provider": "mysql", "driver": "mysql8", "name": "E",
                    "configuration": {"host": "e", "user": "u", "type": "dev"}}
            }}"#;
        let export = read(json, false, &envs(), &PALETTE).unwrap();
        assert_eq!(
            export.new_environments,
            [EnvironmentDef {
                id: "preprod".into(),
                name: "Preprod".into(),
                color_light: Some("#D6FACF".into()),
                color_dark: Some("#405942".into()),
                tint: Tint::Soft,
                rules: EnvironmentRules {
                    auto_commit: false,
                    confirm_execute: true,
                    confirm_edits: true,
                    confirm_writes_twice: false,
                    read_only: false,
                    close_idle_transactions: Some(900),
                },
            }]
        );
        for name in ["A", "B"] {
            let connection = find(&export, name);
            assert_eq!(connection.new_environment.as_deref(), Some("preprod"));
            assert_eq!(connection.config.as_ref().unwrap().environment, "local");
            assert!(
                connection.not_mapped.is_empty(),
                "{:?}",
                connection.not_mapped
            );
        }
        let env = |name| {
            let connection = find(&export, name);
            assert_eq!(connection.new_environment, None);
            connection.config.as_ref().unwrap().environment.clone()
        };
        assert_eq!(
            [env("C"), env("D"), env("E")],
            ["staging", "local", "local"]
        );
        assert_eq!(
            find(&export, "D").not_mapped,
            ["connection type “gone” is not defined in the file: put in Local"]
        );
        // Once Preprod is here, nothing is created again.
        let mut here = envs();
        here.push(export.new_environments[0].clone());
        let again = read(json, false, &here, &PALETTE).unwrap();
        assert!(again.new_environments.is_empty());
        let a = find(&again, "A");
        assert_eq!(a.config.as_ref().unwrap().environment, "preprod");
        assert_eq!(a.new_environment, None);
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
            ["no user name in the file (a project password?): set to root, enter yours"]
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
