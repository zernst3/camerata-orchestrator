//! Everything the app asks the operating system for: file dialogs and the clipboard.
//!
//! [`Platform`] is the one seam. The desktop app uses [`DesktopPlatform`] (the native dialogs of
//! `rfd`, and the clipboard through the page, exactly as the call sites did before this file
//! existed). The liveview build that the click-through tests drive installs a
//! [`RecordingPlatform`] instead: a browser has no native dialog to click, so a test scripts the
//! answers ("the operator picks this folder", "saves to this path", "cancels") and reads back what
//! the app asked for. The call sites are one line each: `platform::current().save_file(...)`.
//!
//! `rfd` and the clipboard scripts are used here and nowhere else; a test below fails if a new
//! call site goes around this file.

use std::collections::VecDeque;
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::{Arc, Mutex, OnceLock};

use serde::{Deserialize, Serialize};

/// What every method answers with: a boxed future that need not be `Send` (the native dialogs
/// and the page-script clipboard run on the UI thread).
pub type PlatformFuture<'a, T> = Pin<Box<dyn Future<Output = T> + 'a>>;

/// A file the operator picked and the app read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PickedFile {
    pub name: String,
    pub bytes: Vec<u8>,
}

/// What came of a save dialog.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SaveOutcome {
    /// The operator chose a place and the bytes were written there.
    Saved,
    /// The operator closed the dialog. Nothing was written and nothing is wrong.
    Cancelled,
    /// The operator chose a place and the write failed.
    Failed,
}

/// The operating system, as far as the app is concerned.
pub trait Platform: Send + Sync {
    /// A folder picker. `None` when the operator cancels.
    fn pick_folder<'a>(&'a self, title: &'a str) -> PlatformFuture<'a, Option<PathBuf>>;
    /// A file picker for one kind of file, read in full. `None` when the operator cancels.
    fn pick_file<'a>(
        &'a self,
        filter_name: &'a str,
        extensions: &'a [&'a str],
    ) -> PlatformFuture<'a, Option<PickedFile>>;
    /// A file picker that answers the chosen path and reads nothing (for a field that holds a
    /// path). `None` when the operator cancels.
    fn pick_path<'a>(&'a self, title: &'a str) -> PlatformFuture<'a, Option<PathBuf>>;
    /// A save dialog that writes `content` where the operator chooses.
    fn save_file<'a>(
        &'a self,
        default_name: &'a str,
        content: &'a [u8],
    ) -> PlatformFuture<'a, SaveOutcome>;
    /// Put `text` on the system clipboard. `true` when something took it.
    fn copy_text<'a>(&'a self, text: &'a str) -> PlatformFuture<'a, bool>;
}

// ── The real thing ────────────────────────────────────────────────────────────────────────

/// The desktop app's platform: native dialogs and the webview clipboard.
#[derive(Debug, Clone, Copy, Default)]
pub struct DesktopPlatform;

impl Platform for DesktopPlatform {
    fn pick_folder<'a>(&'a self, title: &'a str) -> PlatformFuture<'a, Option<PathBuf>> {
        Box::pin(async move {
            rfd::AsyncFileDialog::new()
                .set_title(title)
                .pick_folder()
                .await
                .map(|folder| folder.path().to_path_buf())
        })
    }

    fn pick_file<'a>(
        &'a self,
        filter_name: &'a str,
        extensions: &'a [&'a str],
    ) -> PlatformFuture<'a, Option<PickedFile>> {
        Box::pin(async move {
            let file = rfd::AsyncFileDialog::new()
                .add_filter(filter_name, extensions)
                .pick_file()
                .await?;
            Some(PickedFile {
                name: file.file_name(),
                bytes: file.read().await,
            })
        })
    }

    fn pick_path<'a>(&'a self, title: &'a str) -> PlatformFuture<'a, Option<PathBuf>> {
        Box::pin(async move {
            rfd::AsyncFileDialog::new()
                .set_title(title)
                .pick_file()
                .await
                .map(|file| file.path().to_path_buf())
        })
    }

    fn save_file<'a>(
        &'a self,
        default_name: &'a str,
        content: &'a [u8],
    ) -> PlatformFuture<'a, SaveOutcome> {
        Box::pin(async move {
            let Some(file) = rfd::AsyncFileDialog::new()
                .set_file_name(default_name)
                .save_file()
                .await
            else {
                return SaveOutcome::Cancelled;
            };
            if file.write(content).await.is_ok() {
                SaveOutcome::Saved
            } else {
                SaveOutcome::Failed
            }
        })
    }

    fn copy_text<'a>(&'a self, text: &'a str) -> PlatformFuture<'a, bool> {
        Box::pin(async move {
            // The clipboard bridge's own channel when it is installed (the desktop webview cannot
            // reliably copy from page script), else the page-script fallback.
            if let Some(script) = bridge_copy_script(text) {
                if matches!(
                    dioxus::document::eval(&script).await,
                    Ok(serde_json::Value::Bool(true))
                ) {
                    return true;
                }
            }
            let Some(script) = copy_script(text) else {
                return false;
            };
            matches!(
                dioxus::document::eval(&script).await,
                Ok(serde_json::Value::Bool(true))
            )
        })
    }
}

/// The text a permalink is copied with, through the clipboard bridge's own channel when it is
/// installed. Returns the script for the bridge. Pure.
pub fn bridge_copy_script(text: &str) -> Option<String> {
    let literal = serde_json::to_string(text).ok()?;
    Some(format!(
        "if (window.__camerataClipboardBridgeSend) {{ \
           window.__camerataClipboardBridgeSend({{ op: 'copy', text: {literal} }}); return true; \
         }} return false;"
    ))
}

/// The eval statement that copies `text` to the system clipboard. The text is embedded as a JSON
/// string literal so quotes, newlines and unicode round-trip verbatim. A hidden textarea plus
/// `execCommand('copy')` is the primary path (it works in the desktop webview, where the async
/// clipboard API is not available); the async API is the fallback. Pure.
pub fn copy_script(text: &str) -> Option<String> {
    let literal = serde_json::to_string(text).ok()?;
    Some(format!(
        "var t = {literal}; \
         var a = document.createElement('textarea'); \
         a.value = t; a.setAttribute('readonly', ''); \
         a.style.position = 'fixed'; a.style.opacity = '0'; \
         document.body.appendChild(a); a.select(); \
         var ok = false; \
         try {{ ok = document.execCommand('copy'); }} catch (e) {{ ok = false; }} \
         document.body.removeChild(a); \
         if (!ok && navigator.clipboard && navigator.clipboard.writeText) {{ \
           navigator.clipboard.writeText(t); ok = true; \
         }} \
         return ok;"
    ))
}

/// A picked path as a file field holds it: relative to the repo's own folder. A path that is
/// already relative is kept. An absolute one is cut after the first folder named like the repo
/// (`acme/portal` is the folder `portal`); with no such folder it stays whole, for the reviewer to
/// fix by hand.
pub fn path_in_repo(picked: &std::path::Path, repo: &str) -> String {
    let whole = picked.to_string_lossy().into_owned();
    let folder = repo.rsplit('/').next().unwrap_or_default();
    if !picked.is_absolute() || folder.is_empty() {
        return whole;
    }
    let parts: Vec<&std::ffi::OsStr> = picked
        .components()
        .filter_map(|c| match c {
            std::path::Component::Normal(p) => Some(p),
            _ => None,
        })
        .collect();
    match parts.iter().position(|p| p.to_string_lossy() == folder) {
        Some(i) if i + 1 < parts.len() => parts[i + 1..]
            .iter()
            .map(|p| p.to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .join("/"),
        _ => whole,
    }
}

// ── Which platform is installed ───────────────────────────────────────────────────────────

static PLATFORM: OnceLock<Arc<dyn Platform>> = OnceLock::new();

/// Install the platform for this process. The liveview build calls this first thing in `main`.
/// Fails (and changes nothing) when one is already installed, which includes the default having
/// been used.
///
/// # Errors
/// The platform that was refused, handed back.
pub fn install(platform: Arc<dyn Platform>) -> Result<(), Arc<dyn Platform>> {
    PLATFORM.set(platform)
}

/// The platform this process runs on: whatever [`install`] set, else [`DesktopPlatform`].
pub fn current() -> Arc<dyn Platform> {
    PLATFORM.get_or_init(|| Arc::new(DesktopPlatform)).clone()
}

// ── The test double ───────────────────────────────────────────────────────────────────────

/// The answers a test scripts, in the order the app will ask. A dialog asked with nothing
/// scripted is cancelled, and the call is recorded all the same.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Script {
    /// Answers to `pick_folder`; `None` is "the operator cancelled".
    #[serde(default)]
    pub folders: VecDeque<Option<PathBuf>>,
    /// Answers to `pick_file`: the path of a real file to read, or `None` to cancel.
    #[serde(default)]
    pub files: VecDeque<Option<PathBuf>>,
    /// Answers to `pick_path`: the path the operator chose, or `None` to cancel.
    #[serde(default)]
    pub paths: VecDeque<Option<PathBuf>>,
    /// Answers to `save_file`: the path to write to, or `None` to cancel.
    #[serde(default)]
    pub saves: VecDeque<Option<PathBuf>>,
    /// Answers to `copy_text`; with none left the clipboard takes the text.
    #[serde(default)]
    pub clipboard: VecDeque<bool>,
}

/// One thing the app asked of the platform.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "call", rename_all = "snake_case")]
pub enum Call {
    PickFolder {
        title: String,
        answer: Option<PathBuf>,
    },
    PickFile {
        filter_name: String,
        extensions: Vec<String>,
        answer: Option<PathBuf>,
    },
    PickPath {
        title: String,
        answer: Option<PathBuf>,
    },
    SaveFile {
        default_name: String,
        bytes: usize,
        path: Option<PathBuf>,
        outcome: SaveOutcome,
    },
    CopyText {
        text: String,
        taken: bool,
    },
}

#[derive(Debug, Default)]
struct Recorded {
    script: Script,
    calls: Vec<Call>,
}

/// A platform for tests: it answers from a [`Script`] and records every call.
#[derive(Debug, Default)]
pub struct RecordingPlatform {
    inner: Mutex<Recorded>,
}

impl RecordingPlatform {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Recorded> {
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Add answers behind the ones already scripted.
    pub fn script(&self, more: Script) {
        let mut inner = self.lock();
        inner.script.folders.extend(more.folders);
        inner.script.files.extend(more.files);
        inner.script.paths.extend(more.paths);
        inner.script.saves.extend(more.saves);
        inner.script.clipboard.extend(more.clipboard);
    }

    /// Every call so far, oldest first.
    pub fn calls(&self) -> Vec<Call> {
        self.lock().calls.clone()
    }
}

impl Platform for RecordingPlatform {
    fn pick_folder<'a>(&'a self, title: &'a str) -> PlatformFuture<'a, Option<PathBuf>> {
        let mut inner = self.lock();
        let answer = inner.script.folders.pop_front().flatten();
        inner.calls.push(Call::PickFolder {
            title: title.to_string(),
            answer: answer.clone(),
        });
        Box::pin(std::future::ready(answer))
    }

    fn pick_file<'a>(
        &'a self,
        filter_name: &'a str,
        extensions: &'a [&'a str],
    ) -> PlatformFuture<'a, Option<PickedFile>> {
        let mut inner = self.lock();
        let answer = inner.script.files.pop_front().flatten();
        inner.calls.push(Call::PickFile {
            filter_name: filter_name.to_string(),
            extensions: extensions.iter().map(ToString::to_string).collect(),
            answer: answer.clone(),
        });
        let picked = answer.and_then(|path| {
            let bytes = std::fs::read(&path).ok()?;
            let name = path.file_name()?.to_string_lossy().to_string();
            Some(PickedFile { name, bytes })
        });
        Box::pin(std::future::ready(picked))
    }

    fn pick_path<'a>(&'a self, title: &'a str) -> PlatformFuture<'a, Option<PathBuf>> {
        let mut inner = self.lock();
        let answer = inner.script.paths.pop_front().flatten();
        inner.calls.push(Call::PickPath {
            title: title.to_string(),
            answer: answer.clone(),
        });
        Box::pin(std::future::ready(answer))
    }

    fn save_file<'a>(
        &'a self,
        default_name: &'a str,
        content: &'a [u8],
    ) -> PlatformFuture<'a, SaveOutcome> {
        let mut inner = self.lock();
        let path = inner.script.saves.pop_front().flatten();
        let outcome = match &path {
            None => SaveOutcome::Cancelled,
            Some(p) if std::fs::write(p, content).is_ok() => SaveOutcome::Saved,
            Some(_) => SaveOutcome::Failed,
        };
        inner.calls.push(Call::SaveFile {
            default_name: default_name.to_string(),
            bytes: content.len(),
            path,
            outcome,
        });
        Box::pin(std::future::ready(outcome))
    }

    fn copy_text<'a>(&'a self, text: &'a str) -> PlatformFuture<'a, bool> {
        let mut inner = self.lock();
        let taken = inner.script.clipboard.pop_front().unwrap_or(true);
        inner.calls.push(Call::CopyText {
            text: text.to_string(),
            taken,
        });
        Box::pin(std::future::ready(taken))
    }
}

#[cfg(test)]
mod tests {
    use std::task::{Context, Poll, Waker};

    use super::*;

    /// Run a platform future that is already ready. The recording platform never waits.
    fn ready<T>(mut f: PlatformFuture<'_, T>) -> T {
        match f.as_mut().poll(&mut Context::from_waker(Waker::noop())) {
            Poll::Ready(v) => v,
            Poll::Pending => panic!("the recording platform answered later than now"),
        }
    }

    #[test]
    fn platform_trait_stubs_dialogs_and_clipboard() {
        let dir = tempfile::tempdir().unwrap();
        let save_to = dir.path().join("export.zip");
        let import = dir.path().join("project.json");
        std::fs::write(&import, b"{\"name\":\"acme\"}").unwrap();

        let platform = RecordingPlatform::new();
        platform.script(Script {
            folders: VecDeque::from([Some(PathBuf::from("/Users/auditor/clones")), None]),
            files: VecDeque::from([Some(import.clone())]),
            paths: VecDeque::new(),
            saves: VecDeque::from([Some(save_to.clone()), None]),
            clipboard: VecDeque::from([false]),
        });

        // The folder picker answers what was scripted, then cancels once the script runs out.
        assert_eq!(
            ready(platform.pick_folder("Choose a folder")),
            Some(PathBuf::from("/Users/auditor/clones"))
        );
        assert_eq!(ready(platform.pick_folder("Choose a folder")), None);
        assert_eq!(ready(platform.pick_folder("Choose a folder")), None);

        // The file picker reads the real file it was pointed at.
        let picked = ready(platform.pick_file("JSON", &["json"])).unwrap();
        assert_eq!(picked.name, "project.json");
        assert_eq!(picked.bytes, b"{\"name\":\"acme\"}");
        assert_eq!(ready(platform.pick_file("JSON", &["json"])), None);

        // A save writes the bytes where the script said; the next one is cancelled and writes
        // nothing.
        assert_eq!(
            ready(platform.save_file("export.zip", b"PK-bytes")),
            SaveOutcome::Saved
        );
        assert_eq!(std::fs::read(&save_to).unwrap(), b"PK-bytes");
        assert_eq!(
            ready(platform.save_file("other.zip", b"x")),
            SaveOutcome::Cancelled
        );

        // The clipboard refuses once (scripted), then takes everything.
        assert!(!ready(platform.copy_text("first")));
        assert!(ready(platform.copy_text("second")));

        let calls = platform.calls();
        assert_eq!(calls.len(), 9, "{calls:?}");
        assert!(matches!(
            &calls[3],
            Call::PickFile { filter_name, extensions, .. }
                if filter_name == "JSON" && extensions == &["json".to_string()]
        ));
        assert!(matches!(
            &calls[5],
            Call::SaveFile { default_name, bytes: 8, outcome: SaveOutcome::Saved, .. }
                if default_name == "export.zip"
        ));
        assert!(matches!(
            &calls[8],
            Call::CopyText { text, taken: true } if text == "second"
        ));
    }

    #[test]
    fn a_picked_path_is_given_relative_to_the_repo_folder() {
        use std::path::Path;
        assert_eq!(
            path_in_repo(
                Path::new("/Users/auditor/clones/portal/src/orders/repo.rs"),
                "acme/portal"
            ),
            "src/orders/repo.rs"
        );
        assert_eq!(
            path_in_repo(Path::new("src/orders/repo.rs"), "acme/portal"),
            "src/orders/repo.rs",
            "a relative path is kept"
        );
        assert_eq!(
            path_in_repo(Path::new("/Users/auditor/elsewhere/repo.rs"), "acme/portal"),
            "/Users/auditor/elsewhere/repo.rs",
            "no folder named like the repo: kept whole"
        );
        assert_eq!(
            path_in_repo(Path::new("/Users/auditor/portal"), "acme/portal"),
            "/Users/auditor/portal"
        );
        assert_eq!(
            path_in_repo(Path::new("/a/portal/x.rs"), ""),
            "/a/portal/x.rs"
        );
    }

    #[test]
    fn pick_path_answers_the_scripted_path_and_cancels_when_the_script_runs_out() {
        let platform = RecordingPlatform::new();
        platform.script(Script {
            paths: VecDeque::from([Some(PathBuf::from("/Users/auditor/portal/a.rs"))]),
            ..Script::default()
        });
        assert_eq!(
            ready(platform.pick_path("Choose the file")),
            Some(PathBuf::from("/Users/auditor/portal/a.rs"))
        );
        assert_eq!(ready(platform.pick_path("Choose the file")), None);
        assert!(matches!(
            &platform.calls()[0],
            Call::PickPath { title, answer: Some(_) } if title == "Choose the file"
        ));
    }

    #[test]
    fn a_script_round_trips_through_json_so_a_test_can_post_it() {
        let script = Script {
            folders: VecDeque::from([Some(PathBuf::from("/Users/auditor/a")), None]),
            saves: VecDeque::from([Some(PathBuf::from("/Users/auditor/out.zip"))]),
            clipboard: VecDeque::from([true, false]),
            ..Script::default()
        };
        let json = serde_json::to_string(&script).unwrap();
        assert_eq!(serde_json::from_str::<Script>(&json).unwrap(), script);
        assert!(
            serde_json::from_str::<Script>("{\"fodlers\":[]}").is_err(),
            "a misspelt key is refused, not ignored"
        );
    }

    #[test]
    fn the_clipboard_scripts_escape_the_text_as_one_literal() {
        let script =
            bridge_copy_script("acme/app@abc1234:src/a \"b\".rs:3\nhttps://x/y#L3").unwrap();
        assert!(script.contains("window.__camerataClipboardBridgeSend"));
        assert!(script.contains("\\n"), "{script}");
        assert!(script.contains("\\\"b\\\""), "{script}");
        let copy = copy_script("line1\n\"quoted\" </script>").unwrap();
        assert!(copy.contains("execCommand('copy')"));
        assert!(copy.contains("\\n"), "{copy}");
    }

    /// The rule the file's header states: native dialogs and clipboard scripts live here and
    /// nowhere else, so a host closure can always be driven with the recording platform.
    #[test]
    fn no_dialog_or_clipboard_call_goes_around_the_platform() {
        fn walk(dir: &std::path::Path, hits: &mut Vec<String>) {
            for entry in std::fs::read_dir(dir).unwrap().flatten() {
                let path = entry.path();
                if path.is_dir() {
                    walk(&path, hits);
                    continue;
                }
                let name = path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or_default();
                if path.extension().and_then(|e| e.to_str()) != Some("rs") || name == "platform.rs"
                {
                    continue;
                }
                // The two files that legitimately drive the webview's own editing commands: the
                // native Cmd C / Cmd X / Cmd A bridge and the clipboard probe. Neither is a
                // page-script copy; any other file calling `execCommand` in any quoting is a
                // bypass of the Platform.
                let may_edit = matches!(name, "desktop_chrome.rs" | "clipboard_probe.rs");
                let text = std::fs::read_to_string(&path).unwrap();
                for (n, line) in text.lines().enumerate() {
                    let code = line.trim_start();
                    if code.starts_with("//") {
                        continue;
                    }
                    if code.contains("rfd::")
                        || (!may_edit && code.contains("execCommand("))
                        || code.contains("clipboard.writeText")
                    {
                        hits.push(format!("{}:{}: {code}", path.display(), n + 1));
                    }
                }
            }
        }
        let mut hits = Vec::new();
        walk(
            &std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src"),
            &mut hits,
        );
        assert!(
            hits.is_empty(),
            "go through crate::platform instead:\n{}",
            hits.join("\n")
        );
    }
}
