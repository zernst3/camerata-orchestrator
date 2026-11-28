use std::collections::BTreeSet;
use std::sync::Arc;

use super::fetch::{self, ArtifactSource};
use super::fixtures::FakeTools;
use super::pins::{self, Artifact, ALL_ARTIFACTS};
use super::*;
use crate::scan_tools::ScanTool;
use crate::tool_provisioning::ProvisionError;

fn needs(tools: &[ScanTool], dep_audit: bool) -> RequiredTools {
    RequiredTools {
        scan_tools: tools.iter().copied().collect::<BTreeSet<_>>(),
        dep_audit,
    }
}

fn state_of(report: &ToolSetReport, c: Component) -> ComponentState {
    report
        .entries
        .iter()
        .find(|e| e.component == c)
        .unwrap_or_else(|| panic!("{c:?} is not in the list: {report:?}"))
        .state
        .clone()
}

// ── platform selection ───────────────────────────────────────────────────────────────────────

#[test]
fn the_platform_is_chosen_from_the_os_and_architecture() {
    assert_eq!(
        Platform::from_os_arch("macos", "aarch64"),
        Some(Platform::MacArm64)
    );
    // Intel Macs are not supported (see `there_are_no_intel_mac_pins`).
    assert_eq!(Platform::from_os_arch("macos", "x86_64"), None);
    assert_eq!(
        Platform::from_os_arch("linux", "x86_64"),
        Some(Platform::LinuxX64)
    );
    assert_eq!(
        Platform::from_os_arch("linux", "aarch64"),
        Some(Platform::LinuxArm64)
    );
    assert_eq!(Platform::from_os_arch("windows", "x86_64"), None);
    assert_eq!(Platform::from_os_arch("macos", "powerpc"), None);
}

#[test]
fn the_only_mac_pin_is_apple_silicon_and_linux_pins_both_architectures() {
    for artifact in ALL_ARTIFACTS {
        let p = pins::pins(artifact);
        let mac = p.for_platform(Platform::MacArm64).expect("arm64 pin");
        let mac_word = match artifact {
            Artifact::Node => "darwin-arm64",
            Artifact::OsvScanner => "darwin_arm64",
            _ => "aarch64-apple-darwin",
        };
        assert!(mac.url.contains(mac_word), "{}", mac.url);
        let x64 = p
            .for_platform(Platform::LinuxX64)
            .expect("linux x86_64 pin");
        let arm = p
            .for_platform(Platform::LinuxArm64)
            .expect("linux arm64 pin");
        assert_ne!(x64.sha256, arm.sha256, "{}", p.name);
    }
}

// ── the pins ─────────────────────────────────────────────────────────────────────────────────

#[test]
fn every_pin_downloads_from_its_projects_official_release_location() {
    let expected = [
        (
            Artifact::Ruff,
            "https://github.com/astral-sh/ruff/releases/download/",
        ),
        (Artifact::Node, "https://nodejs.org/dist/"),
        (
            Artifact::Python,
            "https://github.com/astral-sh/python-build-standalone/releases/download/",
        ),
        (
            Artifact::OsvScanner,
            "https://github.com/google/osv-scanner/releases/download/",
        ),
    ];
    for (artifact, official) in expected {
        let p = pins::pins(artifact);
        assert_eq!(p.official_source, official, "{}", p.name);
        assert_eq!(p.pins.len(), Platform::ALL.len(), "{}", p.name);
        for pin in p.pins {
            assert!(pin.url.starts_with(official), "{}", pin.url);
            assert!(
                pin.url
                    .contains(p.version.split('+').next().unwrap_or(p.version)),
                "the URL names the pinned version: {}",
                pin.url
            );
            assert!(fetch::is_official_url(pin.url), "{}", pin.url);
        }
    }
}

#[test]
fn every_checksum_is_a_lowercase_sha256() {
    for artifact in ALL_ARTIFACTS {
        for pin in pins::pins(artifact).pins {
            assert_eq!(pin.sha256.len(), 64, "{}", pin.url);
            assert!(
                pin.sha256
                    .chars()
                    .all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c)),
                "{}",
                pin.url
            );
        }
    }
}

#[test]
fn the_semgrep_requirements_pin_every_package_by_hash() {
    let text = SEMGREP_REQUIREMENTS;
    assert!(text.contains(&format!("semgrep=={}", pins::SEMGREP_VERSION)));
    // Every requirement (a line that starts a package) is followed by at least one hash.
    let mut packages = 0;
    let lines: Vec<&str> = text.lines().collect();
    for (i, line) in lines.iter().enumerate() {
        if line.is_empty() || line.starts_with('#') || line.starts_with(' ') {
            continue;
        }
        packages += 1;
        assert!(line.contains("=="), "an exact pin: {line}");
        assert!(
            lines
                .get(i + 1)
                .is_some_and(|next| next.trim_start().starts_with("--hash=sha256:")),
            "{line} has no hash"
        );
    }
    assert!(packages > 10, "{packages}");
}

#[test]
fn the_eslint_version_shown_is_the_pinned_one() {
    assert!(crate::tool_provisioning::ESLINT_NPM_PACKAGES
        .contains(&format!("eslint@{}", crate::tool_provisioning::ESLINT_VERSION).as_str()));
}

// ── download, checksum, install ──────────────────────────────────────────────────────────────

#[tokio::test]
async fn a_missing_tool_is_downloaded_verified_and_installed_once() {
    let tools = FakeTools::new();
    assert!(!tools.env.is_installed(Artifact::Ruff));
    let ruff = tools.env.ensure_ruff().await.expect("provisions");
    assert_eq!(ruff, tools.entry(Artifact::Ruff));
    assert!(ruff.starts_with(tools.tooling()), "{}", ruff.display());
    assert!(tools.env.is_installed(Artifact::Ruff));
    let out = std::process::Command::new(&ruff)
        .arg("--version")
        .output()
        .expect("the installed ruff starts");
    assert!(out.status.success());
    // Installed: a second call downloads nothing.
    tools.env.ensure_ruff().await.expect("still there");
    assert_eq!(
        tools.source.fetched().len(),
        1,
        "{:?}",
        tools.source.fetched()
    );
}

#[tokio::test]
async fn a_download_that_does_not_match_its_pinned_checksum_is_refused_and_nothing_is_installed() {
    let tools = FakeTools::new();
    tools.tamper(Artifact::Ruff);
    let err = tools.env.ensure_ruff().await.expect_err("refused");
    match &err {
        ProvisionError::ChecksumMismatch {
            tool,
            expected,
            actual,
        } => {
            assert_eq!(*tool, "ruff");
            assert_ne!(expected, actual);
        }
        other => panic!("expected a checksum refusal, got {other:?}"),
    }
    assert!(!tools.env.is_installed(Artifact::Ruff));
    assert!(
        !tools.entry(Artifact::Ruff).exists(),
        "a refused download is never unpacked"
    );
}

#[test]
fn verify_checksum_accepts_only_the_exact_hash() {
    let bytes = b"acme-tool";
    let good = fetch::sha256_hex(bytes);
    assert!(fetch::verify_checksum("acme", bytes, &good).is_ok());
    let mut bad = good.clone();
    bad.replace_range(0..1, if good.starts_with('0') { "1" } else { "0" });
    assert!(matches!(
        fetch::verify_checksum("acme", bytes, &bad),
        Err(ProvisionError::ChecksumMismatch { .. })
    ));
}

#[tokio::test]
async fn a_failed_download_is_a_refusal_naming_the_tool() {
    let tools = FakeTools::new();
    tools
        .source
        .fail_everything("network unreachable (fixture)");
    let err = tools.env.ensure_ruff().await.expect_err("refused");
    assert!(
        matches!(&err, ProvisionError::DownloadFailed { tool: "ruff", detail } if detail.contains("unreachable")),
        "{err:?}"
    );
}

#[tokio::test]
async fn the_official_source_refuses_a_url_outside_the_official_locations() {
    // Refused before any request is made, so this test reaches no network.
    let err = OfficialReleaseSource::default()
        .fetch("https://mirror.acme-downloads.example/ruff.tar.gz")
        .await
        .expect_err("refused");
    assert!(err.contains("not an official release location"), "{err}");
}

#[tokio::test]
async fn a_test_process_never_provisions_into_the_real_tooling_folder() {
    let Some(real) = crate::tool_provisioning::tooling_dir() else {
        return;
    };
    let fakes = FakeTools::new();
    let fake_pins = fakes.env.clone();
    let env = ToolEnv::new(real.clone(), fakes.env.platform, fakes.source.clone())
        .with_pins(move |a| fake_pins.pins(a));
    let err = env.ensure_ruff().await.expect_err("refused");
    assert!(
        matches!(err, ProvisionError::RealToolingProtected { tool: "ruff" }),
        "{err:?}"
    );
    assert!(fakes.source.fetched().is_empty(), "nothing was downloaded");
}

// ── the child PATH ───────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn the_child_path_is_camerata_runtimes_then_the_os_folders_never_the_users_path() {
    let tools = FakeTools::new();
    let before = tools.env.child_path();
    assert_eq!(
        std::env::split_paths(&before).collect::<Vec<_>>(),
        SYSTEM_PATH_DIRS
            .iter()
            .map(std::path::PathBuf::from)
            .collect::<Vec<_>>()
    );
    let node = tools.env.ensure_node().await.expect("node");
    let after: Vec<_> = std::env::split_paths(&tools.env.child_path()).collect();
    assert_eq!(
        after.first(),
        node.node.parent().map(|p| p.to_path_buf()).as_ref()
    );
    for dir in &after {
        assert!(
            dir.starts_with(tools.tooling())
                || SYSTEM_PATH_DIRS
                    .iter()
                    .any(|s| dir == std::path::Path::new(s)),
            "{} is neither Camerata's nor an OS folder",
            dir.display()
        );
    }
}

// ── the tool set ─────────────────────────────────────────────────────────────────────────────

#[test]
fn each_tool_needs_its_runtime_first() {
    let tools = FakeTools::new();
    let report = tool_set_report(
        &tools.env,
        &needs(&[ScanTool::Ruff, ScanTool::Eslint, ScanTool::Semgrep], true),
    );
    let order: Vec<Component> = report.entries.iter().map(|e| e.component).collect();
    assert_eq!(
        order,
        vec![
            Component::Ruff,
            Component::Node,
            Component::EslintPackages,
            Component::Python,
            Component::SemgrepPackages,
            Component::OsvScanner,
        ]
    );
    assert!(report
        .entries
        .iter()
        .all(|e| e.state == ComponentState::WillInstall));
}

#[test]
fn the_list_says_what_is_ready_what_will_be_installed_and_what_cannot_be() {
    let tools = FakeTools::new();
    let report = tool_set_report(
        &tools.env,
        &needs(&[ScanTool::Ruff, ScanTool::Clippy], true),
    );
    let text = report.render();
    assert!(
        text.contains(&format!(
            "ruff {} (for ruff): will be installed",
            pins::RUFF_VERSION
        )),
        "{text}"
    );
    assert!(
        text.contains("osv-scanner v1.9.2 (for osv-scanner): will be installed"),
        "{text}"
    );
    assert!(text.contains("cargo clippy"), "{text}");
    assert!(text.contains("not provisioned"), "{text}");
    assert!(
        !text.contains('\u{2014}') && !text.contains('\u{2013}'),
        "no em or en dash: {text}"
    );
}

#[tokio::test]
async fn provisioning_the_tool_set_installs_everything_and_reports_it_ready() {
    let tools = FakeTools::new();
    let required = needs(&[ScanTool::Ruff, ScanTool::Eslint, ScanTool::Semgrep], true);
    let report = provision_tool_set(&tools.env, &required, true)
        .await
        .expect("provisions");
    assert!(
        report
            .entries
            .iter()
            .all(|e| e.state == ComponentState::Ready),
        "{}",
        report.render()
    );
    assert!(semgrep_is_installed(&tools.env));
    assert!(semgrep_bin(tools.tooling()).starts_with(tools.tooling()));
}

#[tokio::test]
async fn a_tool_set_that_cannot_be_downloaded_is_refused_for_client_and_internal_runs() {
    for client in [true, false] {
        let tools = FakeTools::new();
        tools
            .source
            .fail_everything("network unreachable (fixture)");
        let refusal = provision_tool_set(&tools.env, &needs(&[ScanTool::Ruff], false), client)
            .await
            .expect_err("refused");
        assert_eq!(refusal.component, Component::Ruff);
        assert!(refusal
            .to_string()
            .contains("ruff 0.17.5 could not be provisioned"));
    }
}

#[tokio::test]
async fn a_tampered_runtime_refuses_the_tool_that_runs_on_it() {
    let tools = FakeTools::new();
    tools.tamper(Artifact::Node);
    let refusal = provision_tool_set(&tools.env, &needs(&[ScanTool::Eslint], false), true)
        .await
        .expect_err("refused");
    assert_eq!(refusal.component, Component::Node);
    assert!(refusal.reason.contains("checksum"), "{}", refusal.reason);
}

#[tokio::test]
async fn a_tool_with_no_build_for_this_machine_refuses_a_client_run_only() {
    let fakes = FakeTools::new();
    let fake_pins = fakes.env.clone();
    let env = ToolEnv::new(fakes.tooling().to_path_buf(), None, fakes.source.clone())
        .with_pins(move |a| fake_pins.pins(a));
    let required = needs(&[ScanTool::Ruff], false);
    let refusal = provision_tool_set(&env, &required, true)
        .await
        .expect_err("a client run is refused");
    assert!(
        refusal
            .reason
            .contains("Camerata provisions its tools on Apple silicon Macs and on Linux"),
        "{}",
        refusal.reason
    );
    let report = provision_tool_set(&env, &required, false)
        .await
        .expect("an internal run goes ahead");
    assert!(matches!(
        state_of(&report, Component::Ruff),
        ComponentState::CannotProvision(_)
    ));
    assert!(fakes.source.fetched().is_empty(), "nothing was downloaded");
}

#[tokio::test]
async fn the_list_reads_the_disk_only() {
    let tools = FakeTools::new();
    let _ = tool_set_report(
        &tools.env,
        &needs(&[ScanTool::Ruff, ScanTool::Eslint, ScanTool::Semgrep], true),
    );
    assert!(tools.source.fetched().is_empty());
    assert!(!tools.tooling().exists(), "the list created nothing");
}

/// The source the product uses refuses nothing official: a check that the seam is wired, not
/// a download.
#[test]
fn the_product_environment_uses_the_official_source() {
    if let Ok(env) = ToolEnv::product() {
        assert_eq!(env.platform, Platform::current());
        let _: Arc<dyn ArtifactSource> = env.source.clone();
        assert_eq!(env.pins(Artifact::Ruff), pins::pins(Artifact::Ruff));
    }
}

// ── the on-demand real first run (the one ignored test, #587) ────────────────────────────────

/// A REAL first-run provision: downloads every pinned tool and runtime from its official release
/// location into a temporary tooling folder, verifies every checksum, installs semgrep from its
/// hash-locked requirements and eslint from its lockfile, then runs each tool once with
/// Camerata's child `PATH`. Needs the network; nothing else runs it. For the owner, on a clean Mac:
///
/// ```text
/// cargo test -p camerata-server --lib real_first_run_provision -- --ignored --nocapture
/// ```
///
/// The temporary folder is removed afterwards; the real tooling folder is never touched.
#[tokio::test]
#[ignore = "on demand: downloads every pinned tool for real (about 200 MB); see the doc comment"]
async fn real_first_run_provision() {
    let dir = tempfile::tempdir().expect("a temporary tooling folder");
    // A network that re-signs TLS through a proxy (a cloud sandbox) names its CA in
    // SSL_CERT_FILE; on a Mac on an ordinary network this is unset and changes nothing.
    let pem = std::env::var_os("SSL_CERT_FILE").and_then(|f| std::fs::read(f).ok());
    let source = OfficialReleaseSource::for_the_first_run_harness(pem);
    let env = ToolEnv::new(
        dir.path().join("tooling"),
        Platform::current(),
        Arc::new(source),
    );
    let platform = env
        .platform
        .unwrap_or_else(|| panic!("{}", platform::unsupported_platform_reason()));
    eprintln!("platform: {}", platform.label());
    let required = needs(&[ScanTool::Ruff, ScanTool::Eslint, ScanTool::Semgrep], true);
    eprint!("before:\n{}", tool_set_report(&env, &required).render());
    let report = provision_tool_set(&env, &required, false)
        .await
        .unwrap_or_else(|refusal| panic!("provisioning refused: {refusal}"));
    eprint!("after:\n{}", report.render());
    for entry in &report.entries {
        match &entry.state {
            ComponentState::Ready => {}
            ComponentState::CannotProvision(why) => {
                eprintln!(
                    "{}: cannot be provisioned here: {why}",
                    entry.component.name()
                )
            }
            other => panic!("{} is {other:?}", entry.component.name()),
        }
    }

    let child_path = env.child_path();
    let run = |program: &std::path::Path, args: &[&std::ffi::OsStr]| {
        let out = std::process::Command::new(program)
            .args(args)
            .env("PATH", &child_path)
            .output()
            .unwrap_or_else(|e| panic!("{} did not start: {e}", program.display()));
        assert!(
            out.status.success(),
            "{} failed: {}",
            program.display(),
            String::from_utf8_lossy(&out.stderr)
        );
        let version = String::from_utf8_lossy(&out.stdout).trim().to_string();
        eprintln!("{}: {version}", program.display());
        version
    };
    let version = std::ffi::OsStr::new("--version");
    assert!(run(&env.ensure_ruff().await.unwrap(), &[version]).contains(pins::RUFF_VERSION));
    assert!(run(&env.ensure_osv_scanner().await.unwrap(), &[version]).contains("1.9.2"));
    let node = env.ensure_node().await.unwrap();
    assert_eq!(run(&node.node, &[version]), pins::NODE_VERSION);
    let eslint = crate::tool_provisioning::eslint_run_paths(&env.tooling)
        .await
        .unwrap();
    assert!(run(&node.node, &[eslint.script.as_os_str(), version])
        .contains(crate::tool_provisioning::ESLINT_VERSION));
    if semgrep::semgrep_unavailable_reason(&env).is_none() {
        assert!(run(&semgrep_bin(&env.tooling), &[version]).contains(pins::SEMGREP_VERSION));
    }
}

// ── review round 1: no test path downloads (#587) ────────────────────────────────────────────

#[test]
#[serial_test::serial(process_env)]
fn the_tooling_folder_can_be_pointed_elsewhere() {
    let dir = tempfile::tempdir().unwrap();
    let _over =
        camerata_test_env::EnvGuard::set(crate::tool_provisioning::TOOLING_DIR_ENV, dir.path());
    assert_eq!(
        crate::tool_provisioning::tooling_dir().as_deref(),
        Some(dir.path())
    );
    let env = ToolEnv::product().expect("an environment");
    assert_eq!(env.tooling, dir.path());
}

#[tokio::test]
#[serial_test::serial(process_env)]
async fn offline_a_missing_tool_is_a_clear_refusal_and_nothing_is_downloaded() {
    let dir = tempfile::tempdir().unwrap();
    let _over =
        camerata_test_env::EnvGuard::set(crate::tool_provisioning::TOOLING_DIR_ENV, dir.path());
    let _offline = camerata_test_env::EnvGuard::set(OFFLINE_ENV, "1");
    let env = ToolEnv::product().expect("an environment");
    let err = env.ensure_ruff().await.expect_err("refused offline");
    assert!(
        matches!(err, ProvisionError::Offline { tool: "ruff", .. }),
        "{err:?}"
    );
    let text = err.to_string();
    assert!(text.contains("CAMERATA_OFFLINE=1"), "{text}");
    assert!(text.contains("nothing was downloaded"), "{text}");
    let refusal = provision_tool_set(&env, &needs(&[ScanTool::Ruff], false), false)
        .await
        .expect_err("an internal run is refused too");
    assert!(refusal.reason.contains("offline"), "{}", refusal.reason);
}

/// The fetcher that reaches the internet refuses outright in a cargo test binary, so no test,
/// whatever it forgets to set, can download: it must hand in a fixture source.
#[tokio::test]
async fn a_test_process_cannot_reach_the_official_fetcher() {
    let url = pins::pins(Artifact::Ruff).pins[0].url;
    let err = OfficialReleaseSource::default()
        .fetch(url)
        .await
        .expect_err("refused");
    assert!(err.contains("test process"), "{err}");
}

#[test]
#[serial_test::serial(process_env)]
fn the_disable_variables_take_their_tools_out_of_the_tool_set() {
    let repo = tempfile::tempdir().unwrap();
    let sources = vec![("acme/one".to_string(), repo.path().to_path_buf())];
    let scope = crate::onboard::scope::ScanScope::none();
    {
        let _off = camerata_test_env::EnvGuard::set(crate::dep_audit::DISABLE_ENV_VAR, "1");
        assert!(!required_tool_set(&sources, &[], None, &scope, true).dep_audit);
    }
    {
        let _on = camerata_test_env::EnvGuard::remove(crate::dep_audit::DISABLE_ENV_VAR);
        assert!(required_tool_set(&sources, &[], None, &scope, true).dep_audit);
    }
    let _semgrep_off =
        camerata_test_env::EnvGuard::set(crate::scan_tools::DISABLE_SEMGREP_ENV_VAR, "1");
    let without = needs(&[ScanTool::Ruff, ScanTool::Semgrep], false).without_disabled();
    assert_eq!(
        without.scan_tools.into_iter().collect::<Vec<_>>(),
        vec![ScanTool::Ruff]
    );
}

/// The folder the tests that start the `camerata` binary hand it: every tool reads as installed
/// under the REAL pins, so the binary, offline, finds them all ready.
#[tokio::test]
async fn seeded_fakes_read_as_installed_under_the_real_pins_and_offline_nothing_is_fetched() {
    let dir = tempfile::tempdir().unwrap();
    super::fixtures::seed_fakes_under_the_real_pins(dir.path()).await;
    let env = ToolEnv::new(
        dir.path(),
        Platform::current().or(Some(Platform::LinuxX64)),
        Arc::new(OfflineSource),
    );
    let required = needs(&[ScanTool::Ruff, ScanTool::Eslint, ScanTool::Semgrep], true);
    let report = provision_tool_set(&env, &required, true)
        .await
        .expect("offline, everything is already there");
    assert!(
        report
            .entries
            .iter()
            .all(|e| e.state == ComponentState::Ready),
        "{}",
        report.render()
    );
}

// ── owner ruling: Intel Macs are not supported ───────────────────────────────────────────────

#[test]
fn an_intel_mac_is_refused_with_one_plain_line_and_every_other_machine_is_not() {
    assert_eq!(
        platform::machine_refusal("macos", "x86_64"),
        Some(platform::INTEL_MAC_REFUSAL)
    );
    assert!(!platform::INTEL_MAC_REFUSAL.contains('\n'));
    for (os, arch) in [
        ("macos", "aarch64"),
        ("linux", "x86_64"),
        ("linux", "aarch64"),
    ] {
        assert_eq!(platform::machine_refusal(os, arch), None, "{os} {arch}");
    }
    let env = ToolEnv::for_machine(
        "macos",
        "x86_64",
        "/home/acme/tooling",
        Arc::new(OfflineSource),
    );
    assert_eq!(env.machine_refusal(), Some(platform::INTEL_MAC_REFUSAL));
}

#[test]
fn there_are_no_intel_mac_pins() {
    assert_eq!(Platform::from_os_arch("macos", "x86_64"), None);
    for artifact in ALL_ARTIFACTS {
        for pin in pins::pins(artifact).pins {
            for intel in ["x86_64-apple-darwin", "darwin-x64", "darwin_amd64"] {
                assert!(!pin.url.contains(intel), "{}", pin.url);
            }
        }
    }
}

// ── review round 1: transport and install hygiene ─────────────────────────────────────────────

/// Every download is HTTPS, redirects included: a plain-http URL is refused before a connection
/// is made. Shown against a local server, so nothing leaves the machine.
#[tokio::test]
async fn the_download_client_refuses_plain_http_even_to_a_local_server() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let port = listener.local_addr().unwrap().port();
    let served = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let flag = served.clone();
    let server = std::thread::spawn(move || {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        while std::time::Instant::now() < deadline {
            if let Ok((mut conn, _)) = listener.accept() {
                use std::io::Write;
                flag.store(true, std::sync::atomic::Ordering::SeqCst);
                let _ = conn.write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 2\r\n\r\nok");
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
    });
    let client = fetch::download_client(None).unwrap();
    let result = client
        .get(format!("http://127.0.0.1:{port}/acme.tar.gz"))
        .send()
        .await;
    server.join().unwrap();
    assert!(result.is_err(), "a plain-http request was sent: {result:?}");
    assert!(
        !served.load(std::sync::atomic::Ordering::SeqCst),
        "the local server was reached"
    );
}

#[test]
fn a_download_over_the_size_cap_is_refused() {
    let mut buf = Vec::new();
    assert!(fetch::take_chunk(&mut buf, b"12345", 10).is_ok());
    assert!(fetch::take_chunk(&mut buf, b"67890", 10).is_ok());
    let err = fetch::take_chunk(&mut buf, b"!", 10).expect_err("over the cap");
    assert!(err.contains("10 bytes"), "{err}");
    assert!(fetch::MAX_DOWNLOAD_BYTES >= 128 * 1024 * 1024);
}

#[test]
fn the_aside_folder_keeps_the_whole_install_name() {
    let dir = std::path::Path::new("/home/acme/tooling/pinned/ruff-0.17.5");
    let aside = fetch::aside_path(dir);
    assert_eq!(aside.parent(), dir.parent());
    let name = aside.file_name().unwrap().to_string_lossy().into_owned();
    assert!(name.starts_with(".ruff-0.17.5.old-"), "{name}");
}

#[tokio::test]
async fn a_stale_staging_folder_is_swept_before_an_install() {
    let tools = FakeTools::new();
    let root = fetch::pinned_root(tools.tooling());
    let stale = root.join(".staging-ruff-0.17.5-dead");
    std::fs::create_dir_all(&stale).unwrap();
    std::fs::write(stale.join("half"), "an interrupted install").unwrap();
    let other = root.join(".staging-node-v24.22.3-dead");
    std::fs::create_dir_all(&other).unwrap();
    tools.env.ensure_ruff().await.unwrap();
    assert!(
        !stale.exists(),
        "the dead install's staging folder is removed"
    );
    assert!(
        other.exists(),
        "another tool's staging folder is left to its own install"
    );
}

/// Two installs of the same artifact at once (two scans, two processes): one downloads, the
/// other waits on the lock, re-probes and uses that install.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_concurrent_installs_of_one_artifact_download_once_and_both_get_it() {
    let tools = FakeTools::new();
    tools
        .source
        .delay_every_fetch(std::time::Duration::from_millis(300));
    let (a, b) = tokio::join!(
        {
            let env = tools.env.clone();
            tokio::spawn(async move { env.ensure_ruff().await })
        },
        {
            let env = tools.env.clone();
            tokio::spawn(async move { env.ensure_ruff().await })
        },
    );
    let (a, b) = (a.unwrap().expect("first"), b.unwrap().expect("second"));
    assert_eq!(a, b);
    assert_eq!(
        tools.source.fetched().len(),
        1,
        "{:?}",
        tools.source.fetched()
    );
    assert!(tools.env.is_installed(Artifact::Ruff));
}
