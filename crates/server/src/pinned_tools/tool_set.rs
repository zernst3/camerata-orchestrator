//! The tool set an inspection needs, its state on this machine, and provisioning it.
//!
//! One computation serves every caller: `camerata inspect` (the real run and `--plan-only`) and
//! the app's pre-flight call [`required_tool_set`] then [`tool_set_report`] or
//! [`provision_tool_set`], so what the operator is shown before the start is what the run
//! provisions and then runs.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::fmt::Write as _;
use std::path::PathBuf;

use super::pins::{self, Artifact, SEMGREP_VERSION};
use super::{semgrep, ToolEnv};
use crate::scan_tools::ScanTool;
use crate::tool_provisioning::{self, ProvisionError};

/// What an inspection runs: the preview scan tools its selected rules route to on the stacks it
/// detected, and whether the dependency audit runs.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RequiredTools {
    pub scan_tools: BTreeSet<ScanTool>,
    pub dep_audit: bool,
}

/// One thing Camerata installs (or, for clippy, one thing it does not).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Component {
    Ruff,
    Node,
    EslintPackages,
    Python,
    SemgrepPackages,
    OsvScanner,
    /// `cargo clippy`: built with the repository's own Rust toolchain from this machine.
    /// Camerata does not provision a Rust toolchain (an open question on #587).
    RustToolchain,
}

impl Component {
    pub fn name(self) -> &'static str {
        match self {
            Component::Ruff => "ruff",
            Component::Node => "node",
            Component::EslintPackages => "eslint",
            Component::Python => "python",
            Component::SemgrepPackages => "semgrep",
            Component::OsvScanner => "osv-scanner",
            Component::RustToolchain => "cargo clippy",
        }
    }

    pub fn version(self) -> &'static str {
        match self {
            Component::Ruff => pins::RUFF_VERSION,
            Component::Node => pins::NODE_VERSION,
            Component::EslintPackages => tool_provisioning::ESLINT_VERSION,
            Component::Python => pins::PYTHON_VERSION,
            Component::SemgrepPackages => SEMGREP_VERSION,
            Component::OsvScanner => pins::OSV_SCANNER_VERSION,
            Component::RustToolchain => "the machine's toolchain",
        }
    }
}

/// The components a tool needs, runtime first.
fn components_for(tool: ScanTool) -> &'static [Component] {
    match tool {
        ScanTool::Ruff => &[Component::Ruff],
        ScanTool::Eslint => &[Component::Node, Component::EslintPackages],
        ScanTool::Semgrep => &[Component::Python, Component::SemgrepPackages],
        ScanTool::Clippy => &[Component::RustToolchain],
    }
}

/// A component's state on this machine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ComponentState {
    /// Installed from its pin.
    Ready,
    /// Not installed yet; provisioning will download it.
    WillInstall,
    /// Camerata has no build of it for this machine. A client inspection that needs it is
    /// refused before it starts.
    CannotProvision(String),
    /// Not provisioned by Camerata at all (clippy). Shown so the list is complete; never a
    /// refusal, because it is the one spawn #587 left on the machine's own toolchain.
    NotProvisioned(String),
}

/// One line of the tool list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolSetEntry {
    pub component: Component,
    /// The scan tools it is needed for ("eslint", "osv-scanner", ...).
    pub needed_for: Vec<&'static str>,
    pub state: ComponentState,
}

/// The tool list: every component the inspection needs and its state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolSetReport {
    pub tooling: PathBuf,
    pub entries: Vec<ToolSetEntry>,
}

impl ToolSetReport {
    /// The entries that cannot be provisioned on this machine.
    pub fn cannot_provision(&self) -> Vec<&ToolSetEntry> {
        self.entries
            .iter()
            .filter(|e| matches!(e.state, ComponentState::CannotProvision(_)))
            .collect()
    }

    /// The entries provisioning will download.
    pub fn will_install(&self) -> Vec<&ToolSetEntry> {
        self.entries
            .iter()
            .filter(|e| e.state == ComponentState::WillInstall)
            .collect()
    }

    /// The operator's text: one line per component, then the folder.
    pub fn render(&self) -> String {
        let mut out = String::new();
        if self.entries.is_empty() {
            out.push_str("tools: none (no scan tool runs in this inspection)\n");
            return out;
        }
        for e in &self.entries {
            let state = match &e.state {
                ComponentState::Ready => "ready".to_string(),
                ComponentState::WillInstall => "will be installed".to_string(),
                ComponentState::CannotProvision(why) => format!("cannot be provisioned: {why}"),
                ComponentState::NotProvisioned(why) => format!("not provisioned: {why}"),
            };
            let _ = writeln!(
                out,
                "  {} {} (for {}): {state}",
                e.component.name(),
                e.component.version(),
                e.needed_for.join(" and "),
            );
        }
        let _ = writeln!(out, "  tooling folder: {}", self.tooling.display());
        out
    }
}

/// Why an inspection's tool set could not be provisioned.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolSetRefusal {
    pub component: Component,
    pub reason: String,
    pub report: ToolSetReport,
}

impl std::fmt::Display for ToolSetRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} {} could not be provisioned: {}",
            self.component.name(),
            self.component.version(),
            self.reason
        )
    }
}

/// The tool set an inspection needs, from the rules it previews and the stacks of its
/// repositories. Mirrors `merge_scan_preview` exactly: per repository, the preview rules that
/// apply to it, gated by the languages its admitted files are in
/// ([`crate::scan_tools::group_by_tool`]). Reads the repositories' file lists (no tool runs, no
/// network).
pub fn required_tool_set(
    sources: &[(String, PathBuf)],
    preview_rules: &[crate::onboard::SelectedRule],
    corpus: Option<&camerata_rules::RuleSet>,
    scope: &crate::onboard::scope::ScanScope,
    dep_audit: bool,
) -> RequiredTools {
    let mut required = RequiredTools {
        scan_tools: BTreeSet::new(),
        dep_audit: dep_audit && !sources.is_empty(),
    };
    let Some(set) = corpus else {
        return required;
    };
    if preview_rules.is_empty() {
        return required;
    }
    let lookup = |id: &str| set.get_by_id(id);
    for (spec, dir) in sources {
        let for_repo: Vec<crate::onboard::SelectedRule> = preview_rules
            .iter()
            .filter(|r| r.applies_to(spec))
            .cloned()
            .collect();
        if for_repo.is_empty() {
            continue;
        }
        let languages: HashSet<String> =
            match crate::onboard::files::read_local_repo_files(dir, &scope.for_repo(spec)) {
                Ok(ex) => crate::scan_tools::languages_from_files(&ex.files),
                Err(_) => HashSet::new(),
            };
        let (by_tool, _) = crate::scan_tools::group_by_tool(&for_repo, &lookup, Some(&languages));
        required.scan_tools.extend(by_tool.keys().copied());
    }
    required
}

/// Every component `required` needs, each with the tools it is for, in a stable order.
fn components(required: &RequiredTools) -> BTreeMap<Component, Vec<&'static str>> {
    let mut out: BTreeMap<Component, Vec<&'static str>> = BTreeMap::new();
    for tool in &required.scan_tools {
        for c in components_for(*tool) {
            out.entry(*c).or_default().push(tool.name());
        }
    }
    if required.dep_audit {
        out.entry(Component::OsvScanner)
            .or_default()
            .push("osv-scanner");
    }
    out
}

fn artifact_state(env: &ToolEnv, artifact: Artifact) -> ComponentState {
    let pins = env.pins(artifact);
    let Some(platform) = env.platform else {
        return ComponentState::CannotProvision(super::platform::unsupported_platform_reason());
    };
    if pins.for_platform(platform).is_none() {
        return ComponentState::CannotProvision(format!(
            "{} {} publishes no build for {}",
            pins.name,
            pins.version,
            platform.label()
        ));
    }
    if env.is_installed(artifact) {
        ComponentState::Ready
    } else {
        ComponentState::WillInstall
    }
}

fn component_state(env: &ToolEnv, c: Component) -> ComponentState {
    match c {
        Component::Ruff => artifact_state(env, Artifact::Ruff),
        Component::Node => artifact_state(env, Artifact::Node),
        Component::Python => artifact_state(env, Artifact::Python),
        Component::OsvScanner => artifact_state(env, Artifact::OsvScanner),
        Component::EslintPackages => match artifact_state(env, Artifact::Node) {
            ComponentState::CannotProvision(why) => {
                ComponentState::CannotProvision(format!("it runs on Node.js, and {why}"))
            }
            _ if env.is_installed(Artifact::Node)
                && tool_provisioning::eslint_workspace_is_current(
                    &tool_provisioning::eslint_workspace_dir(&env.tooling),
                ) =>
            {
                ComponentState::Ready
            }
            _ => ComponentState::WillInstall,
        },
        Component::SemgrepPackages => match semgrep::semgrep_unavailable_reason(env) {
            Some(why) => ComponentState::CannotProvision(why),
            None if semgrep::semgrep_is_installed(env) => ComponentState::Ready,
            None => ComponentState::WillInstall,
        },
        Component::RustToolchain => ComponentState::NotProvisioned(
            "cargo clippy builds the repository with this machine's own Rust toolchain".to_string(),
        ),
    }
}

/// The state of every component `required` needs. Reads the disk only: no download, no install,
/// no process. This is what `--plan-only` and the pre-flight show.
pub fn tool_set_report(env: &ToolEnv, required: &RequiredTools) -> ToolSetReport {
    ToolSetReport {
        tooling: env.tooling.clone(),
        entries: components(required)
            .into_iter()
            .map(|(component, needed_for)| ToolSetEntry {
                component,
                needed_for,
                state: component_state(env, component),
            })
            .collect(),
    }
}

async fn provision_component(env: &ToolEnv, c: Component) -> Result<(), ProvisionError> {
    match c {
        Component::Ruff => env.ensure(Artifact::Ruff).await.map(drop),
        Component::Node => env.ensure(Artifact::Node).await.map(drop),
        Component::Python => env.ensure(Artifact::Python).await.map(drop),
        Component::OsvScanner => env.ensure(Artifact::OsvScanner).await.map(drop),
        Component::EslintPackages => {
            let node = env.ensure_node().await?;
            tool_provisioning::ensure_eslint_with(&env.tooling, &env.npm_command(&node))
                .await
                .map(drop)
        }
        Component::SemgrepPackages => semgrep::ensure_semgrep(env).await.map(drop),
        Component::RustToolchain => Ok(()),
    }
}

/// Provision every component `required` needs, or refuse.
///
/// Called when an inspection is triggered, before its first model call.
///
/// - A component Camerata has no build of for this machine refuses a CLIENT inspection before
///   anything is installed. An internal inspection goes ahead without it: the list already said
///   it cannot be provisioned, and the tool's pass is disclosed as not run.
/// - Every other missing component is installed in order (a runtime before the tool that runs on
///   it). The first one that fails (a download error, a checksum that does not match, an install
///   step that fails) refuses the inspection, client or internal: the owner's rule is that every
///   tool a scan needs is there when the scan starts.
/// - The one exception is the test-process guard ([`ProvisionError::RealToolingProtected`]): a
///   cargo test binary may not install into the real per-user tooling folder, so the component
///   is left as it is and the scan step discloses it, as it did before #587. It cannot occur in
///   the product.
///
/// On success every installable component is ready and the returned report says so.
pub async fn provision_tool_set(
    env: &ToolEnv,
    required: &RequiredTools,
    client_engagement: bool,
) -> Result<ToolSetReport, ToolSetRefusal> {
    let before = tool_set_report(env, required);
    if client_engagement {
        if let Some(entry) = before.cannot_provision().first() {
            let ComponentState::CannotProvision(why) = &entry.state else {
                unreachable!("cannot_provision returns only CannotProvision entries");
            };
            return Err(ToolSetRefusal {
                component: entry.component,
                reason: why.clone(),
                report: before.clone(),
            });
        }
    }
    for entry in &before.entries {
        if entry.state != ComponentState::WillInstall {
            continue;
        }
        match provision_component(env, entry.component).await {
            Ok(()) | Err(ProvisionError::RealToolingProtected { .. }) => {}
            Err(e) => {
                return Err(ToolSetRefusal {
                    component: entry.component,
                    reason: e.to_string(),
                    report: tool_set_report(env, required),
                });
            }
        }
    }
    Ok(tool_set_report(env, required))
}
