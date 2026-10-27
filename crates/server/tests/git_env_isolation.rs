//! The one `git` command builder (`camerata_server::workspace::git_command`) ignores the ambient
//! variables that redirect git to another repository or inject configuration. In an integration
//! test (its own process) so setting `GIT_DIR` cannot disturb any other test.

#![allow(clippy::unwrap_used)]

use camerata_server::test_git::{commit_all, git, write_file};
use camerata_server::workspace::{git_command, git_vars_to_remove, GitEnv, REPO_REDIRECTING_VARS};
use camerata_test_env::EnvGuard;

fn repo_with(file: &str) -> tempfile::TempDir {
    let tmp = tempfile::TempDir::new().unwrap();
    git(tmp.path(), &["init", "-q"]);
    write_file(tmp.path(), file, "content\n");
    commit_all(tmp.path(), "init");
    tmp
}

async fn ls_files(cmd: &mut tokio::process::Command) -> String {
    let out = cmd.arg("ls-files").output().await.unwrap();
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

#[tokio::test]
#[serial_test::serial(process_env)]
async fn a_git_dir_in_the_environment_does_not_redirect_a_code_read() {
    let ours = repo_with("ours.txt");
    let elsewhere = repo_with("elsewhere.txt");
    let _git_dir = EnvGuard::set("GIT_DIR", elsewhere.path().join(".git"));
    let _work_tree = EnvGuard::set("GIT_WORK_TREE", elsewhere.path());
    let _index = EnvGuard::set("GIT_INDEX_FILE", elsewhere.path().join(".git/index"));

    // The control: a plain command in our repo is redirected, which is the hazard.
    let mut plain = tokio::process::Command::new("git");
    plain.current_dir(ours.path());
    assert_eq!(ls_files(&mut plain).await, "elsewhere.txt");

    // The builder reads the repository it was pointed at, in both modes.
    for mode in [GitEnv::Isolated, GitEnv::Network] {
        let mut cmd = git_command(Some(ours.path()), mode);
        assert_eq!(ls_files(&mut cmd).await, "ours.txt", "{mode:?}");
    }
}

#[tokio::test]
#[serial_test::serial(process_env)]
async fn injected_config_is_stripped_for_code_reads_and_kept_for_network_steps() {
    let ours = repo_with("ours.txt");
    let _count = EnvGuard::set("GIT_CONFIG_COUNT", "1");
    let _key = EnvGuard::set("GIT_CONFIG_KEY_0", "grep.column");
    let _value = EnvGuard::set("GIT_CONFIG_VALUE_0", "true");
    let get = |mode| {
        let mut cmd = git_command(Some(ours.path()), mode);
        cmd.args(["config", "--get", "grep.column"]);
        async move {
            String::from_utf8_lossy(&cmd.output().await.unwrap().stdout)
                .trim()
                .to_string()
        }
    };
    assert_eq!(
        get(GitEnv::Isolated).await,
        "",
        "a code read sees no injected config"
    );
    // The network steps keep the indexed pairs: the tests rewrite remotes with them.
    assert_eq!(get(GitEnv::Network).await, "true");
}

#[test]
fn the_names_removed_cover_every_redirecting_variable_and_the_config_family() {
    let ambient = [
        "PATH",
        "GIT_CONFIG",
        "GIT_CONFIG_GLOBAL",
        "GIT_CONFIG_SYSTEM",
        "GIT_CONFIG_NOSYSTEM",
        "GIT_CONFIG_COUNT",
        "GIT_CONFIG_KEY_0",
        "GIT_CONFIG_VALUE_12",
    ]
    .map(String::from);
    let isolated = git_vars_to_remove(GitEnv::Isolated, ambient.iter().cloned());
    for var in REPO_REDIRECTING_VARS {
        assert!(isolated.iter().any(|v| v == var), "{var}");
    }
    for var in &ambient[1..] {
        assert!(isolated.contains(var), "{var}");
    }
    assert!(!isolated.iter().any(|v| v == "PATH"));
    let network = git_vars_to_remove(GitEnv::Network, ambient.iter().cloned());
    for var in [
        "GIT_CONFIG_COUNT",
        "GIT_CONFIG_KEY_0",
        "GIT_CONFIG_VALUE_12",
    ] {
        assert!(
            !network.iter().any(|v| v == var),
            "{var} stays for network steps"
        );
    }
    for var in [
        "GIT_CONFIG",
        "GIT_CONFIG_GLOBAL",
        "GIT_CONFIG_SYSTEM",
        "GIT_CONFIG_NOSYSTEM",
    ] {
        assert!(network.iter().any(|v| v == var), "{var}");
    }
    for var in REPO_REDIRECTING_VARS {
        assert!(network.iter().any(|v| v == var), "{var}");
    }
    // And the one process-level setting the builder adds.
    let cmd = git_command(None, GitEnv::Isolated);
    let envs: Vec<_> = cmd.as_std().get_envs().collect();
    assert!(envs
        .iter()
        .any(|(k, v)| *k == "GIT_OPTIONAL_LOCKS" && *v == Some(std::ffi::OsStr::new("0"))));
}
