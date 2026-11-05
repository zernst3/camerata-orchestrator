//! What every click-through test stands on: a fresh seeded server on a free port, a request-logging
//! proxy between it and the liveview build, the liveview build itself, and a Chrome tab.
//!
//! Nothing here is left running: every process is killed by [`Stack`]'s drop, which runs when a
//! test panics too. A missing Chrome fails the test (it never skips).

use std::collections::BTreeMap;
use std::fs::File;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant};

use headless_chrome::browser::tab::ModifierKey;
use headless_chrome::{Browser, LaunchOptionsBuilder, Tab};
use serde_json::{json, Value};

/// The fixture the seed command loads.
pub fn fixture_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../server/fixtures/review-demo")
}

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// How long a test waits for the page or a server before it gives up and says what it saw.
const PATIENCE: Duration = Duration::from_secs(30);

// ── The binaries ──────────────────────────────────────────────────────────────────────────

/// `target/debug`, where the test binary's own folder (`deps`) sits.
fn target_debug() -> PathBuf {
    let exe = std::env::current_exe().expect("the test binary has a path");
    exe.parent()
        .and_then(Path::parent)
        .expect("the test binary is in target/debug/deps")
        .to_path_buf()
}

/// Build `camerata` (the seed command) and `camerata-server` once per test run, sign them the way
/// every dev build of them is signed, and return their paths. When they are up to date this is a
/// no-op build.
fn binaries() -> &'static (PathBuf, PathBuf) {
    static BUILT: OnceLock<(PathBuf, PathBuf)> = OnceLock::new();
    BUILT.get_or_init(|| {
        let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_string());
        // The variables `cargo test` sets for this test (the package, the manifest, the jobserver)
        // are not the build's: left in, they make the inner build disagree with a build from a
        // shell about what is up to date, and it recompiles the world every time.
        let mut build = Command::new(cargo);
        build
            .args(["build", "-p", "camerata", "-p", "camerata-server"])
            .current_dir(workspace_root());
        for (name, _) in std::env::vars() {
            let from_cargo_test = name.starts_with("CARGO_PKG_")
                || name.starts_with("CARGO_BIN_EXE_")
                || name.starts_with("CARGO_MANIFEST")
                || name.starts_with("CARGO_CRATE_")
                || name.starts_with("CARGO_PRIMARY_")
                || name.starts_with("CARGO_TARGET_TMPDIR")
                || name.starts_with("CARGO_BIN_NAME")
                || name == "CARGO_MAKEFLAGS"
                || name == "MAKEFLAGS"
                || name == "MFLAGS";
            if from_cargo_test {
                build.env_remove(name);
            }
        }
        let status = build.status().expect("cargo runs");
        assert!(
            status.success(),
            "cargo build of camerata and camerata-server failed"
        );
        let dir = target_debug();
        let (camerata, server) = (dir.join("camerata"), dir.join("camerata-server"));
        let _ = Command::new("/bin/bash")
            .arg(workspace_root().join("scripts/sign-dev.sh"))
            .arg(&camerata)
            .arg(&server)
            .status();
        (camerata, server)
    })
}

fn liveview_binary() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_camerata-ui"))
}

// ── Chrome ────────────────────────────────────────────────────────────────────────────────

fn chrome_path() -> PathBuf {
    if let Ok(p) = std::env::var("CAMERATA_CHROME") {
        return PathBuf::from(p);
    }
    PathBuf::from("/Applications/Google Chrome.app/Contents/MacOS/Google Chrome")
}

// ── Small helpers ─────────────────────────────────────────────────────────────────────────

/// A free port on 127.0.0.1 (found by binding and letting go; the window for a race is tiny).
pub fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .expect("a port")
        .local_addr()
        .expect("an address")
        .port()
}

fn runtime() -> &'static tokio::runtime::Runtime {
    static RT: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
    RT.get_or_init(|| {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("a runtime")
    })
}

/// GET `url` and parse the body as JSON.
pub fn get_json(url: &str) -> Value {
    runtime().block_on(async {
        let resp = reqwest::get(url).await.expect("the server answers");
        let status = resp.status();
        let text = resp.text().await.expect("a body");
        assert!(status.is_success(), "GET {url}: {status} {text}");
        serde_json::from_str(&text).unwrap_or_else(|e| panic!("GET {url}: not JSON ({e}): {text}"))
    })
}

/// GET `url`, returning the status and the body text.
pub fn get_status(url: &str) -> (u16, String) {
    runtime().block_on(async {
        let resp = reqwest::get(url).await.expect("the server answers");
        let status = resp.status().as_u16();
        (status, resp.text().await.expect("a body"))
    })
}

/// POST JSON, returning the status and the body text.
pub fn post_json(url: &str, body: &Value) -> (u16, String) {
    runtime().block_on(async {
        let resp = reqwest::Client::new()
            .post(url)
            .json(body)
            .send()
            .await
            .expect("the server answers");
        let status = resp.status().as_u16();
        (status, resp.text().await.expect("a body"))
    })
}

// ── The request log (a proxy between the liveview build and the server) ───────────────────

/// One request the liveview build made of the server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Req {
    pub method: String,
    /// Path and query, as sent.
    pub target: String,
}

impl Req {
    pub fn path(&self) -> &str {
        self.target.split('?').next().unwrap_or("")
    }
}

type Log = Arc<Mutex<Vec<Req>>>;

/// A TCP proxy that forwards every byte both ways and records the request line of every
/// HTTP/1.1 request on the way up. It exists so a test can say "no request left but the queue
/// reload" about the wire, not about what the app says it did.
struct Proxy {
    port: u16,
    stop: Arc<AtomicBool>,
}

impl Proxy {
    fn start(upstream_port: u16, log: Log) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("the proxy binds");
        let port = listener.local_addr().expect("an address").port();
        let stop = Arc::new(AtomicBool::new(false));
        let stop_flag = stop.clone();
        std::thread::spawn(move || {
            for conn in listener.incoming() {
                if stop_flag.load(Ordering::SeqCst) {
                    break;
                }
                let Ok(client) = conn else { continue };
                let Ok(upstream) = TcpStream::connect(("127.0.0.1", upstream_port)) else {
                    continue;
                };
                let (Ok(client_up), Ok(upstream_down)) = (client.try_clone(), upstream.try_clone())
                else {
                    continue;
                };
                let log = log.clone();
                std::thread::spawn(move || pump_requests(client_up, upstream, &log));
                std::thread::spawn(move || {
                    let (mut from, mut to) = (upstream_down, client);
                    let _ = std::io::copy(&mut from, &mut to);
                    let _ = to.shutdown(std::net::Shutdown::Both);
                });
            }
        });
        Self { port, stop }
    }
}

impl Drop for Proxy {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        // Wake the accept loop so it sees the flag.
        let _ = TcpStream::connect(("127.0.0.1", self.port));
    }
}

/// Copy client bytes to the server, logging each request head as it passes.
fn pump_requests(mut client: TcpStream, mut upstream: TcpStream, log: &Log) {
    let mut buf: Vec<u8> = Vec::new();
    let mut body_left = 0usize;
    let mut chunk = [0u8; 16 * 1024];
    loop {
        let n = match client.read(&mut chunk) {
            Ok(0) | Err(_) => break,
            Ok(n) => n,
        };
        if upstream.write_all(&chunk[..n]).is_err() {
            break;
        }
        buf.extend_from_slice(&chunk[..n]);
        loop {
            if body_left > 0 {
                let take = body_left.min(buf.len());
                buf.drain(..take);
                body_left -= take;
                if body_left > 0 {
                    break;
                }
            }
            let Some(end) = buf.windows(4).position(|w| w == b"\r\n\r\n") else {
                break;
            };
            let head = String::from_utf8_lossy(&buf[..end]).into_owned();
            buf.drain(..end + 4);
            let mut lines = head.lines();
            let first = lines.next().unwrap_or_default();
            let mut parts = first.split_whitespace();
            let (method, target) = (
                parts.next().unwrap_or_default().to_string(),
                parts.next().unwrap_or_default().to_string(),
            );
            body_left = lines
                .filter_map(|l| l.split_once(':'))
                .find(|(k, _)| k.trim().eq_ignore_ascii_case("content-length"))
                .and_then(|(_, v)| v.trim().parse().ok())
                .unwrap_or(0);
            log.lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push(Req { method, target });
        }
    }
    let _ = upstream.shutdown(std::net::Shutdown::Both);
}

// ── The stack ─────────────────────────────────────────────────────────────────────────────

/// Which seeded project to open.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Seed {
    /// One inspection and the reviewer's work on it: five held rows.
    Base,
    /// The base project with every held row released: the export button shows.
    Clean,
    /// A second inspection after the code moved: stale records, a moved row, a new site.
    Moved,
}

impl Seed {
    fn arg(self) -> &'static str {
        match self {
            Self::Base => "base",
            Self::Clean => "clean",
            Self::Moved => "moved",
        }
    }
}

/// Tests share one machine and one Chrome install: run them one at a time.
fn serial() -> MutexGuard<'static, ()> {
    static LOCK: Mutex<()> = Mutex::new(());
    LOCK.lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

struct Killed(Child);

impl Drop for Killed {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// A fresh seeded server, the liveview build in front of it and a Chrome tab on the page.
pub struct Stack {
    /// The seeded data folder (kept until the stack drops).
    pub data: tempfile::TempDir,
    /// The server, directly (for the test's own reads: what the server holds is the truth).
    pub server: String,
    /// `http://127.0.0.1:PORT/api/projects/ID`.
    pub api: String,
    pub project: String,
    /// The liveview build's own address.
    pub page: String,
    /// What the liveview build asked of the server, oldest first.
    log: Log,
    pub tab: Arc<Tab>,
    // Drop order matters: the tab and browser first, then the processes, then the proxy.
    _browser: Browser,
    _liveview: Killed,
    _proxy: Proxy,
    _server: Killed,
    _logs: tempfile::TempDir,
    _serial: MutexGuard<'static, ()>,
}

impl Stack {
    /// Seed `seed` into a fresh folder, start the server and the liveview build, and open the
    /// page in a new Chrome. Waits until the review shell has drawn its lane tabs.
    pub fn open(seed: Seed) -> Self {
        let serial = serial();
        let (camerata, server_bin) = binaries();
        let work = tempfile::tempdir().expect("a work folder");
        let data = tempfile::tempdir().expect("a data folder");
        let logs = tempfile::tempdir().expect("a log folder");
        let data_dir = data.path().join("seed");

        let seeded = Command::new(camerata)
            .args(["seed-review"])
            .arg(fixture_dir())
            .arg("--data-dir")
            .arg(&data_dir)
            .args(["--variant", seed.arg()])
            .current_dir(work.path())
            .env("CAMERATA_NO_KEYCHAIN", "1")
            .output()
            .expect("the seed command runs");
        let stdout = String::from_utf8_lossy(&seeded.stdout).into_owned();
        assert!(
            seeded.status.success(),
            "seed-review failed: {stdout}{}",
            String::from_utf8_lossy(&seeded.stderr)
        );
        let project = stdout
            .split("Seeded project ")
            .nth(1)
            .and_then(|s| s.split_whitespace().next())
            .expect("the seed names the project")
            .to_string();

        // The server: no model credentials, no keychain, the seeded folder, a free port.
        let server_port = free_port();
        let server_log = File::create(logs.path().join("server.log")).expect("a log file");
        let server_child = Command::new(server_bin)
            .env("CAMERATA_NO_KEYCHAIN", "1")
            .env("CAMERATA_DATA_DIR", &data_dir)
            .env("CAMERATA_SERVER_ADDR", format!("127.0.0.1:{server_port}"))
            .env_remove("ANTHROPIC_API_KEY")
            .env_remove("OPENROUTER_API_KEY")
            .env_remove("OPENAI_API_KEY")
            .current_dir(work.path())
            .stdout(Stdio::from(server_log.try_clone().expect("a log handle")))
            .stderr(Stdio::from(server_log))
            .spawn()
            .expect("the server starts");
        let server_guard = Killed(server_child);
        let server = format!("http://127.0.0.1:{server_port}");
        wait_until(PATIENCE, || {
            std::net::TcpStream::connect(("127.0.0.1", server_port)).is_ok()
                && reqwest_ok(&format!("{server}/api/health"))
        })
        .unwrap_or_else(|()| {
            panic!(
                "the server did not come up:\n{}",
                read_log(logs.path(), "server.log")
            )
        });

        // The proxy, and the liveview build pointed at it.
        let log: Log = Arc::default();
        let proxy = Proxy::start(server_port, log.clone());
        let lv_log = File::create(logs.path().join("liveview.log")).expect("a log file");
        let lv_err = File::create(logs.path().join("liveview.err")).expect("a log file");
        let lv_child = Command::new(liveview_binary())
            .args(["liveview", "--project", &project, "--port", "0"])
            .env(
                "CAMERATA_BFF_URL",
                format!("http://127.0.0.1:{}", proxy.port),
            )
            .env("CAMERATA_NO_KEYCHAIN", "1")
            .env_remove("ANTHROPIC_API_KEY")
            .env_remove("OPENROUTER_API_KEY")
            .current_dir(work.path())
            .stdout(Stdio::from(lv_log))
            .stderr(Stdio::from(lv_err))
            .spawn()
            .expect("the liveview build starts");
        let lv_guard = Killed(lv_child);
        let mut page = String::new();
        wait_until(PATIENCE, || {
            let out = read_log(logs.path(), "liveview.log");
            match out.split("listening on ").nth(1) {
                Some(rest) if rest.contains('\n') => {
                    page = rest.lines().next().unwrap_or_default().trim().to_string();
                    true
                }
                _ => false,
            }
        })
        .unwrap_or_else(|()| {
            panic!(
                "the liveview build did not come up:\n{}{}",
                read_log(logs.path(), "liveview.log"),
                read_log(logs.path(), "liveview.err")
            )
        });

        // Chrome.
        let chrome = chrome_path();
        assert!(
            chrome.exists(),
            "Google Chrome is not at {} (set CAMERATA_CHROME to its binary). The click-through \
             tests never skip.",
            chrome.display()
        );
        let profile = logs.path().join("chrome-profile");
        let options = LaunchOptionsBuilder::default()
            .path(Some(chrome))
            .headless(true)
            .window_size(Some((1440, 1000)))
            .user_data_dir(Some(profile))
            .idle_browser_timeout(Duration::from_secs(300))
            .build()
            .expect("launch options");
        let browser = Browser::new(options).expect("Chrome starts");
        let tab = browser.new_tab().expect("a tab");
        tab.set_default_timeout(PATIENCE);

        let stack = Self {
            api: format!("{server}/api/projects/{project}"),
            server,
            project,
            page: page.clone(),
            log,
            tab,
            data,
            _browser: browser,
            _liveview: lv_guard,
            _proxy: proxy,
            _server: server_guard,
            _logs: logs,
            _serial: serial,
        };
        stack.tab.navigate_to(&page).expect("the page loads");
        stack.tab.wait_until_navigated().expect("the page loaded");
        stack.install_helpers();
        stack.wait_for("the lane tabs", "T.has('.triage-tab', 'Tree')");
        stack
    }

    /// The page's own helper functions (`T.*`), installed after every navigation.
    fn install_helpers(&self) {
        self.tab
            .evaluate(HELPERS_JS, false)
            .expect("the helpers install");
    }

    // ── What the page says ──

    /// Evaluate JS and return its value.
    pub fn js(&self, expr: &str) -> Value {
        let obj = self
            .tab
            .evaluate(expr, true)
            .unwrap_or_else(|e| panic!("JS failed: {e}\n{expr}"));
        obj.value.unwrap_or(Value::Null)
    }

    pub fn js_bool(&self, expr: &str) -> bool {
        self.js(expr).as_bool().unwrap_or(false)
    }

    pub fn js_string(&self, expr: &str) -> String {
        match self.js(expr) {
            Value::String(s) => s,
            Value::Null => String::new(),
            other => other.to_string(),
        }
    }

    /// The visible text of the first element matching `selector`.
    pub fn text(&self, selector: &str) -> String {
        self.js_string(&format!("T.text({})", js_str(selector)))
    }

    /// Everything on the page, as text (for failure messages).
    pub fn page_text(&self) -> String {
        self.js_string("document.body.innerText")
    }

    /// Wait until `expr` is true in the page. On timeout, panic with what the page said, so the
    /// failure reads as an assertion about the screen.
    pub fn wait_for(&self, what: &str, expr: &str) {
        let start = Instant::now();
        loop {
            if self.js_bool(expr) {
                return;
            }
            if start.elapsed() > PATIENCE {
                let text = self.page_text();
                panic!(
                    "timed out waiting for {what} ({expr}). The page said:\n{}\n--- liveview log ---\n{}",
                    text.chars().take(2500).collect::<String>(),
                    read_log(self._logs.path(), "liveview.err")
                );
            }
            std::thread::sleep(Duration::from_millis(40));
        }
    }

    /// Wait until `selector` has an element whose text contains `text`.
    pub fn wait_text(&self, selector: &str, text: &str) {
        self.wait_for(
            &format!("{text:?} in {selector}"),
            &format!("T.has({}, {})", js_str(selector), js_str(text)),
        );
    }

    // ── What the reviewer does ──

    /// A real mouse click on the first element matching `selector` whose text contains `text`
    /// (any element when `text` is empty). Waits for the element first.
    pub fn click(&self, selector: &str, text: &str) {
        self.wait_for(
            &format!("{selector} {text:?} to click"),
            &format!("T.mark({}, {})", js_str(selector), js_str(text)),
        );
        // Let the page answer the focus (if the mark gave one) before the press.
        std::thread::sleep(Duration::from_millis(120));
        let el = self
            .tab
            .find_element("[data-t-click]")
            .unwrap_or_else(|e| panic!("the marked element is gone: {e}"));
        el.click()
            .unwrap_or_else(|e| panic!("clicking {selector} {text:?} failed: {e}"));
        self.js("T.unmark()");
    }

    /// A real click on `inner` inside the first `row` whose text contains `text`.
    pub fn click_in(&self, row: &str, text: &str, inner: &str) {
        self.wait_for(
            &format!("{inner} in {row} {text:?} to click"),
            &format!(
                "T.markIn({}, {}, {})",
                js_str(row),
                js_str(text),
                js_str(inner)
            ),
        );
        std::thread::sleep(Duration::from_millis(120));
        let el = self
            .tab
            .find_element("[data-t-click]")
            .unwrap_or_else(|e| panic!("the marked element is gone: {e}"));
        el.click()
            .unwrap_or_else(|e| panic!("clicking {inner} in {row} {text:?} failed: {e}"));
        self.js("T.unmark()");
    }

    /// Press one key (a name from the US keyboard layout: `ArrowDown`, `Enter`, `a`, `Escape`).
    pub fn key(&self, key: &str) {
        self.tab
            .press_key(key)
            .unwrap_or_else(|e| panic!("pressing {key} failed: {e}"));
    }

    /// Press a key with Control held.
    pub fn ctrl_key(&self, key: &str) {
        self.tab
            .press_key_with_modifiers(key, Some(&[ModifierKey::Ctrl]))
            .unwrap_or_else(|e| panic!("pressing Ctrl+{key} failed: {e}"));
    }

    /// Focus the first element matching `selector` (that contains `text`) and type `value`
    /// into it, replacing what was there.
    pub fn type_into(&self, selector: &str, text: &str, value: &str) {
        self.wait_for(
            &format!("{selector} {text:?} to type into"),
            &format!("T.focus({}, {})", js_str(selector), js_str(text)),
        );
        self.tab
            .send_character(value)
            .unwrap_or_else(|e| panic!("typing into {selector} failed: {e}"));
    }

    // ── What the shell says ──

    /// The progress strip's numbers, as the person sees them, and the Held count.
    pub fn strip(&self) -> Strip {
        Strip::parse(&self.text(".review-strip"))
    }

    /// Wait for the strip to read `expected`, then say so: the end of every test.
    pub fn assert_shell_counts(&self, expected: &Strip) {
        let start = Instant::now();
        loop {
            let seen = self.strip();
            if &seen == expected {
                break;
            }
            if start.elapsed() > PATIENCE {
                panic!(
                    "the strip reads {seen:?}, expected {expected:?}\nstrip text: {:?}",
                    self.text(".review-strip")
                );
            }
            std::thread::sleep(Duration::from_millis(60));
        }
        // And the server agrees with the screen: the strip is the server's counts, not a copy
        // of a stale read.
        let truth = Strip::of_server(&get_json(&format!("{}/review", self.api)));
        assert_eq!(
            &truth, expected,
            "the server's counts disagree with the strip"
        );
    }

    /// Every request the liveview build made of the server so far.
    pub fn requests(&self) -> Vec<Req> {
        self.log
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// How many requests have been made (a mark to pass to [`Self::requests_since`]).
    pub fn mark(&self) -> usize {
        self.requests().len()
    }

    pub fn requests_since(&self, mark: usize) -> Vec<Req> {
        self.requests().split_off(mark)
    }

    /// What the server's own files hold, as one string per file (the decisions file, the edits).
    pub fn store(&self, name: &str) -> Value {
        let path = self
            .data
            .path()
            .join("seed/camerata")
            .join(name)
            .join(format!("{}.json", self.project));
        let text = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()));
        serde_json::from_str(&text)
            .unwrap_or_else(|e| panic!("{} is not JSON: {e}", path.display()))
    }

    /// A hash-like fingerprint of every file in the server's data folder, to prove a gesture
    /// wrote nothing.
    pub fn data_fingerprint(&self) -> BTreeMap<String, Vec<u8>> {
        fn walk(root: &Path, at: &Path, out: &mut BTreeMap<String, Vec<u8>>) {
            let Ok(entries) = std::fs::read_dir(at) else {
                return;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    walk(root, &path, out);
                } else if path.extension().is_some_and(|e| e == "json") {
                    let rel = path
                        .strip_prefix(root)
                        .unwrap_or(&path)
                        .display()
                        .to_string();
                    out.insert(rel, std::fs::read(&path).unwrap_or_default());
                }
            }
        }
        let mut out = BTreeMap::new();
        let root = self.data.path().join("seed");
        walk(&root, &root, &mut out);
        out
    }

    /// Make a real git clone of `repo` (`owner/name`) with `files`, on disk under the data folder,
    /// and link the project's repo to it, so the app can read code at a line. Returns the clone's
    /// folder.
    pub fn link_repo(&self, repo: &str, files: &[(&str, &str)]) -> PathBuf {
        let dir = self.data.path().join("clone").join(repo.replace('/', "-"));
        std::fs::create_dir_all(&dir).expect("a clone folder");
        let git = |args: &[&str]| {
            let out = Command::new("git")
                .args(args)
                .current_dir(&dir)
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .env("GIT_CONFIG_SYSTEM", "/dev/null")
                .output()
                .expect("git runs");
            assert!(
                out.status.success(),
                "git {args:?}: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        };
        git(&["init", "-q"]);
        git(&[
            "remote",
            "add",
            "origin",
            &format!("https://github.com/{repo}.git"),
        ]);
        for (path, text) in files {
            let full = dir.join(path);
            std::fs::create_dir_all(full.parent().expect("a parent")).expect("a folder");
            std::fs::write(&full, text).expect("a file");
        }
        git(&["add", "-A"]);
        git(&[
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.com",
            "commit",
            "-q",
            "-m",
            "the clone the click-through test reads",
        ]);
        let enc = repo.replace('/', "%2F");
        let (status, body) = post_json(
            &format!("{}/repos/{enc}/link", self.api),
            &json!({ "path": dir }),
        );
        assert_eq!(status, 200, "linking {repo} failed: {body}");
        dir
    }

    /// The rows the release gate refuses, as the server lists them (held and merged without a
    /// decision).
    pub fn gate_held(&self) -> Vec<Value> {
        let gate = get_json(&format!("{}/review/export-gate", self.api));
        ["held", "merged_without_decision"]
            .iter()
            .flat_map(|k| gate[*k].as_array().cloned().unwrap_or_default())
            .collect()
    }

    /// The seeded server made no model call: its usage meter is still at zero.
    pub fn assert_no_model_calls(&self) {
        let usage = get_json(&format!("{}/api/usage", self.server));
        assert_eq!(usage["calls"], json!(0), "a model was called: {usage}");
        assert_eq!(
            usage["input_tokens"],
            json!(0),
            "tokens were spent: {usage}"
        );
    }

    /// The platform's record of what the app asked of the OS, oldest first.
    pub fn platform_calls(&self) -> Vec<Value> {
        match get_json(&format!("{}/__platform/calls", self.page)) {
            Value::Array(a) => a,
            other => panic!("the call log is not a list: {other}"),
        }
    }

    /// Script the answers to the next dialogs and copies.
    pub fn script_platform(&self, script: &Value) {
        let (status, body) = post_json(&format!("{}/__platform/script", self.page), script);
        assert_eq!(status, 204, "scripting the platform failed: {body}");
    }
}

fn reqwest_ok(url: &str) -> bool {
    runtime().block_on(async {
        reqwest::get(url)
            .await
            .is_ok_and(|r| r.status().is_success())
    })
}

fn read_log(dir: &Path, name: &str) -> String {
    std::fs::read_to_string(dir.join(name)).unwrap_or_default()
}

fn wait_until(limit: Duration, mut done: impl FnMut() -> bool) -> Result<(), ()> {
    let start = Instant::now();
    while start.elapsed() < limit {
        if done() {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    Err(())
}

/// A string as a JS literal.
pub fn js_str(s: &str) -> String {
    serde_json::to_string(s).expect("a string encodes")
}

// ── The shell counts ──────────────────────────────────────────────────────────────────────

/// The progress strip's numbers: `Undecided 15 of 31 . Confirmed 11 . Ignored 1 . FP 2 .
/// Tech debt 1 . Merged 1 . Held 5`. A term the strip leaves out is zero.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Strip {
    pub unresolved: usize,
    pub total: usize,
    pub confirmed: usize,
    pub ignored: usize,
    pub false_positive: usize,
    pub tech_debt: usize,
    pub merged: usize,
    pub held: usize,
}

impl Strip {
    /// Read the strip's text.
    pub fn parse(text: &str) -> Self {
        let mut s = Self::default();
        let flat = text.replace('\n', " ");
        for term in flat.split('\u{b7}').map(str::trim) {
            let words: Vec<&str> = term.split_whitespace().collect();
            let num = |w: &str| w.replace(',', "").parse::<usize>().unwrap_or(0);
            match words.as_slice() {
                ["Undecided", n, "of", total] => {
                    s.unresolved = num(n);
                    s.total = num(total);
                }
                ["Confirmed", n] => s.confirmed = num(n),
                ["Ignored", n] => s.ignored = num(n),
                ["FP", n] => s.false_positive = num(n),
                ["Tech", "debt", n] => s.tech_debt = num(n),
                ["Merged", n] => s.merged = num(n),
                ["Held", n] => s.held = num(n),
                _ => {}
            }
        }
        s
    }

    /// One site moves from the bucket it was in (`None` is undecided) to `to`.
    pub fn shift(&mut self, from: Option<&str>, to: &str) {
        if from == Some(to) {
            return;
        }
        *self.bucket(from.unwrap_or("")) -= 1;
        *self.bucket(to) += 1;
    }

    /// One site moves to Confirmed.
    pub fn shift_to_confirmed(&mut self, from: Option<&str>) {
        self.shift(from, "confirmed");
    }

    fn bucket(&mut self, state: &str) -> &mut usize {
        match state {
            "" => &mut self.unresolved,
            "confirmed" => &mut self.confirmed,
            "false_positive" => &mut self.false_positive,
            "tech_debt_later" => &mut self.tech_debt,
            "ignored" => &mut self.ignored,
            "merged" => &mut self.merged,
            other => panic!("no bucket for {other}"),
        }
    }

    /// The same numbers from the server's `GET /review`.
    pub fn of_server(review: &Value) -> Self {
        let c = &review["counts"];
        let n = |k: &str| usize::try_from(c[k].as_u64().unwrap_or(0)).unwrap_or(0);
        Self {
            unresolved: n("unresolved"),
            total: n("total"),
            confirmed: n("confirmed"),
            ignored: n("ignored"),
            false_positive: n("false_positive"),
            tech_debt: n("tech_debt"),
            merged: n("merged"),
            held: n("held"),
        }
    }
}

// ── The page helpers ──────────────────────────────────────────────────────────────────────

/// Installed into the page after each load. `T.mark` tags the element a real click then targets.
const HELPERS_JS: &str = r#"
window.T = {
  all: (sel) => [...document.querySelectorAll(sel)],
  // Matches on the DOM's own text, which a stylesheet's `text-transform` does not change.
  find: (sel, text) => [...document.querySelectorAll(sel)].find(
    (e) => !text || (e.textContent || '').includes(text)),
  has: (sel, text) => !!window.T.find(sel, text),
  text: (sel) => { const e = document.querySelector(sel); return e ? e.innerText : ''; },
  mark: (sel, text) => {
    window.T.unmark();
    const e = window.T.find(sel, text);
    if (!e) return false;
    e.scrollIntoView({ block: 'center' });
    e.setAttribute('data-t-click', '1');
    // A reviewer's press lands in a region that already has focus (they tabbed in or clicked
    // before). Give the region focus first, so the page does not re-draw between the press and
    // the release of this click and send the release to a different element.
    const region = e.closest('[tabindex]');
    if (region && document.activeElement !== region && !e.matches('input, textarea, select')) {
      region.focus();
    }
    return true;
  },
  // Mark `inner` inside the first `row` whose text contains `text`.
  markIn: (row, text, inner) => {
    window.T.unmark();
    const r = window.T.find(row, text);
    const e = r && r.querySelector(inner);
    if (!e) return false;
    e.scrollIntoView({ block: 'center' });
    e.setAttribute('data-t-click', '1');
    const region = e.closest('[tabindex]');
    if (region && document.activeElement !== region) region.focus();
    return true;
  },
  unmark: () => { document.querySelectorAll('[data-t-click]').forEach((e) => e.removeAttribute('data-t-click')); },
  focus: (sel, text) => {
    const e = window.T.find(sel, text);
    if (!e) return false;
    e.focus();
    if (e.select) e.select();
    return document.activeElement === e;
  },
  count: (sel) => document.querySelectorAll(sel).length,
};
true
"#;
