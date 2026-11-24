//! The tunnel through Windows' own `ssh.exe` (K69, D-138), against the
//! OpenSSH server and the PostgreSQL server a throwaway Windows runner starts
//! (`.github/workflows/windows-ssh-probe.yml`). Each test writes its own
//! `ssh_config` and known_hosts but the alias test, which reads the profile's
//! `.ssh\config` the workflow wrote. Skipped elsewhere (`testing::k69::runner`).

#![cfg(windows)]

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use kerno_core::Error;
use kerno_core::config::{ConnectionConfig, Secret, Secrets, SshAuth};
use kerno_core::connector::{HostKeyStore, open_tunnel};
use kerno_core::error::{FailureKind, explain};
use kerno_core::openssh::{OpenSshEnv, find_ssh, host_aliases};
use kerno_core::ssh::{SshError, Tunnel};
use kerno_core::testing::k69::{self, ssh_processes, wait_until_gone};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// The askpass program: what the app's `main` does first, alone.
const ASKPASS: &str = env!("CARGO_BIN_EXE_kerno-askpass");

/// The askpass program of the run: `KERNO_K69_ASKPASS` (the workflow names
/// the release's windowed `kerno.exe` for a second pass of the question
/// tests), else [`ASKPASS`]. Where the workflow says the release is tested
/// (`KERNO_K69_RELEASE`), anything but `kerno.exe` fails the test (S7).
fn askpass() -> PathBuf {
    let chosen =
        std::env::var_os("KERNO_K69_ASKPASS").map_or_else(|| ASKPASS.into(), PathBuf::from);
    if std::env::var_os("KERNO_K69_RELEASE").is_some() {
        assert!(
            chosen
                .file_name()
                .is_some_and(|name| name.eq_ignore_ascii_case("kerno.exe")),
            "the release step must ask through the shipped kerno.exe, not {}",
            chosen.display()
        );
    }
    chosen
}

/// The file at `path`, by its full name (`RUNNER~1` and the like resolved).
fn canonical(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|error| panic!("{}: {error}", path.display()))
}

/// Every `ssh.exe` of the tunnel whose folder is `marker`, a jump host's
/// included: each one's executable is `ssh` (the file Windows started,
/// whatever a command line or `PATH` says), and each reads the tunnel's own
/// file (`-F <marker>/config`, the guard's rules). How many there are (S1).
fn runs_only(marker: &str, ssh: &Path) -> usize {
    let running = k69::ssh_executables(marker);
    eprintln!("ssh.exe of the tunnel {marker}: {running:#?}");
    for (path, command) in &running {
        assert_eq!(
            canonical(path),
            canonical(ssh),
            "{} runs, not {}: {command}",
            path.display(),
            ssh.display()
        );
        assert!(
            command.contains(&format!("-F {marker}/config")),
            "{command}"
        );
    }
    running.len()
}

/// The passphrase of the keys made with one.
const PASSPHRASE: &str = k69::PASSPHRASE;

/// PostgreSQL's SSLRequest packet: the runner's server has no TLS and
/// answers `N`.
const SSL_REQUEST: [u8; 8] = [0, 0, 0, 8, 0x04, 0xd2, 0x16, 0x2f];

fn key(name: &str) -> SshAuth {
    SshAuth::KeyFile {
        path: k69::key(name),
    }
}

async fn open(
    env: &OpenSshEnv,
    config: &ConnectionConfig,
    passphrase: Option<&str>,
    trusted: Option<&str>,
) -> Result<Arc<Tunnel>, SshError> {
    let secrets = Secrets {
        ssh_key_passphrase: passphrase.map(Secret::new),
        ..Secrets::default()
    };
    let host_keys = HostKeyStore {
        trusted_fingerprint: trusted.map(str::to_string),
        openssh: Some(env.clone()),
        ..HostKeyStore::default()
    };
    match open_tunnel(config, &secrets, &host_keys).await {
        Ok(tunnel) => Ok(tunnel.expect("a tunnel")),
        Err(Error::Ssh(error)) => Err(*error),
        Err(other) => panic!("expected an SSH error, got {other}"),
    }
}

/// Checks that PostgreSQL answers at the end of the tunnel.
async fn probe(tunnel: &Tunnel, config: &ConnectionConfig) {
    let mut stream = tunnel
        .open_stream(&config.host, config.port)
        .await
        .expect("open a stream");
    let read = async {
        stream.write_all(&SSL_REQUEST).await.unwrap();
        let mut answer = [0u8; 1];
        stream.read_exact(&mut answer).await.unwrap();
        assert_eq!(answer[0], b'N', "PostgreSQL answers the SSLRequest");
    };
    tokio::time::timeout(Duration::from_secs(15), read)
        .await
        .expect("no answer through the tunnel");
}

/// The name of the tunnel's private folder, which every `ssh.exe` of it
/// names (`-F <name>/config`).
fn marker(tunnel: &Tunnel) -> String {
    tunnel
        .openssh_dir()
        .and_then(|dir| dir.file_name())
        .expect("an OpenSSH tunnel")
        .to_string_lossy()
        .into_owned()
}

#[tokio::test]
async fn windows_ssh_exe_reaches_postgres() {
    if !k69::runner() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let fixture = k69::openssh(dir.path(), askpass(), true);
    // An ssh.exe comes before System32's on this step's PATH (Git's, in a
    // bash step): a search of PATH would take it. Kerno takes Windows' own
    // (R3), and Windows ran that file, the jump host's included (S1).
    let first = which_ssh().expect("an ssh.exe on PATH");
    eprintln!("PATH's first ssh: {}", first.display());
    assert_ne!(canonical(&first), canonical(&k69::system32("ssh.exe")));
    assert_eq!(fixture.env.ssh, k69::system32("ssh.exe"));
    let jump = format!("{}@{}", k69::user(), k69::JUMP);
    let config = k69::config(k69::ssh(key("kerno_k69"), Some(jump)));
    let tunnel = open(&fixture.env, &config, None, None)
        .await
        .expect("tunnel");
    // The first stream is the one the start opened, the second a new ssh.
    probe(&tunnel, &config).await;
    probe(&tunnel, &config).await;
    let held = tunnel.open_stream(&config.host, config.port).await.unwrap();
    let running = runs_only(&marker(&tunnel), &k69::system32("ssh.exe"));
    assert!(running >= 2, "the stream's and the jump host's: {running}");
    drop(held);
    // A driver that dials an address gets a single-use loopback port.
    let address = tunnel
        .forward_once(&config.host, config.port)
        .await
        .unwrap();
    let mut local = tokio::net::TcpStream::connect(address).await.unwrap();
    local.write_all(&SSL_REQUEST).await.unwrap();
    let mut answer = [0u8; 1];
    local.read_exact(&mut answer).await.unwrap();
    assert_eq!(answer[0], b'N');
    eprintln!(
        "K69 PROVED: a stream through {} reached PostgreSQL behind a jump host; {running} \
         ssh.exe, each that file, while PATH names {} first",
        fixture.env.ssh.display(),
        first.display()
    );
}

/// The first `ssh.exe` a bash step's `PATH` names (Git's, on the runner).
fn which_ssh() -> Option<PathBuf> {
    std::env::var_os("PATH").and_then(|path| {
        std::env::split_paths(&path)
            .map(|dir| dir.join("ssh.exe"))
            .find(|candidate| candidate.is_file())
    })
}

/// The profile's `.ssh\config` (written by the workflow): its alias is
/// listed, and a tunnel through the alias alone works.
#[tokio::test]
async fn a_host_alias_of_the_profile_config() {
    if !k69::runner() {
        return;
    }
    let home = kerno_core::home::dir().expect("USERPROFILE");
    let env = OpenSshEnv::detect(
        std::env::var_os("PATH").as_deref(),
        Some(&home),
        None,
        askpass(),
    )
    .expect("ssh.exe found");
    let file = env.aliases_from.clone().unwrap();
    assert_eq!(file, home.join(".ssh").join("config"));
    let aliases = host_aliases(&file, Some(&home));
    assert!(aliases.contains(&"k69-alias".to_string()), "{aliases:?}");
    let mut ssh = k69::ssh(SshAuth::Agent { socket: None }, None);
    ssh.host = "k69-alias".into();
    ssh.port = 0;
    ssh.user = String::new();
    let config = k69::config(ssh);
    let tunnel = open(&env, &config, None, None).await.expect("alias tunnel");
    probe(&tunnel, &config).await;
    eprintln!(
        "K69 PROVED: the alias k69-alias of {} ({aliases:?})",
        file.display()
    );
}

/// The key's passphrase: asked through Kerno's askpass pipe when none is
/// stored, a wrong one said in plain words, the right one given to `ssh`.
#[tokio::test]
async fn a_passphrase_is_answered_through_the_askpass_pipe() {
    if !k69::runner() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let fixture = k69::openssh(dir.path(), askpass(), true);
    let config = k69::config(k69::ssh(key("kerno_k69_enc"), None));

    // No passphrase stored: ssh asked Kerno, Kerno can ask the user.
    let error = open(&fixture.env, &config, None, None).await.unwrap_err();
    match &error {
        SshError::Passphrase { path, reason } => {
            assert_eq!(*reason, "a passphrase is required");
            assert!(path.ends_with("kerno_k69_enc"), "{path}");
        }
        other => panic!("expected a passphrase question, got {other:?}"),
    }
    let explained = explain(&Error::from(error).to_string(), &config);
    eprintln!("K69 PROVED: no passphrase: {}", explained.headline);
    assert!(
        explained.headline.contains("needs a passphrase"),
        "{explained:?}"
    );

    // A wrong one.
    let error = open(&fixture.env, &config, Some("not the passphrase"), None)
        .await
        .unwrap_err();
    assert!(
        matches!(&error, SshError::Passphrase { reason, .. } if *reason == "the passphrase is wrong"),
        "{error:?}"
    );
    let text = Error::from(error).to_string();
    eprintln!("K69 PROVED: a wrong passphrase: {text}");
    assert!(text.contains("the passphrase is wrong"), "{text}");

    // The right one crosses the pipe to ssh.exe.
    let tunnel = open(&fixture.env, &config, Some(PASSPHRASE), None)
        .await
        .expect("tunnel with the passphrase");
    probe(&tunnel, &config).await;
    // A second stream: a new ssh.exe asks again, and is answered again.
    probe(&tunnel, &config).await;
    // Two at once: each ssh.exe is answered on its own (R9).
    let (first, second) = tokio::join!(
        tunnel.open_stream(&config.host, config.port),
        tunnel.open_stream(&config.host, config.port)
    );
    let (first, second) = (first.expect("first"), second.expect("second"));
    // No ssh.exe's command line holds the passphrase (its environment is
    // `SSH_ASKPASS`, `SSH_ASKPASS_REQUIRE`, `KERNO_ASKPASS` and
    // `KERNO_ASKPASS_SPAWN`, by construction).
    let lines = ssh_processes();
    assert!(lines.len() >= 2, "{lines:#?}");
    assert!(
        lines.iter().all(|line| !line.contains(PASSPHRASE)),
        "{lines:#?}"
    );
    drop((first, second));
    eprintln!(
        "K69 PROVED: two ssh.exe at once each answered; no command line holds the passphrase"
    );
    eprintln!("K69 PROVED: the passphrase answered through the askpass pipe, twice");
    drop(tunnel);

    // The run's askpass program in a folder whose name has a space (an
    // installed Kerno lives under "Program Files").
    let spaced = dir.path().join("k69 askpass");
    std::fs::create_dir(&spaced).unwrap();
    let program = spaced.join(askpass().file_name().unwrap());
    std::fs::copy(askpass(), &program).unwrap();
    let env = OpenSshEnv {
        askpass: program.clone(),
        ..fixture.env.clone()
    };
    let tunnel = open(&env, &config, Some(PASSPHRASE), None)
        .await
        .expect("tunnel with the askpass in a folder with a space");
    probe(&tunnel, &config).await;
    eprintln!(
        "K69 PROVED: the passphrase answered by {}",
        program.display()
    );
}

#[tokio::test]
async fn an_unknown_host_key_is_shown_then_trusted_then_remembered() {
    if !k69::runner() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let fixture = k69::openssh(dir.path(), askpass(), false);
    let config = k69::config(k69::ssh(key("kerno_k69"), None));
    let known = || std::fs::read_to_string(&fixture.known_hosts).unwrap_or_default();

    let error = open(&fixture.env, &config, None, None).await.unwrap_err();
    let fingerprint = match &error {
        SshError::UnknownHostKey {
            host,
            port,
            fingerprint,
            ..
        } => {
            assert_eq!((host.as_str(), *port), ("127.0.0.1", k69::SSH_PORT));
            assert!(fingerprint.starts_with("SHA256:"), "{fingerprint}");
            fingerprint.clone()
        }
        other => panic!("expected an unknown host key, got {other:?}"),
    };
    assert_eq!(known(), "", "nothing recorded before consent");
    let explained = explain(&Error::from(error).to_string(), &config);
    assert_eq!(explained.kind, FailureKind::SshHostKeyUnknown);
    eprintln!("K69 PROVED: shown: {} {fingerprint}", explained.headline);

    // Another fingerprint is not consent.
    let wrong = open(
        &fixture.env,
        &config,
        None,
        Some("SHA256:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"),
    )
    .await;
    assert!(
        matches!(wrong, Err(SshError::UnknownHostKey { .. })),
        "{wrong:?}"
    );
    assert_eq!(known(), "");

    let tunnel = open(&fixture.env, &config, None, Some(&fingerprint))
        .await
        .expect("trusted");
    probe(&tunnel, &config).await;
    assert!(known().contains("ssh-"), "recorded: {}", known());
    eprintln!("K69 PROVED: trusted, and ssh.exe recorded it");
    drop(tunnel);
    let tunnel = open(&fixture.env, &config, None, None)
        .await
        .expect("remembered");
    probe(&tunnel, &config).await;
    eprintln!("K69 PROVED: remembered");
}

/// The jump host field (`user@alias`), through the same server on its
/// second port.
#[tokio::test]
async fn the_jump_host_field_goes_through_the_jump_host() {
    if !k69::runner() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let fixture = k69::openssh(dir.path(), askpass(), true);
    let jump = format!("{}@{}", k69::user(), k69::JUMP);
    let config = k69::config(k69::ssh(key("kerno_k69"), Some(jump.clone())));
    let tunnel = open(&fixture.env, &config, None, None)
        .await
        .expect("tunnel through the jump host");
    probe(&tunnel, &config).await;
    probe(&tunnel, &config).await;
    eprintln!("K69 PROVED: through the jump host {jump}");
}

/// A chain of two jump hosts, through an `ssh.exe` copied into a folder
/// whose path has a space (as Win32-OpenSSH's installer's
/// `C:\Program Files\OpenSSH`), named in the settings Kerno runs, the way
/// `find_ssh`'s result is: OpenSSH starts each hop's `ssh.exe` itself
/// (`-J`), and every one is that copy reading the tunnel's own file (S1).
#[tokio::test]
async fn a_chain_of_jump_hosts_runs_the_chosen_ssh_exe_from_a_folder_with_a_space() {
    if !k69::runner() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let openssh = dir.path().join("Program Files test").join("OpenSSH");
    std::fs::create_dir_all(&openssh).unwrap();
    let system = k69::system32("ssh.exe");
    for entry in std::fs::read_dir(system.parent().unwrap()).unwrap() {
        let entry = entry.unwrap();
        if entry.file_type().unwrap().is_file() {
            std::fs::copy(entry.path(), openssh.join(entry.file_name())).unwrap();
        }
    }
    let mut fixture = k69::openssh(dir.path(), askpass(), true);
    fixture.env.ssh = openssh.join("ssh.exe");
    let jump = format!("{},{}", k69::JUMP, k69::HOP2);
    let config = k69::config(k69::ssh(key("kerno_k69"), Some(jump.clone())));
    let tunnel = open(&fixture.env, &config, None, None)
        .await
        .expect("a tunnel through two jump hosts");
    probe(&tunnel, &config).await;
    let held = tunnel.open_stream(&config.host, config.port).await.unwrap();
    let marker = marker(&tunnel);
    let running = runs_only(&marker, &fixture.env.ssh);
    assert!(running >= 3, "the stream's and its two hops': {running}");
    drop(held);
    drop(tunnel);
    wait_until_gone(&marker).await;
    eprintln!(
        "K69 PROVED: through {jump}, {running} ssh.exe, each {}",
        fixture.env.ssh.display()
    );
}

/// A PEM key (Kerno cannot try its passphrase) used by the jump host and by
/// the target: each hop's `ssh.exe` asks for it under the stream's number
/// and is answered once; a wrong one is still said as wrong (S5). The jump
/// host's `IdentityFile` is written with `/` where `-i` has `\` (S6).
#[tokio::test]
async fn a_pem_key_used_by_the_jump_host_and_the_target() {
    if !k69::runner() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let fixture = k69::openssh(dir.path(), askpass(), true);
    let jump = format!("{}@{}", k69::user(), k69::JUMP_PEM);
    let config = k69::config(k69::ssh(key("kerno_k69_pem"), Some(jump.clone())));
    let error = open(&fixture.env, &config, Some("not the passphrase"), None)
        .await
        .unwrap_err();
    assert!(
        matches!(&error, SshError::Passphrase { reason, .. } if *reason == "the passphrase is wrong"),
        "{error:?}"
    );
    let tunnel = open(&fixture.env, &config, Some(PASSPHRASE), None)
        .await
        .expect("the PEM key on both hops");
    probe(&tunnel, &config).await;
    probe(&tunnel, &config).await;
    eprintln!("K69 PROVED: a PEM key's passphrase answered on both hops through {jump}");
}

/// A key written `~/.ssh/…`: expanded to `C:\Users\…/.ssh/…`, two
/// separators in one path, and still known as the key `ssh.exe` asks for
/// (S6).
#[tokio::test]
async fn a_key_written_with_slashes_under_home() {
    if !k69::runner() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let fixture = k69::openssh(dir.path(), askpass(), true);
    let config = k69::config(k69::ssh(
        SshAuth::KeyFile {
            path: "~/.ssh/kerno_k69_enc".into(),
        },
        None,
    ));
    let error = open(&fixture.env, &config, None, None).await.unwrap_err();
    assert!(
        matches!(&error, SshError::Passphrase { reason, path } if *reason == "a passphrase is required" && path.ends_with("kerno_k69_enc")),
        "{error:?}"
    );
    let tunnel = open(&fixture.env, &config, Some(PASSPHRASE), None)
        .await
        .expect("the key written ~/.ssh/kerno_k69_enc");
    probe(&tunnel, &config).await;
    eprintln!("K69 PROVED: ~/.ssh/kerno_k69_enc asked for and answered: {error}");
}

/// A profile whose name has an accent ("Élodie"): the tunnel's folder, its
/// `-F` file's includes, and the askpass program (`SSH_ASKPASS`,
/// `KERNO_ASKPASS`) all under it (S6).
#[tokio::test]
async fn a_profile_folder_with_an_accent() {
    if !k69::runner() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let profile = dir.path().join("Élodie");
    std::fs::create_dir_all(&profile).unwrap();
    let mut fixture = k69::openssh(&profile, askpass(), true);
    fixture.env.tunnels = Some(profile.join("AppData").join("Local").join("ssh"));
    let program = profile.join(askpass().file_name().unwrap());
    std::fs::copy(askpass(), &program).unwrap();
    fixture.env.askpass = program.clone();
    let config = k69::config(k69::ssh(key("kerno_k69_enc"), None));
    let tunnel = open(&fixture.env, &config, Some(PASSPHRASE), None)
        .await
        .expect("a tunnel from a folder with an accent");
    probe(&tunnel, &config).await;
    assert!(tunnel.openssh_dir().unwrap().starts_with(&profile));
    eprintln!(
        "K69 PROVED: the passphrase answered by {}, the tunnel's folder under {}",
        program.display(),
        profile.display()
    );
}

/// Every `ssh.exe` of a tunnel (each stream's and its jump host's) and its
/// askpass pipe are gone once the streams and the tunnel are dropped.
#[tokio::test]
async fn nothing_is_left_after_the_tunnel_is_dropped() {
    if !k69::runner() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let fixture = k69::openssh(dir.path(), askpass(), true);
    let jump = format!("{}@{}", k69::user(), k69::JUMP);
    let config = k69::config(k69::ssh(key("kerno_k69"), Some(jump)));
    let tunnel = open(&fixture.env, &config, None, None)
        .await
        .expect("tunnel");
    let marker = marker(&tunnel);
    let pipe = std::fs::read_to_string(tunnel.openssh_dir().unwrap().join("ask"))
        .unwrap()
        .lines()
        .next()
        .map(|id| format!(r"\\.\pipe\kerno-{id}"))
        .unwrap();
    let first = tunnel.open_stream(&config.host, config.port).await.unwrap();
    let second = tunnel.open_stream(&config.host, config.port).await.unwrap();
    let all = ssh_processes();
    let running: Vec<&String> = all.iter().filter(|line| line.contains(&marker)).collect();
    eprintln!("ssh.exe of the tunnel: {running:#?}");
    assert!(
        running.len() >= 2,
        "two streams, two ssh.exe at least: {all:#?}"
    );
    // Nothing listens: no ssh.exe of the tunnel has a -L.
    assert!(
        running.iter().all(|line| !line.contains(" -L ")),
        "{running:?}"
    );

    drop(first);
    drop(second);
    drop(tunnel);
    wait_until_gone(&marker).await;
    // A jump host's ssh.exe may not name the folder: none at all is left
    // (every command line holds "").
    wait_until_gone("").await;
    assert!(!PathBuf::from(&marker).exists(), "{marker} is left");
    tokio::time::sleep(Duration::from_millis(500)).await;
    let opened = tokio::net::windows::named_pipe::ClientOptions::new().open(&pipe);
    assert_eq!(
        opened.err().map(|error| error.kind()),
        Some(std::io::ErrorKind::NotFound),
        "the askpass pipe {pipe} is gone"
    );
    eprintln!("K69 PROVED: no ssh.exe, no folder, no pipe left after a close");
}

/// A tunnel cancelled while `ssh.exe` is still logging in (a server that
/// never says hello), alone and behind a jump host: nothing of it is left,
/// the jump host's `ssh.exe` included (Windows does not stop a child with
/// its parent). Mutation seen: `taskkill /T` left out of `Tree`'s drop.
#[tokio::test]
async fn nothing_is_left_after_a_cancelled_start() {
    if !k69::runner() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let fixture = k69::openssh(dir.path(), askpass(), true);
    // Takes the connection and says nothing: ssh.exe waits for its banner.
    let silent = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = silent.local_addr().unwrap().port();
    let held = tokio::spawn(async move {
        let mut held = Vec::new();
        while let Ok((stream, _)) = silent.accept().await {
            held.push(stream);
        }
    });
    let folders = || -> Vec<PathBuf> {
        std::fs::read_dir(k69::tunnels())
            .into_iter()
            .flatten()
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| {
                path.file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.starts_with("kerno-ssh-"))
            })
            .collect()
    };
    for jump in [None, Some(format!("{}@{}", k69::user(), k69::JUMP))] {
        let mut ssh = k69::ssh(key("kerno_k69"), jump.clone());
        ssh.port = port;
        let config = k69::config(ssh);
        let before = folders();
        let env = fixture.env.clone();
        let opening = tokio::spawn(async move { open(&env, &config, None, None).await.map(drop) });
        // The target's ssh.exe, and the jump host's, which names the target.
        let mut markers = vec![format!("-p {port} ")];
        if jump.is_some() {
            markers.push(format!("]:{port}"));
        }
        let deadline = std::time::Instant::now() + Duration::from_secs(20);
        loop {
            let running = ssh_processes();
            if markers
                .iter()
                .all(|marker| running.iter().any(|line| line.contains(marker)))
            {
                eprintln!("ssh.exe while logging in ({jump:?}): {running:#?}");
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "ssh.exe never started: {running:#?}"
            );
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
        assert!(!opening.is_finished(), "still logging in");
        opening.abort();
        assert!(opening.await.unwrap_err().is_cancelled());
        for marker in &markers {
            wait_until_gone(marker).await;
        }
        let after = folders();
        let left: Vec<&PathBuf> = after.iter().filter(|path| !before.contains(path)).collect();
        assert!(left.is_empty(), "tunnel folders left: {left:?}");
        eprintln!("K69 PROVED: no ssh.exe and no folder left after a cancel (jump: {jump:?})");
    }
    held.abort();
}

/// A key file on another computer is refused before ssh.exe starts.
#[tokio::test]
async fn a_key_on_a_share_is_refused_before_ssh_starts() {
    if !k69::runner() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let fixture = k69::openssh(dir.path(), askpass(), true);
    let config = k69::config(k69::ssh(
        SshAuth::KeyFile {
            path: r"\\k69-nowhere\share\id".into(),
        },
        None,
    ));
    let before = ssh_processes().len();
    let error = open(&fixture.env, &config, None, None).await.unwrap_err();
    assert!(matches!(error, SshError::Key { .. }), "{error:?}");
    assert_eq!(ssh_processes().len(), before);
    assert_eq!(find_ssh(None), Some(k69::system32("ssh.exe")));
}

/// A key Kerno cannot try itself (PEM): its passphrase is given once per
/// ssh.exe, and a wrong one is said as wrong, not as a refused login (R8).
#[tokio::test]
async fn a_pem_key_s_wrong_passphrase_is_said() {
    if !k69::runner() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let fixture = k69::openssh(dir.path(), askpass(), true);
    let config = k69::config(k69::ssh(key("kerno_k69_pem"), None));
    let error = open(&fixture.env, &config, Some("not the passphrase"), None)
        .await
        .unwrap_err();
    assert!(
        matches!(&error, SshError::Passphrase { reason, .. } if *reason == "the passphrase is wrong"),
        "{error:?}"
    );
    eprintln!("K69 PROVED: a PEM key's wrong passphrase: {error}");
    let tunnel = open(&fixture.env, &config, Some(PASSPHRASE), None)
        .await
        .expect("the PEM key with its passphrase");
    probe(&tunnel, &config).await;
    eprintln!("K69 PROVED: a PEM key opened through the askpass pipe");
}

/// A key named `~\…` is the profile's: what is checked, tried and given to
/// ssh.exe is the expanded path (R7).
#[tokio::test]
async fn a_home_key_path_is_expanded() {
    if !k69::runner() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let fixture = k69::openssh(dir.path(), askpass(), true);
    let config = k69::config(k69::ssh(
        SshAuth::KeyFile {
            path: r"~\.ssh\kerno_k69_enc".into(),
        },
        None,
    ));
    let error = open(&fixture.env, &config, Some("not the passphrase"), None)
        .await
        .unwrap_err();
    assert!(
        matches!(&error, SshError::Passphrase { reason, .. } if *reason == "the passphrase is wrong"),
        "{error:?}"
    );
    let tunnel = open(&fixture.env, &config, Some(PASSPHRASE), None)
        .await
        .expect("the key under ~");
    probe(&tunnel, &config).await;
    let expanded = k69::key("kerno_k69_enc").display().to_string();
    eprintln!("K69 PROVED: ~\\.ssh\\kerno_k69_enc opened as {expanded}");
}

/// A profile whose name holds a space ("C:\Users\Jean Dupont"): the
/// tunnels' folder and the SSH files under such a folder, behind a jump
/// host, whose command line OpenSSH writes with `-F` unquoted (R11).
#[tokio::test]
async fn a_folder_with_a_space_behind_a_jump_host() {
    if !k69::runner() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let spaced = dir.path().join("Jean Dupont");
    std::fs::create_dir_all(&spaced).unwrap();
    let mut fixture = k69::openssh(&spaced, askpass(), true);
    fixture.env.tunnels = Some(spaced.join("AppData Local").join("ssh"));
    let jump = format!("{}@{}", k69::user(), k69::JUMP);
    let config = k69::config(k69::ssh(key("kerno_k69"), Some(jump)));
    let tunnel = open(&fixture.env, &config, None, None)
        .await
        .expect("a tunnel from a folder with a space, behind a jump host");
    probe(&tunnel, &config).await;
    let stream = tunnel.open_stream(&config.host, config.port).await.unwrap();
    let marker = marker(&tunnel);
    let lines: Vec<String> = ssh_processes()
        .into_iter()
        .filter(|line| line.contains(&marker))
        .collect();
    eprintln!("ssh.exe from a folder with a space: {lines:#?}");
    assert!(
        lines.len() >= 2,
        "the target's and the jump host's: {lines:#?}"
    );
    assert!(tunnel.openssh_dir().unwrap().starts_with(&spaced));
    drop(stream);
    drop(tunnel);
    wait_until_gone(&marker).await;
    eprintln!(
        "K69 PROVED: a tunnel folder under {} behind a jump host",
        spaced.display()
    );
}

/// What the app does when it quits: every ssh.exe of every tunnel, a jump
/// host's included, stopped and waited for (R10).
#[tokio::test]
async fn stop_all_leaves_no_ssh_exe() {
    if !k69::runner() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let fixture = k69::openssh(dir.path(), askpass(), true);
    let jump = format!("{}@{}", k69::user(), k69::JUMP);
    let config = k69::config(k69::ssh(key("kerno_k69"), Some(jump)));
    let tunnel = open(&fixture.env, &config, None, None)
        .await
        .expect("tunnel");
    let streams = (
        tunnel.open_stream(&config.host, config.port).await.unwrap(),
        tunnel.open_stream(&config.host, config.port).await.unwrap(),
    );
    let marker = marker(&tunnel);
    let running = ssh_processes()
        .into_iter()
        .filter(|line| line.contains(&marker))
        .count();
    assert!(running >= 4, "two streams behind a jump host: {running}");
    kerno_core::openssh::stop_all();
    // Waited for: nothing of it is left at once.
    let left: Vec<String> = ssh_processes()
        .into_iter()
        .filter(|line| line.contains(&marker))
        .collect();
    assert!(left.is_empty(), "left after stop_all: {left:#?}");
    drop(streams);
    drop(tunnel);
    eprintln!("K69 PROVED: stop_all stopped {running} ssh.exe at once");
}
