//! SSH tunnels through the system's OpenSSH client (`ssh`), for what the
//! built-in client cannot do: `~/.ssh/config` aliases, ProxyJump and jump
//! hosts, IdentityAgent (1Password, Secretive), RSA and every other key type,
//! and OpenSSH's own known_hosts.
//!
//! One `ssh -N` runs per tunnel. It forwards a Unix socket in a private
//! (0700) directory to the database, so no TCP port is opened for other
//! local processes (the same promise as the built-in client, see
//! [`crate::ssh`]); the socket appears once `ssh` has authenticated, which is
//! how the tunnel knows it is up.
//!
//! `ssh` never asks anything on a terminal: `SSH_ASKPASS` points at Kerno
//! itself (forced), and [`answer_askpass`] passes the question to the tunnel
//! over a second socket in that directory. The tunnel answers the passphrase
//! of its own key file (from the secret store), answers "yes" to a host-key
//! question only when the fingerprint shown is the one the user chose to
//! trust, and refuses everything else, passwords included. No secret is ever
//! in an argument or in the environment of `ssh`.
//!
//! The running tunnel is Unix's (`openssh/unix.rs`). Windows has no Unix
//! sockets in std or tokio: there OpenSSH is never offered yet, and the
//! built-in client is (`openssh/windows.rs`, K64, D-131).

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};

use crate::config::{Secret, SshAuth, SshConfig};

#[cfg(unix)]
mod unix;
#[cfg(windows)]
mod windows;
#[cfg(unix)]
pub(crate) use unix::LocalStream;
#[cfg(unix)]
pub use unix::{Forward, answer_askpass};
#[cfg(windows)]
pub(crate) use windows::LocalStream;
#[cfg(windows)]
pub use windows::{Forward, answer_askpass};

/// Variable that tells a child `ssh`'s askpass program (Kerno) which socket
/// to ask.
pub const ASKPASS_VAR: &str = "KERNO_ASKPASS";

/// Said when no `ssh` program can be used.
#[cfg(unix)]
pub const NOT_FOUND: &str = "OpenSSH was not found on this computer; use the built-in client";
/// Said when no `ssh` program can be used: on Windows, never (K64, D-131).
#[cfg(not(unix))]
pub const NOT_FOUND: &str = "Tunnels through the system's OpenSSH are not available on Windows yet; use the built-in client";

/// Where the system's OpenSSH is and how Kerno runs it. The app builds it
/// once; tests pass their own configuration file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenSshEnv {
    /// The `ssh` program.
    pub ssh: PathBuf,
    /// A configuration file passed as `-F` (`KERNO_SSH_CONFIG`, or a test's
    /// file). `None` lets `ssh` read its own (`~/.ssh/config`, then the
    /// system's).
    pub config: Option<PathBuf>,
    /// The file the dialog lists `Host` aliases from: `config`, else
    /// `~/.ssh/config`.
    pub aliases_from: Option<PathBuf>,
    /// The program `ssh` runs to ask a question: Kerno itself, which
    /// answers through [`answer_askpass`].
    pub askpass: PathBuf,
}

impl OpenSshEnv {
    /// Finds `ssh` on `path` (the `PATH` variable) and in the usual places
    /// a GUI app's short `PATH` misses. `None` when there is none, and on
    /// Windows, where the tunnel cannot run yet (`openssh/windows.rs`).
    pub fn detect(
        path: Option<&OsStr>,
        home: Option<&Path>,
        config: Option<PathBuf>,
        askpass: PathBuf,
    ) -> Option<Self> {
        if cfg!(windows) {
            return None;
        }
        let ssh = find_ssh(path)?;
        let aliases_from = config
            .clone()
            .or_else(|| home.map(|home| home.join(".ssh/config")));
        Some(Self {
            ssh,
            config,
            aliases_from,
            askpass,
        })
    }
}

/// The `ssh` program: the first on `path`, then `/usr/bin`,
/// `/opt/homebrew/bin` and `/usr/local/bin`.
pub fn find_ssh(path: Option<&OsStr>) -> Option<PathBuf> {
    let listed = path.map(std::env::split_paths).into_iter().flatten();
    let usual = ["/usr/bin", "/opt/homebrew/bin", "/usr/local/bin"].map(PathBuf::from);
    listed
        .chain(usual)
        .map(|dir| dir.join("ssh"))
        .find(|candidate| candidate.is_file())
}

// Arguments --------------------------------------------------------------------

/// The arguments of the `ssh` that forwards `socket` to `db_host:db_port`
/// as the SSH server sees it. No secret is ever in them.
pub fn tunnel_args(
    ssh: &SshConfig,
    config: Option<&Path>,
    socket: &Path,
    db_host: &str,
    db_port: u16,
) -> Vec<OsString> {
    let mut args: Vec<OsString> = Vec::new();
    let mut push = |arg: &str| args.push(arg.into());
    push("-N");
    for option in [
        "ExitOnForwardFailure=yes",
        // Questions go to Kerno (askpass), which refuses what it cannot
        // answer: a BatchMode set in the user's file would hide the key's
        // passphrase question.
        "BatchMode=no",
        "PasswordAuthentication=no",
        "KbdInteractiveAuthentication=no",
        // A ControlMaster of the user's would make this tunnel a client of
        // another process, or leave one behind.
        "ControlPath=none",
    ] {
        push("-o");
        push(option);
    }
    if ssh.keepalive_secs > 0 {
        push("-o");
        push(&format!("ServerAliveInterval={}", ssh.keepalive_secs));
        push("-o");
        push("ServerAliveCountMax=3");
    }
    if let Some(jump) = ssh.jump.as_deref().map(str::trim).filter(|j| !j.is_empty()) {
        push("-J");
        push(jump);
    }
    // Left out when empty: the SSH configuration provides them, and a
    // value on the command line would override it.
    if ssh.port > 0 {
        push("-p");
        push(&ssh.port.to_string());
    }
    if !ssh.user.trim().is_empty() {
        push("-l");
        push(ssh.user.trim());
    }
    let bracketed = if db_host.contains(':') {
        format!("[{db_host}]")
    } else {
        db_host.to_string()
    };
    let forward = format!("{}:{bracketed}:{db_port}", socket.display());
    if let Some(config) = config {
        args.push("-F".into());
        args.push(config.into());
    }
    match &ssh.auth {
        SshAuth::KeyFile { path } => {
            args.extend(["-i".into(), path.into()]);
            args.extend(["-o".into(), "IdentitiesOnly=yes".into()]);
        }
        SshAuth::Agent {
            socket: Some(agent),
        } => {
            let mut option = OsString::from("IdentityAgent=");
            option.push(agent);
            args.extend(["-o".into(), option]);
        }
        // From the SSH configuration and SSH_AUTH_SOCK; a password is
        // refused by `ConnectionConfig::validate`.
        SshAuth::Agent { socket: None } | SshAuth::Password => {}
    }
    args.extend(["-L".into(), forward.into()]);
    // `--`: a host field starting with `-` is a host, never an option.
    args.extend(["--".into(), ssh.host.trim().into()]);
    args
}

/// Whether `text` is safe as a host, user or jump-host list for `ssh`:
/// OpenSSH puts jump hosts in a shell command (ProxyJump), so nothing that a
/// shell would read as more than a word, and nothing `ssh` would read as an
/// option: neither a word nor the host after its `user@` starts with `-`, and
/// in a list that holds for each jump host, not only the first.
pub fn safe_ssh_word(text: &str, list: bool) -> bool {
    let word = |word: &str| {
        let host = word.rsplit('@').next().unwrap_or(word);
        !word.is_empty()
            && !word.starts_with('-')
            && !host.trim_start_matches('[').starts_with('-')
            && word.chars().all(|c| {
                c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_' | '@' | ':' | '[' | ']')
            })
    };
    if list {
        text.split(',').all(word)
    } else {
        word(text)
    }
}

// The tunnel ---------------------------------------------------------------------

/// What one OpenSSH tunnel needs.
#[derive(Debug, Clone)]
pub struct OpenSshRequest<'a> {
    /// The SSH settings.
    pub ssh: &'a SshConfig,
    /// The system's OpenSSH.
    pub env: &'a OpenSshEnv,
    /// The database, as the SSH server sees it.
    pub db_host: &'a str,
    /// The database port.
    pub db_port: u16,
    /// The passphrase of the key file, answered when `ssh` asks for it.
    pub key_passphrase: Option<&'a Secret>,
    /// A host-key fingerprint the user chose to trust (`SHA256:…`).
    pub trusted_fingerprint: Option<&'a str>,
}

// ~/.ssh/config ----------------------------------------------------------------

/// The `Host` aliases of an SSH configuration file and the files it
/// `Include`s, in order and without duplicates: names only, no patterns
/// (`*`, `?`, `!`). `Match` blocks are skipped. Relative includes are read
/// from the including file's directory; `~` is `home`.
pub fn host_aliases(config: &Path, home: Option<&Path>) -> Vec<String> {
    let mut aliases = Vec::new();
    read_aliases(config, home, 0, &mut aliases);
    aliases
}

fn read_aliases(file: &Path, home: Option<&Path>, depth: usize, out: &mut Vec<String>) {
    // ssh itself stops at 16; a loop of includes stops here.
    if depth > 8 {
        return;
    }
    let Ok(text) = std::fs::read_to_string(file) else {
        return;
    };
    let dir = file.parent().unwrap_or(Path::new("/"));
    let mut in_match = false;
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (keyword, rest) = line
            .split_once(|c: char| c.is_whitespace() || c == '=')
            .unwrap_or((line, ""));
        let words = config_words(rest.trim_start_matches(|c: char| c.is_whitespace() || c == '='));
        match keyword.to_ascii_lowercase().as_str() {
            "host" => {
                in_match = false;
                for word in words {
                    let pattern = word.contains(['*', '?', '!']);
                    if !pattern && !out.contains(&word) {
                        out.push(word);
                    }
                }
            }
            "match" => in_match = true,
            "include" if !in_match => {
                for word in words {
                    for path in include_paths(&word, dir, home) {
                        read_aliases(&path, home, depth + 1, out);
                    }
                }
            }
            _ => {}
        }
    }
}

/// The words of a configuration line, `"…"` quotes removed.
fn config_words(text: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut word = String::new();
    let mut quoted = false;
    for c in text.chars() {
        match c {
            '"' => quoted = !quoted,
            c if c.is_whitespace() && !quoted => {
                if !word.is_empty() {
                    words.push(std::mem::take(&mut word));
                }
            }
            c => word.push(c),
        }
    }
    if !word.is_empty() {
        words.push(word);
    }
    words
}

/// The files an `Include` names: `~` expanded, relative to `dir`, a glob
/// (`*`, `?`) in the file name matched against the directory, sorted.
fn include_paths(word: &str, dir: &Path, home: Option<&Path>) -> Vec<PathBuf> {
    let path = match (word.strip_prefix("~/"), home) {
        (Some(rest), Some(home)) => home.join(rest),
        _ => dir.join(word),
    };
    let Some(name) = path.file_name().and_then(OsStr::to_str) else {
        return Vec::new();
    };
    if !name.contains(['*', '?']) {
        return vec![path];
    }
    let parent = path.parent().unwrap_or(Path::new("/"));
    let Ok(entries) = std::fs::read_dir(parent) else {
        return Vec::new();
    };
    let mut found: Vec<PathBuf> = entries
        .flatten()
        .filter(|entry| {
            entry
                .file_name()
                .to_str()
                .is_some_and(|file| wildcard(name.as_bytes(), file.as_bytes()))
        })
        .map(|entry| entry.path())
        .filter(|path| path.is_file())
        .collect();
    found.sort();
    found
}

/// `*` and `?` matching, as ssh's globs do in a file name.
fn wildcard(pattern: &[u8], text: &[u8]) -> bool {
    match (pattern.first(), text.first()) {
        (None, None) => true,
        (Some(b'*'), _) => {
            wildcard(&pattern[1..], text) || (!text.is_empty() && wildcard(pattern, &text[1..]))
        }
        (Some(b'?'), Some(_)) => wildcard(&pattern[1..], &text[1..]),
        (Some(p), Some(t)) if p == t => wildcard(&pattern[1..], &text[1..]),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::SshClient;

    fn ssh(auth: SshAuth) -> SshConfig {
        SshConfig {
            host: "prod".into(),
            port: 0,
            user: String::new(),
            auth,
            keepalive_secs: 0,
            client: SshClient::OpenSsh,
            jump: None,
        }
    }

    fn words(args: &[OsString]) -> Vec<String> {
        args.iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect()
    }

    /// J2 (DBeaver #5845, #14749): every combination of fields, and never a
    /// secret on the command line.
    #[test]
    fn the_arguments_follow_the_fields() {
        let socket = Path::new("/tmp/kerno-ssh-1/db");
        let base = [
            "-N",
            "-o",
            "ExitOnForwardFailure=yes",
            "-o",
            "BatchMode=no",
            "-o",
            "PasswordAuthentication=no",
            "-o",
            "KbdInteractiveAuthentication=no",
            "-o",
            "ControlPath=none",
        ];
        // An alias alone: port, user and key come from the file.
        let alias = words(&tunnel_args(
            &ssh(SshAuth::Agent { socket: None }),
            None,
            socket,
            "db.internal",
            5432,
        ));
        let mut expected: Vec<&str> = base.to_vec();
        expected.extend(["-L", "/tmp/kerno-ssh-1/db:db.internal:5432", "--", "prod"]);
        assert_eq!(alias, expected);

        // Everything set.
        let mut full = ssh(SshAuth::KeyFile {
            path: "/home/ada/.ssh/id_rsa".into(),
        });
        full.port = 2222;
        full.user = "ada".into();
        full.keepalive_secs = 15;
        full.jump = Some(" ops@jump:22,edge ".into());
        let args = words(&tunnel_args(
            &full,
            Some(Path::new("/etc/kerno/ssh_config")),
            socket,
            "::1",
            3306,
        ));
        let mut expected: Vec<&str> = base.to_vec();
        expected.extend([
            "-o",
            "ServerAliveInterval=15",
            "-o",
            "ServerAliveCountMax=3",
            "-J",
            "ops@jump:22,edge",
            "-p",
            "2222",
            "-l",
            "ada",
            "-F",
            "/etc/kerno/ssh_config",
            "-i",
            "/home/ada/.ssh/id_rsa",
            "-o",
            "IdentitiesOnly=yes",
            "-L",
            "/tmp/kerno-ssh-1/db:[::1]:3306",
            "--",
            "prod",
        ]);
        assert_eq!(args, expected);

        // An agent socket of its own (1Password, Secretive).
        let agent = words(&tunnel_args(
            &ssh(SshAuth::Agent {
                socket: Some("/Users/ada/agent.sock".into()),
            }),
            None,
            socket,
            "db",
            5432,
        ));
        assert!(
            agent
                .windows(2)
                .any(|pair| pair == ["-o", "IdentityAgent=/Users/ada/agent.sock"]),
            "{agent:?}"
        );
        // A host that looks like an option stays a host.
        let mut sneaky = ssh(SshAuth::Agent { socket: None });
        sneaky.host = "-oProxyCommand=id".into();
        let args = words(&tunnel_args(&sneaky, None, socket, "db", 1));
        assert_eq!(args[args.len() - 2..], ["--", "-oProxyCommand=id"]);
    }

    #[test]
    fn ssh_words_are_shell_safe() {
        for word in ["bastion", "ops@10.0.0.1:2222", "[::1]:22", "a_b-c.d"] {
            assert!(safe_ssh_word(word, false), "{word}");
        }
        assert!(safe_ssh_word("a@j1:22,j2", true));
        assert!(!safe_ssh_word("a@j1:22,j2", false));
        for word in [
            "", "a b", "a;b", "$(id)", "`id`", "a|b", "-oX=y", "a'b", "a\"b",
        ] {
            assert!(!safe_ssh_word(word, true), "{word}");
        }
        // An option hidden after the first jump host, or after a user.
        for word in [
            "j1,-D8080",
            "j1,,j2",
            "j1,",
            "ops@-D8080:22",
            "ops@[-w0]:22",
        ] {
            assert!(!safe_ssh_word(word, true), "{word}");
        }
        assert!(!safe_ssh_word("ops@-D8080", false));
    }

    /// K64: on Windows the system's OpenSSH is not offered, even with an
    /// `ssh` on `PATH`, and the reason says Windows.
    #[cfg(windows)]
    #[test]
    fn openssh_is_not_offered_on_windows_yet() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("ssh"), "").unwrap();
        std::fs::write(dir.path().join("ssh.exe"), "").unwrap();
        let path = std::env::join_paths([dir.path()]).unwrap();
        assert_eq!(
            OpenSshEnv::detect(Some(&path), None, None, "k".into()),
            None
        );
        assert!(
            NOT_FOUND.contains("not available on Windows yet"),
            "{NOT_FOUND}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn ssh_is_found_on_path_or_in_the_usual_places() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("ssh"), "").unwrap();
        let path = std::env::join_paths([Path::new("/nonexistent"), dir.path()]).unwrap();
        assert_eq!(find_ssh(Some(&path)), Some(dir.path().join("ssh")));
        // A GUI app's short PATH: the usual places.
        if Path::new("/usr/bin/ssh").is_file() {
            assert_eq!(
                find_ssh(Some(OsStr::new("/nonexistent"))),
                Some(PathBuf::from("/usr/bin/ssh"))
            );
        }
        let env = OpenSshEnv::detect(Some(&path), Some(Path::new("/home/ada")), None, "/k".into())
            .unwrap();
        assert_eq!(
            env.aliases_from,
            Some(PathBuf::from("/home/ada/.ssh/config"))
        );
        assert_eq!(env.config, None);
    }

    /// Beekeeper #1344, DBeaver #4516: the dialog offers the user's aliases.
    #[test]
    fn host_aliases_follow_includes_and_skip_patterns() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("home");
        std::fs::create_dir_all(home.join(".ssh/conf.d")).unwrap();
        std::fs::create_dir_all(dir.path().join("work")).unwrap();
        let config = home.join(".ssh/config");
        std::fs::write(
            &config,
            "# mine\nHost prod-bastion staging *.internal !evil\n  User ops\n\
             Include conf.d/*.conf ~/.ssh/extra \"work dir\"\n\
             Match host db-*\n  Include ~/.ssh/never\nHost=db-jump\nHost prod-bastion\n\
             Include config\n",
        )
        .unwrap();
        std::fs::write(home.join(".ssh/conf.d/b.conf"), "host b-two\n").unwrap();
        std::fs::write(home.join(".ssh/conf.d/a.conf"), "HOST a-one a-?\n").unwrap();
        std::fs::write(home.join(".ssh/conf.d/skip.txt"), "Host not-included\n").unwrap();
        std::fs::write(home.join(".ssh/extra"), "Host extra\n").unwrap();
        std::fs::write(home.join(".ssh/never"), "Host in-match\n").unwrap();
        assert_eq!(
            host_aliases(&config, Some(&home)),
            [
                "prod-bastion",
                "staging",
                "a-one",
                "b-two",
                "extra",
                "db-jump"
            ]
        );
        assert!(host_aliases(&dir.path().join("missing"), None).is_empty());
    }
}
