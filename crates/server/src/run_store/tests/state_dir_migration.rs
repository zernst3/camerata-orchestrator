//! Through the real resolver ([`crate::scan_cache::state_dir::dir`]), with `HOME` and the XDG
//! folders pointed at a temporary directory: a run created in the old cache folder is moved on
//! first use and resumes from the app data folder; the override and a failed move both leave
//! the old folder where it is.

use std::path::{Path, PathBuf};

use camerata_api_types::state_dir::{cache_dir_for, default_dir_for, Platform};
use camerata_test_env::EnvGuard;

use super::*;

/// The old and new default folders for a fake home under `root`, with every variable the
/// resolver reads pinned (the override removed), so nothing on the machine running the test is
/// read or written.
struct FakeHome {
    _root: tempfile::TempDir,
    old: PathBuf,
    new: PathBuf,
    _env: Vec<EnvGuard>,
}

fn fake_home() -> FakeHome {
    let root = tempfile::tempdir().unwrap();
    let home = root.path().join("home");
    let xdg_data = root.path().join("xdg-data");
    let xdg_cache = root.path().join("xdg-cache");
    let env = vec![
        EnvGuard::remove(crate::scan_cache::state_dir::STATE_DIR_ENV),
        EnvGuard::set("HOME", &home),
        EnvGuard::set("XDG_DATA_HOME", &xdg_data),
        EnvGuard::set("XDG_CACHE_HOME", &xdg_cache),
    ];
    let platform = Platform::current();
    let old = cache_dir_for(
        platform,
        None,
        Some(home.as_os_str()),
        Some(xdg_cache.as_os_str()),
    );
    let new = default_dir_for(
        platform,
        None,
        Some(home.as_os_str()),
        Some(xdg_data.as_os_str()),
    );
    FakeHome {
        _root: root,
        old,
        new,
        _env: env,
    }
}

/// Create a run the way a released build did: with the state folder at `state` (the override
/// names the base, `camerata/` is appended), one good reply and one failed call stored.
async fn run_saved_in(state: &Path, repo: &tempfile::TempDir, model: &Scripted) -> String {
    let base = state.parent().unwrap();
    let _at = EnvGuard::set(crate::scan_cache::state_dir::STATE_DIR_ENV, base);
    let store = new_run(repo);
    {
        let journal = JournalLlm::new(model, store.clone());
        assert!(journal.complete(ask("part one")).await.is_ok());
        assert!(journal.complete(ask("BOOM part")).await.is_err());
    }
    let id = store.run_id();
    drop(store);
    assert!(state.join("runs").join(&id).join("run.json").exists());
    id
}

/// Open the run again and prove it is the same run: the next attempt, the stored good reply
/// replayed without a model call and the failed one asked again.
async fn assert_resumes(id: &str, model: &Scripted) {
    let store = RunStore::open(id).unwrap();
    assert_eq!(
        store.record().attempt,
        2,
        "the saved run is continued, not restarted"
    );
    model.fail_boom.store(false, Ordering::SeqCst);
    let before = model.calls();
    let journal = JournalLlm::new(model, store.clone());
    let replayed = journal.complete(ask("part one")).await.unwrap();
    assert_eq!(
        model.calls(),
        before,
        "the stored reply answered without a model call"
    );
    assert!(replayed.text.contains("part one"));
    assert!(journal.complete(ask("BOOM part")).await.is_ok());
    assert_eq!(
        model.calls(),
        before + 1,
        "only the failed call went to the model"
    );
}

#[tokio::test]
#[serial_test::serial(process_env)]
async fn a_run_saved_in_the_cache_folder_moves_on_first_use_and_resumes() {
    seeded(async {
        let fx = fake_home();
        let repo = git_repo();
        let model = Scripted::new();
        model.fail_boom.store(true, Ordering::SeqCst);
        let id = run_saved_in(&fx.old, &repo, &model).await;

        assert_eq!(
            runs_dir(),
            fx.new.join("runs"),
            "the new default is the app data folder"
        );
        assert!(fx.new.join("runs").join(&id).join("run.json").exists());
        assert!(!fx.old.join("runs").join(&id).exists(), "moved, not copied");
        assert!(
            list_runs().iter().any(|r| r.run_id == id),
            "the moved run is listed"
        );
        assert_resumes(&id, &model).await;
    })
    .await;
}

#[tokio::test]
#[serial_test::serial(process_env)]
async fn a_failed_move_keeps_the_old_folder_and_the_run_still_resumes_there() {
    seeded(async {
        let fx = fake_home();
        let repo = git_repo();
        let model = Scripted::new();
        model.fail_boom.store(true, Ordering::SeqCst);
        let id = run_saved_in(&fx.old, &repo, &model).await;
        // Something the move may not overwrite: a file where the runs folder would go.
        std::fs::create_dir_all(&fx.new).unwrap();
        std::fs::write(fx.new.join("runs"), "not a folder").unwrap();

        assert_eq!(
            runs_dir(),
            fx.old.join("runs"),
            "the old folder stays in use"
        );
        assert!(fx.old.join("runs").join(&id).join("run.json").exists());
        assert_eq!(
            std::fs::read_to_string(fx.new.join("runs")).unwrap(),
            "not a folder"
        );
        assert_resumes(&id, &model).await;
    })
    .await;
}

#[test]
#[serial_test::serial(process_env)]
fn nothing_is_migrated_when_the_override_is_set() {
    let fx = fake_home();
    std::fs::create_dir_all(fx.old.join("runs/r-1")).unwrap();
    std::fs::write(fx.old.join("runs/r-1/run.json"), "{}").unwrap();
    let explicit = tempfile::tempdir().unwrap();
    let _state = EnvGuard::set(crate::scan_cache::state_dir::STATE_DIR_ENV, explicit.path());

    assert_eq!(runs_dir(), explicit.path().join("camerata").join("runs"));
    assert!(
        fx.old.join("runs/r-1/run.json").exists(),
        "the old run is left alone"
    );
    assert!(
        !fx.new.join("runs").exists(),
        "and nothing was moved to the new folder"
    );
}
