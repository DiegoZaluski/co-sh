//! Manager behavior tests: discovery, lazy spawning, dedup, backoff,
//! eviction and teardown — all over in-memory fake servers.

use std::{
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use tokio::sync::{mpsc, watch};

use crate::lsp::manager::{ClientFactory, ClientKey, ClientLifecycle, Manager, ManagerConfig};
use crate::lsp::test::{auto_serve, spawn_fake_server};
use crate::lsp::{LanguageServer, LspError, ServerSpec};

// Fixtures

fn spec(
    name: &'static str,
    command: &'static str,
    extensions: &'static [&'static str],
    markers: &'static [&'static str],
) -> ServerSpec {
    ServerSpec {
        name,
        command,
        args: &[],
        extensions,
        filenames: &[],
        root_markers: markers,
    }
}

/// root/
///  ├── Cargo.toml
///  └── pkg/
///   ├── Cargo.toml
///   ├── lib.rs
fn nested_project_tree() -> (tempfile::TempDir, PathBuf, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();
    let pkg = root.join("pkg");
    std::fs::create_dir_all(&pkg).unwrap();
    std::fs::write(root.join("Cargo.toml"), "[workspace]\n").unwrap();
    std::fs::write(pkg.join("Cargo.toml"), "[package]\n").unwrap();
    std::fs::write(pkg.join("lib.rs"), "").unwrap();
    (dir, root, pkg)
}

/// Factory standing up a fully scripted fake server per spawn.
///
/// Every kill switch lands in `kills`; flipping one drops that server's pipe
/// halves, which the client observes as process death.
fn working_factory(
    counter: Arc<AtomicUsize>,
    kills: Arc<Mutex<Vec<watch::Sender<bool>>>>,
) -> ClientFactory {
    Arc::new(move |config| {
        counter.fetch_add(1, Ordering::SeqCst);
        let kills = Arc::clone(&kills);
        Box::pin(async move {
            let (server, client_stream) = spawn_fake_server(32 * 1024);
            let (dead_stderr, _dead_peer) = tokio::io::duplex(1);

            let (kill_tx, kill_rx) = watch::channel(false);
            kills.lock().expect("kills lock").push(kill_tx);

            // Race the responder against the kill switch. The loser is
            // dropped by select!, and the kill branch dropping `server`
            // closes the pipe the client observes as process death.
            async fn wait_for_kill(mut rx: watch::Receiver<bool>) {
                loop {
                    if *rx.borrow_and_update() {
                        return;
                    }
                    if rx.changed().await.is_err() {
                        return;
                    }
                }
            }
            tokio::spawn(async move {
                tokio::select! {
                    biased;
                    _ = wait_for_kill(kill_rx) => {},
                    _ = auto_serve(server) => {},
                }
            });

            let (read_half, write_half) = tokio::io::split(client_stream);
            let client =
                LanguageServer::from_streams(config, read_half, write_half, Some(dead_stderr));
            client.initialize(Duration::from_secs(5)).await?;
            Ok(client)
        })
    })
}

fn never_factory() -> ClientFactory {
    Arc::new(|_config| Box::pin(async { Err(LspError::NotRunning) }))
}

fn failing_factory(counter: Arc<AtomicUsize>, detail: &'static str) -> ClientFactory {
    Arc::new(move |_config| {
        counter.fetch_add(1, Ordering::SeqCst);
        let detail = detail.to_owned();
        Box::pin(async move {
            Err(LspError::Spawn {
                server: "fake".into(),
                detail,
            })
        })
    })
}

// Discovery

#[tokio::test]
async fn root_resolution_walks_up_to_nearest_marker() {
    let (_dir, root, pkg) = nested_project_tree();
    let manager = Manager::build(
        ManagerConfig::new(root.clone()),
        vec![
            spec("rust-nested", "unused", &[".rs"], &["Cargo.toml"]),
            spec("rust-markerless", "unused", &[".rs"], &[]),
        ],
        never_factory(),
    );

    let matches = manager.matches_for_file(&pkg.join("lib.rs"));

    let nested = matches
        .iter()
        .find(|(_, name)| *name == "rust-nested")
        .unwrap();
    // Manager roots go through `canonical_root`, which strips the Windows
    // verbatim prefix `canonicalize` adds — expect the stripped form.
    assert_eq!(
        nested.0.root,
        super::super::manager::canonical_root(&pkg),
        "nearest marker wins over workspace root"
    );

    let markerless = matches
        .iter()
        .find(|(_, name)| *name == "rust-markerless")
        .unwrap();
    assert_eq!(
        markerless.0.root,
        super::super::manager::canonical_root(&root),
        "marker-less specs fall back to workspace root"
    );
}

#[tokio::test]
async fn specs_without_marker_ancestry_are_skipped() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();
    let deep = root.join("vendor").join("thing");
    std::fs::create_dir_all(&deep).unwrap();
    std::fs::write(deep.join("main.py"), "").unwrap();

    let manager = Manager::build(
        ManagerConfig::new(root),
        vec![spec(
            "pyright",
            "pyright-langserver",
            &[".py"],
            &["pyproject.toml"],
        )],
        never_factory(),
    );

    assert!(
        manager.matches_for_file(&deep.join("main.py")).is_empty(),
        "no pyproject.toml in ancestry → spec does not apply"
    );
}

#[tokio::test]
async fn root_discovery_uses_distinctive_markers_and_ignores_generic() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();
    std::fs::write(root.join("Cargo.toml"), "[package]\n").unwrap();
    std::fs::write(root.join(".git"), "").unwrap();

    let manager = Manager::build(
        ManagerConfig::new(root.clone()),
        vec![
            // Rust: Cargo.toml is a distinctive marker → discovered.
            spec("rust-analyzer", "unused", &[".rs"], &["Cargo.toml", ".git"]),
            // Bare .git alone must NOT pull in a matching server.
            spec("yaml-ls", "unused", &[".yaml"], &[".git"]),
            spec("clangd", "unused", &[".c"], &["Makefile", ".git"]),
        ],
        never_factory(),
    );

    let keys = manager.matches_for_root(&root);
    let found: Vec<String> = keys.iter().map(|k| k.server.clone()).collect();
    assert_eq!(
        found,
        vec!["rust-analyzer".to_string()],
        "Cargo.toml wins, .git alone is generic"
    );
}

#[tokio::test]
async fn ensure_for_root_spawns_discoverable_servers() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();
    std::fs::write(root.join("go.mod"), "module x\n").unwrap();

    let counter = Arc::new(AtomicUsize::new(0));
    let mut config = ManagerConfig::new(root.clone());
    config.resolves_binaries = false;
    let manager = Manager::build(
        config,
        vec![
            spec("gopls", "unused", &[".go"], &["go.mod"]),
            spec("yaml-ls", "unused", &[".yaml"], &[".git"]),
        ],
        working_factory(counter.clone(), Arc::new(Mutex::new(Vec::new()))),
    );

    let handles = manager.ensure_for_root(&root).await.unwrap();
    assert_eq!(handles.len(), 1, "only gopls is discovered at the root");
    assert_eq!(
        counter.load(Ordering::SeqCst),
        1,
        "one spawn, no double process"
    );

    // Idempotent: re-ensuring reuses the running client.
    let again = manager.ensure_for_root(&root).await.unwrap();
    assert_eq!(again.len(), 1);
    assert_eq!(counter.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn unsupported_extension_matches_nothing_and_is_not_an_error() {
    let dir = tempfile::tempdir().unwrap();
    let manager = Manager::build(
        ManagerConfig::new(dir.path().to_path_buf()),
        vec![],
        never_factory(),
    );
    let file = dir.path().join("image.png");
    std::fs::write(&file, b"png").unwrap();

    assert!(manager.matches_for_file(&file).is_empty());

    let handles = manager.ensure_for_file(&file).await.unwrap();
    assert!(handles.is_empty());
}

#[tokio::test]
async fn extensionless_files_match_by_name() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();
    let file = root.join("Dockerfile");
    std::fs::write(&file, "FROM scratch\n").unwrap();

    let manager = Manager::build(
        ManagerConfig::new(root.clone()),
        vec![ServerSpec {
            name: "docker-ls",
            command: "unused",
            args: &[],
            extensions: &[".dockerfile"],
            filenames: &["dockerfile"],
            root_markers: &["Dockerfile"],
        }],
        never_factory(),
    );

    let matches = manager.matches_for_file(&file);
    assert_eq!(
        matches[0].0.root,
        super::super::manager::canonical_root(&root)
    );
    assert_eq!(matches.len(), 1, "bare `Dockerfile` claims the spec");
}

/// The same project reached through a symlink must resolve to the same
/// `ClientKey` — otherwise two processes of one server run against one
/// project.
#[cfg(unix)]
#[tokio::test]
async fn symlinked_project_resolves_to_canonical_key() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();
    let project = root.join("project");
    std::fs::create_dir_all(&project).unwrap();
    std::fs::write(project.join("Cargo.toml"), "[package]\n").unwrap();
    std::fs::write(project.join("lib.rs"), "").unwrap();
    let link = root.join("link");
    std::os::unix::fs::symlink(&project, &link).unwrap();

    let manager = Manager::build(
        ManagerConfig::new(root.clone()),
        vec![spec("rust", "unused", &[".rs"], &["Cargo.toml"])],
        never_factory(),
    );

    let direct = manager.matches_for_file(&project.join("lib.rs"));
    let via_link = manager.matches_for_file(&link.join("lib.rs"));

    assert_eq!(direct.len(), 1);
    assert_eq!(via_link.len(), 1, "symlinked file stays inside workspace");
    assert_eq!(
        direct[0].0, via_link[0].0,
        "symlink and real path must share one ClientKey"
    );
    assert_eq!(direct[0].0.root, project.canonicalize().unwrap());
}

/// Two servers of the same language with different roots coexist (the
/// keying fix for crush #1751).
#[tokio::test]
async fn same_language_different_roots_are_distinct_clients() {
    let (_dir, root, pkg) = nested_project_tree();
    let mut config = ManagerConfig::new(root);
    config.resolves_binaries = false;
    let manager = Manager::build(
        config,
        vec![spec("rust", "unused", &[".rs"], &["Cargo.toml"])],
        working_factory(
            Arc::new(AtomicUsize::new(0)),
            Arc::new(Mutex::new(Vec::new())),
        ),
    );

    let root_file = _dir.path().join("main.rs");
    std::fs::write(&root_file, "").unwrap();

    let at_root = manager.ensure_for_file(&root_file).await.unwrap();
    let at_pkg = manager.ensure_for_file(&pkg.join("lib.rs")).await.unwrap();

    assert_eq!(at_root.len(), 1);
    assert_eq!(at_pkg.len(), 1);
    assert!(
        !Arc::ptr_eq(&at_root[0], &at_pkg[0]),
        "distinct roots must yield distinct clients"
    );

    let keys: Vec<ClientKey> = manager.states().into_iter().map(|(key, _)| key).collect();
    assert_eq!(keys.len(), 2);
}

// Spawning

fn single_spec_manager(dir: &Path, factory: ClientFactory, backoff: Duration) -> Manager {
    single_spec_manager_with(dir, factory, backoff, false)
}

fn single_spec_manager_with(
    dir: &Path,
    factory: ClientFactory,
    backoff: Duration,
    resolves_binaries: bool,
) -> Manager {
    let mut config = ManagerConfig::new(dir.to_path_buf());
    config.spawn_backoff = backoff;
    config.resolves_binaries = resolves_binaries;
    Manager::build(
        config,
        vec![spec("fake-a", "unused", &[".a"], &[])],
        factory,
    )
}

fn touch(dir: &Path, name: &str) -> PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, "").unwrap();
    path
}

#[tokio::test]
async fn lazy_spawn_reuses_running_client() {
    let dir = tempfile::tempdir().unwrap();
    let counter = Arc::new(AtomicUsize::new(0));
    let manager = single_spec_manager(
        dir.path(),
        working_factory(Arc::clone(&counter), Arc::new(Mutex::new(Vec::new()))),
        Duration::ZERO,
    );

    let file = touch(dir.path(), "one.a");

    let first = manager.ensure_for_file(&file).await.unwrap();
    assert_eq!(first.len(), 1);
    let second = manager.ensure_for_file(&file).await.unwrap();
    assert_eq!(second.len(), 1);

    assert_eq!(counter.load(Ordering::SeqCst), 1, "second touch must reuse");
    assert!(Arc::ptr_eq(&first[0], &second[0]));
    assert_eq!(manager.clients_for_file(&file).len(), 1);
    manager.stop_all().await;
}

/// Concurrent first-touches share ONE in-flight spawn (opencode's dedup).
/// Multi-thread flavor: the decision/publish window has no await inside, so
/// only real thread preemption can interleave it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_first_touch_spawns_once() {
    let dir = tempfile::tempdir().unwrap();
    let counter = Arc::new(AtomicUsize::new(0));
    let manager = Arc::new(single_spec_manager(
        dir.path(),
        working_factory(Arc::clone(&counter), Arc::new(Mutex::new(Vec::new()))),
        Duration::ZERO,
    ));

    let file = touch(dir.path(), "race.a");
    let tasks: Vec<_> = (0..5)
        .map(|_| {
            let manager = Arc::clone(&manager);
            let file = file.clone();
            tokio::spawn(async move { manager.ensure_for_file(&file).await.unwrap().len() })
        })
        .collect();

    for task in tasks {
        assert_eq!(task.await.unwrap(), 1);
    }
    assert_eq!(counter.load(Ordering::SeqCst), 1, "single flight");
}

#[tokio::test]
async fn missing_binary_is_soft_skipped_when_others_answer() {
    let dir = tempfile::tempdir().unwrap();

    // The only matching spec has no real binary on PATH: the manager must
    // surface Unavailable (soft), not a hard Spawn error, and never call the
    // factory.
    let counter = Arc::new(AtomicUsize::new(0));
    let mut config = ManagerConfig::new(dir.path().to_path_buf());
    config.resolves_binaries = true;
    let manager = Manager::build(
        config,
        vec![spec(
            "ghost-only",
            "definitely-not-a-real-binary-xyz",
            &[".x"],
            &[],
        )],
        working_factory(Arc::clone(&counter), Arc::new(Mutex::new(Vec::new()))),
    );
    let file = touch(dir.path(), "solo.x");

    let err = manager.ensure_for_file(&file).await.unwrap_err();
    assert!(matches!(err, LspError::Unavailable(_)), "got {err:?}");
    assert_eq!(
        counter.load(Ordering::SeqCst),
        0,
        "binary check precedes the factory"
    );
}

/// When another server DID answer, a sibling's missing binary stays silent.
#[tokio::test]
async fn missing_binary_does_not_mask_working_server() {
    let dir = tempfile::tempdir().unwrap();
    let counter = Arc::new(AtomicUsize::new(0));
    let kills: Arc<Mutex<Vec<watch::Sender<bool>>>> = Arc::new(Mutex::new(Vec::new()));
    let mut config = ManagerConfig::new(dir.path().to_path_buf());
    // The ghost exercises the PATH check (true); the real server then answers
    // through the factory, so resolution must be off for it (false) — split
    // into two managers instead: one per behavior.
    config.resolves_binaries = true;
    let ghost_manager = Manager::build(
        config,
        vec![spec(
            "ghost",
            "definitely-not-a-real-binary-xyz",
            &[".x"],
            &[],
        )],
        never_factory(),
    );
    let mut plain = ManagerConfig::new(dir.path().to_path_buf());
    plain.resolves_binaries = false;
    let real_manager = Manager::build(
        plain,
        vec![spec("real", "unused", &[".x"], &[])],
        working_factory(Arc::clone(&counter), Arc::clone(&kills)),
    );

    // The ghost alone: soft Unavailable.
    let file = touch(dir.path(), "mixed.x");
    let err = ghost_manager.ensure_for_file(&file).await.unwrap_err();
    assert!(matches!(err, LspError::Unavailable(_)));

    // The real one answers; nothing masks it.
    let handles = real_manager.ensure_for_file(&file).await.unwrap();
    assert_eq!(handles.len(), 1);
}

/// A hard failure parks the key: immediate retries fail fast without
/// invoking the factory again.
#[tokio::test]
async fn failed_spawn_parks_key_in_backoff() {
    let dir = tempfile::tempdir().unwrap();
    let counter = Arc::new(AtomicUsize::new(0));
    let manager = single_spec_manager(
        dir.path(),
        failing_factory(Arc::clone(&counter), "boom"),
        Duration::from_secs(60),
    );

    let file = touch(dir.path(), "bad.a");

    let first = manager.ensure_for_file(&file).await.unwrap_err();
    assert!(matches!(first, LspError::Spawn { .. }), "got {first:?}");

    let second = manager.ensure_for_file(&file).await.unwrap_err();
    assert!(matches!(second, LspError::Unavailable(_)), "got {second:?}");
    assert_eq!(
        counter.load(Ordering::SeqCst),
        1,
        "backoff blocks the factory"
    );
    assert_eq!(
        manager.states().first().map(|(_, lifecycle)| *lifecycle),
        Some(ClientLifecycle::Failed)
    );
}

/// Zero backoff means every failed touch may try again.
#[tokio::test]
async fn zero_backoff_allows_immediate_retry() {
    let dir = tempfile::tempdir().unwrap();
    let counter = Arc::new(AtomicUsize::new(0));
    let manager = single_spec_manager(
        dir.path(),
        failing_factory(Arc::clone(&counter), "boom"),
        Duration::ZERO,
    );

    let file = touch(dir.path(), "retry.a");
    for _ in 0..3 {
        assert!(manager.ensure_for_file(&file).await.is_err());
    }
    assert_eq!(counter.load(Ordering::SeqCst), 3);
}

/// Death of the underlying process evicts the client so the next touch
/// respawns.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn dead_client_evicts_and_respawns_on_next_touch() {
    let dir = tempfile::tempdir().unwrap();
    let counter = Arc::new(AtomicUsize::new(0));
    let kills: Arc<Mutex<Vec<watch::Sender<bool>>>> = Arc::new(Mutex::new(Vec::new()));
    let manager = single_spec_manager(
        dir.path(),
        working_factory(Arc::clone(&counter), Arc::clone(&kills)),
        Duration::ZERO,
    );

    let file = touch(dir.path(), "die.a");
    let first = manager.ensure_for_file(&file).await.unwrap();
    assert_eq!(first.len(), 1);
    assert_eq!(counter.load(Ordering::SeqCst), 1);

    // Kill the only server.
    kills
        .lock()
        .unwrap()
        .pop()
        .expect("kill switch")
        .send_replace(true);

    // Wait for the evictor to notice (generous ceiling for loaded CI).
    for _ in 0..250 {
        if manager.clients_for_file(&file).is_empty() && manager.states().is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(manager.states().is_empty(), "dead client must be evicted");

    let second = manager.ensure_for_file(&file).await.unwrap();
    assert_eq!(second.len(), 1);
    assert_eq!(counter.load(Ordering::SeqCst), 2, "respawn after death");
}

#[tokio::test]
async fn stop_all_shuts_everything_down_and_clears_state() {
    let dir = tempfile::tempdir().unwrap();
    let counter = Arc::new(AtomicUsize::new(0));
    let manager = single_spec_manager(
        dir.path(),
        working_factory(Arc::clone(&counter), Arc::new(Mutex::new(Vec::new()))),
        Duration::ZERO,
    );

    let file = touch(dir.path(), "stop.a");
    manager.ensure_for_file(&file).await.unwrap();
    assert_eq!(manager.states().len(), 1);

    manager.stop_all().await;
    assert!(manager.states().is_empty());

    // Next touch starts fresh.
    manager.ensure_for_file(&file).await.unwrap();
    assert_eq!(counter.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn managed_events_are_tagged_with_identity() {
    let dir = tempfile::tempdir().unwrap();
    let (managed_tx, mut managed_rx) = mpsc::unbounded_channel();
    let mut config = ManagerConfig::new(dir.path().to_path_buf());
    config.events = Some(managed_tx);
    config.resolves_binaries = false;
    let manager = Manager::build(
        config,
        vec![spec("tagged", "unused", &[".t"], &[])],
        working_factory(
            Arc::new(AtomicUsize::new(0)),
            Arc::new(Mutex::new(Vec::new())),
        ),
    );

    let file = touch(dir.path(), "event.t");
    manager.ensure_for_file(&file).await.unwrap();

    let deadline = Duration::from_secs(3);
    loop {
        let event = tokio::time::timeout(deadline, managed_rx.recv())
            .await
            .expect("event arrives");
        let Some(event) = event else {
            panic!("channel closed before any event")
        };
        match &event.event {
            crate::lsp::Event::StateChanged(_) => {
                assert_eq!(event.server, "tagged");
                assert_eq!(
                    event.root,
                    super::super::manager::canonical_root(dir.path())
                );
                break;
            }
            _ => continue,
        }
    }
}

/// `stop_client` tears down one server and leaves siblings untouched.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stop_client_targets_only_its_key() {
    let dir = tempfile::tempdir().unwrap();
    let counter = Arc::new(AtomicUsize::new(0));
    let catalog = [
        spec("one", "unused", &[".one"], &[]),
        spec("two", "unused", &[".two"], &[]),
    ];
    let mut config = ManagerConfig::new(dir.path().to_path_buf());
    config.resolves_binaries = false;
    let manager = Manager::build(
        config,
        catalog.to_vec(),
        working_factory(Arc::clone(&counter), Arc::new(Mutex::new(Vec::new()))),
    );

    let file_one = touch(dir.path(), "a.one");
    let file_two = touch(dir.path(), "b.two");
    manager.ensure_for_file(&file_one).await.unwrap();
    manager.ensure_for_file(&file_two).await.unwrap();
    assert_eq!(manager.states().len(), 2);

    let (key_one, _) = manager
        .states()
        .into_iter()
        .find(|(key, _)| key.server == "one")
        .unwrap();
    manager.stop_client(&key_one).await;

    let states = manager.states();
    assert_eq!(states.len(), 1);
    assert_eq!(states[0].0.server, "two");
}

/// The workspace root with different drive/path casing must still be
/// recognized during the marker walk (Windows filesystems are
/// case-insensitive; `Path::starts_with` is not).
#[tokio::test]
#[cfg(windows)]
async fn root_resolution_tolerates_path_casing() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();
    std::fs::write(root.join("Cargo.toml"), "[package]\n").unwrap();

    // Flip the case of the first character of the drive prefix, whatever
    // drive the tempdir landed on (`C:` ↔ `c:`). Skip when the root has no
    // drive/UNC prefix — nothing to case-flip.
    let text = root.display().to_string();
    let cased = match text.chars().next() {
        Some(first) if first.is_ascii_alphabetic() => {
            let mut flipped = String::with_capacity(text.len());
            flipped.push(first.to_ascii_lowercase());
            flipped.push_str(&text[1..]);
            std::path::PathBuf::from(flipped)
        }
        // No drive prefix to case-flip; the walk is not exercised here.
        _ => return,
    };
    assert_ne!(
        cased.display().to_string(),
        root.display().to_string(),
        "test must build a differently-cased root to exercise the walk"
    );

    let manager = Manager::build(
        ManagerConfig::new(cased),
        vec![spec("rust", "unused", &[".rs"], &["Cargo.toml"])],
        never_factory(),
    );

    let matches = manager.matches_for_file(&root.join("lib.rs"));
    assert_eq!(
        matches.len(),
        1,
        "differently-cased workspace root must still match"
    );
}
