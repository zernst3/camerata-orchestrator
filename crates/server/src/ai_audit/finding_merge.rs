//! Ranking, classification, deduplication and the same-cause merge of findings from the scan tiers.

use super::*;

/// Severity rank for keeping the most-severe representative when merging duplicates.
pub(crate) fn severity_rank(s: &str) -> u8 {
    match s {
        "critical" => 4,
        "high" => 3,
        "medium" => 2,
        "low" => 1,
        _ => 0,
    }
}

// ── P1: finding_class (Security vs Hygiene) + shared primary-selection ranking ───────────────
//
// the codebase-inspection hardening notes, P1. The regression this section
// exists to prevent: a low structural/style row (e.g. an `ARCH-MIDDLEWARE-FIRST-1`-shaped
// layering rule) sitting at the SAME code location or defect cluster as a genuine security
// finding (e.g. a reflected-Origin CORS-with-credentials misconfiguration) must never win
// primary and hide the security finding. Before this section, BOTH merge passes
// (`merge_location_group`'s exact-location collapse and `merge_semantic_group`'s cross-family/
// cross-file collapse) picked a primary using ONLY "adopted-vs-invented" / origin-tier +
// severity — with no notion of WHAT KIND of defect a finding is. A deterministic, non-`AI-`
// rule id (any adopted corpus/floor rule, regardless of topic) always outranked an AI-invented
// security finding on that axis alone, independent of severity. `finding_class` closes that gap
// by making the SECURITY/HYGIENE split the first, highest-priority key in the shared
// `primary_rank` both passes now use.

/// Which side of the security/hygiene split a finding falls on, for cross-tier merge primacy
/// (design point 3): Security beats Hygiene UNCONDITIONALLY, ahead of severity/confidence/
/// specificity — a structural/style row must never absorb and hide a security row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FindingClass {
    Security,
    Hygiene,
}

/// Rule-id tokens that mark a finding as `FindingClass::Security` for merge-primacy purposes:
/// secrets/credentials, RLS/access policy, authn/authz, injection (SQL/XSS/SSRF/deserialization/
/// RCE), transport security (TLS/CORS), resource exposure, open redirect, weak crypto/
/// randomness, and CSRF concerns — exactly the families
/// the codebase-inspection hardening notes P1 names as Security. This is a
/// CONTENT scan over the rule id's hyphen-delimited WORDS ([`security_token_matches`]), not a
/// prefix rule: `SEC-*`/`SUPABASE-*` deterministic floor rules match via their own prefix
/// shortcut below (belt-and-suspenders — every current one also matches a token), but so do
/// `ARCH-*`/`AI-*` ids that happen to encode a security concern (`ARCH-NO-SECRETS-IN-URL-1`,
/// `ARCH-FETCH-THEN-AUTHORIZE-1`, an invented `AI-CORS-...` finding) — the corpus does NOT
/// reserve security naming to one prefix family, so prefix-only matching would misclassify real
/// cases in both directions.
///
/// Word-boundary matched (not raw substring), on purpose: a naive `rule_id.contains(token)` scan
/// was cross-checked against every rule id in `crates/rules/principles/**` and produced real
/// false positives — `"RCE"` (meant for remote-code-execution) matched inside `"RESOURCE"` and
/// `"INTERCEPTORS"` (`ARCH-RESOURCE-LIFECYCLE-1`, a subprocess/temp-file cleanup rule;
/// `JAVA-RESOURCE-MANAGEMENT-1`, a try-with-resources rule; two interceptor-pattern rules), none
/// of which are security. `security_token_matches` requires a token to match a WHOLE
/// hyphen-delimited word (or be a prefix of one, so `"DESERIAL"` still matches
/// `"DESERIALIZATION"`), which eliminates that whole class of accidental substring hits while
/// keeping every genuine match (`"RLS"` as its own word in `SUPABASE-RLS-ENABLED-1`, `"AUTH"` as
/// a prefix of `"AUTHORIZE"`/`"AUTHZ"`).
///
/// Two more corpus-verified removals from an earlier draft of this list, kept out for the same
/// reason: bare `"INJECT"` — every CURRENT corpus id containing an "inject"-rooted word
/// (`CSHARP-DEPENDENCY-INJECTION-CONSTRUCTOR-1`, `JAVA-SPRING-CONSTRUCTOR-INJECTION-1`,
/// `JAVASCRIPT-ANGULAR-DI-CONSTRUCTOR-OR-INJECT-1`) is a DEPENDENCY-injection pattern rule, not
/// an injection-VULNERABILITY rule; real SQL-injection rules are already caught via `"SQL"`.
/// Bare `"SESSION"` — the one hit (`PYTHON-FASTAPI-DI-SESSION-1`) is a DATABASE-session
/// lifecycle rule, not an authentication session; real auth-session rules are already caught via
/// `"AUTH"` (`SUPABASE-AUTH-GETSESSION-SERVER-1`). Both words are common enough in non-security
/// naming that keeping them would re-introduce the fail-safe-direction violation this whole
/// section exists to prevent: a Hygiene finding mislabeled Security can still beat a GENUINE
/// Security finding it clusters with, via the severity/confidence/specificity tiebreaks, since
/// both would tie at `class_rank` — so a wrong Security label is not harmless.
///
/// Deliberately excludes the bare word "BYPASS": the corpus's OWN `categorize_rule_id`
/// authorization bucket treats bare "BYPASS" as an authz signal, which over-fires on a purely
/// architectural finding like "the handler bypasses the repository layer" (no auth concept at
/// all). Every genuine auth-bypass rule name already carries a more specific token this list
/// covers directly (`AUTH`, `RBAC`, `ACCESS-CONTROL`, `SERVICE-ROLE`, `PERMISSION`), so dropping
/// the bare word loses no real coverage while removing that false-positive source from the
/// rule-id scan. `finding_class` still consults `category`, so a finding whose `category` was
/// explicitly backfilled to `"authorization"` via that same bare-"BYPASS" heuristic can still be
/// classified Security through that path — a known, narrow residual imprecision inherited from
/// `categorize_rule_id`, not from this list; flagged for review rather than silently accepted.
#[rustfmt::skip]
pub(super) const SECURITY_RULE_TOKENS: &[&str] = &[
    // Secrets / credentials
    "SECRET", "CREDENTIAL", "PASSWORD", "API-KEY", "APIKEY", "PRIVATE-KEY", "HARDCODED",
    // RLS / access policy
    "RLS", "ROW-LEVEL", "POLICY", "SEARCH-PATH",
    // Authn / authz
    "AUTH", "RBAC", "PERMISSION", "ACCESS-CONTROL", "SERVICE-ROLE", "LOGIN", "JWT", "CSRF",
    // Injection. Deliberately NOT bare "SQL": that matched every SQL-adjacent rule regardless
    // of topic (indexing, N+1, connection pooling, migrations-checked-in, `SQLX` itself via
    // prefix matching), none of which are injection concerns. The specific SQL-injection
    // shapes the corpus actually uses are "raw SQL" / "string SQL" / "parameterized" (its
    // ABSENCE is the vulnerability) — each is its own token below, plus explicit
    // "*-INJECTION" compounds for command/code/SQL injection so a future/invented id like
    // `AI-SQL-INJECTION` still matches without bare "INJECT" reintroducing the
    // dependency-injection false positive documented above.
    "RAW-SQL", "STRING-SQL", "PARAMETERIZED", "SQL-INJECTION", "COMMAND-INJECTION",
    "CODE-INJECTION", "XSS", "SSRF", "DESERIAL", "RCE",
    // Transport security / resource exposure
    "TLS", "SSL", "HTTPS", "CERT", "CORS", "EXPOSE", "EXPOSURE", "EXPOSED", "PUBLIC-BUCKET",
    // Redirect / crypto / tokens
    "REDIRECT", "CRYPTO", "RANDOM", "NONCE", "TOKEN",
    // Object-level access, path handling and privilege (H5): ids the model invents for these
    // classes ("AI-IDOR-INVOICES", "AI-PATH-TRAVERSAL-DOWNLOAD") carry none of the tokens above.
    "IDOR", "TRAVERSAL", "MASS-ASSIGNMENT", "UNAUTHENTICATED", "PRIVILEGE",
    // Explicit "security" naming (CI security-scan rules, security-headers/method-security
    // rules) — a rule that names itself "security" in its own id is Security almost by
    // definition, and this token catches `CICD-*-SECURITY-SCAN-1`,
    // `JAVASCRIPT-EXPRESS-SECURITY-HEADERS-1`, `JAVA-SPRING-METHOD-SECURITY-1`, none of which
    // any other token above reaches.
    "SECURITY",
];

/// True when `token` matches one of `id`'s hyphen-delimited WORDS — either exactly, or as a
/// prefix of that word (so a fragment like `"DESERIAL"` still matches the word
/// `"DESERIALIZATION"`). A single-word token (`"AUTH"`) is checked against each word of `id`; a
/// token that is ITSELF hyphenated (`"API-KEY"`, `"PRIVATE-KEY"`) is checked against every
/// CONSECUTIVE run of `id`'s words of the same length, so `"PRIVATE-KEY"` matches
/// `SEC-NO-PRIVATE-KEY-1`'s `["PRIVATE", "KEY"]` run without matching `"PRIVATE"` or `"KEY"`
/// alone elsewhere. Deliberately NOT a raw substring scan — see `SECURITY_RULE_TOKENS`'s doc
/// comment for the false positives (`"RCE"` inside `"RESOURCE"`) that motivated this.
pub(super) fn security_token_matches(id_words: &[&str], token: &str) -> bool {
    let token_words: Vec<&str> = token.split('-').collect();
    if token_words.len() == 1 {
        let t = token_words[0];
        id_words.iter().any(|w| w.starts_with(t))
    } else {
        id_words
            .windows(token_words.len())
            .any(|window| window == token_words.as_slice())
    }
}

/// Semantic categories (the closed taxonomy backfilled by `categorize_rule_id` — see
/// `KNOWN_CATEGORIES`) that are themselves Security families. Consulted as a SECOND signal
/// (after the rule-id token scan) so a finding classified by calibration's `category` field
/// rather than a security-sounding rule id (an opaque invented AI- name, say) still lands on
/// the right side of the split.
pub(super) fn category_is_security(category: &str) -> bool {
    matches!(
        category,
        "authorization"
            | "authentication"
            | "secret-exposure"
            | "injection"
            | "transport-security"
            | "rls-policy"
            | "resource-exposure"
            | "input-validation"
    )
}

/// Classify a finding as Security or Hygiene for merge-primacy purposes (design point 1). Layered,
/// most-specific signal first: (1) the `SEC-*`/`SUPABASE-*` prefix families — every rule in
/// either family is security-scoped by the corpus's own domain convention (verified against the
/// full corpus: all 8 `SEC-*` and all 17 `SUPABASE-*` ids are security), so this is a cheap,
/// maintenance-free net that also future-proofs a new rule added to either family without an
/// obviously security-sounding word in its id; (2) a security-sounding rule id
/// (`SECURITY_RULE_TOKENS`, word-boundary matched — see its doc comment), which is what carries
/// `ARCH-*`/`AI-*` ids that happen to encode a security concern; (3) an already-assigned security
/// category; (4) Hygiene by default — an unclassified or genuinely structural/style/testing/
/// process finding never wins primacy over a KNOWN security finding it happens to cluster with,
/// which is the fail-safe direction (never silently promote noise to Security; a real security
/// finding is caught by (1)-(3) instead). Kept in this ONE function, with the rationale above,
/// per the design's "keep the classification in one place" requirement — every merge-primacy
/// call site derives class through this function, never by re-deriving its own notion of "is
/// this security".
pub(crate) fn finding_class(f: &Finding) -> FindingClass {
    let id = f.rule_id.to_ascii_uppercase();
    if id.starts_with("SEC-") || id.starts_with("SUPABASE-") {
        return FindingClass::Security;
    }
    let id_words: Vec<&str> = id.split('-').collect();
    if SECURITY_RULE_TOKENS
        .iter()
        .any(|t| security_token_matches(&id_words, t))
    {
        return FindingClass::Security;
    }
    if f.category.as_deref().is_some_and(category_is_security) {
        return FindingClass::Security;
    }
    FindingClass::Hygiene
}

/// Rank for `finding_class`: Security strictly outranks Hygiene.
pub(crate) fn class_rank(c: FindingClass) -> u8 {
    match c {
        FindingClass::Security => 1,
        FindingClass::Hygiene => 0,
    }
}

/// Rank for a finding's calibrated confidence, for the THIRD primary-selection key (design
/// point 3: class, then severity, then confidence, then specificity). `"needs-review"` is the
/// ONLY value calibration uses to flag a debatable/under-evidenced finding, so it alone ranks
/// low; a clear `"high"` verdict and `None` (no calibrated opinion at all — the deterministic
/// floor/checkers, which are exact by construction and are never run through calibration) are
/// BOTH "not flagged as debatable" and rank equally above it. This keeps a deterministic
/// finding's exact line as the tiebreak winner (via `origin_rank`, the next key) in a same-class,
/// same-severity tie against an AI "high"-confidence finding, preserving the pre-existing
/// deterministic-anchor behavior the D1/D2/D7 line-grading tests depend on, while still letting a
/// "needs-review"-flagged finding lose to a clearer one of the same class and severity.
pub(crate) fn confidence_rank(c: Option<&str>) -> u8 {
    match c {
        Some("needs-review") => 0,
        _ => 1,
    }
}

/// W7: whether a finding is an ACTIONABLE statement (a `Defect`, a `Deviation`, or a legacy
/// row whose producer never assigned a kind — which the exporters surface as `unspecified`
/// and which is overwhelmingly the defect-shaped legacy population) as opposed to an
/// INFORMATIONAL one (`ConventionObserved` / `NoConvention` / `NotAssessed`). `1` for
/// actionable, `0` for informational. The FIRST key of [`primary_rank`], so an informational
/// row can never win primacy over a defect it co-grouped with, whatever its origin/severity —
/// and the discriminant [`kinds_merge_compatible`] uses to stop such a group forming at all.
pub(crate) fn kind_actionability_rank(f: &Finding) -> u8 {
    match &f.kind {
        Some(k) if k.is_informational() => 0,
        _ => 1,
    }
}

/// Whether a row is HELD for review rather than asserted: the structured flag, or the
/// calibration/adapter `needs-review` confidence (the two are a biconditional by construction,
/// and either alone is enough to hold the row — see `report_export::is_hedged`).
pub(crate) fn is_held_row(f: &Finding) -> bool {
    f.needs_review || f.confidence.as_deref() == Some("needs-review")
}

/// Rank for [`is_held_row`]: an asserted row (`1`) strictly outranks a held one (`0`).
pub(crate) fn asserted_rank(f: &Finding) -> u8 {
    u8::from(!is_held_row(f))
}

/// W7: whether two findings make MERGE-COMPATIBLE kinds of statement. A merge that absorbs a
/// `Defect` into a `ConventionObserved` primary — or vice versa — is a correctness bug with a
/// name now that `FindingKind` exists: the absorbed row's statement (there IS a fault / the
/// codebase consistently does X) disappears from the report entirely. The rule:
///
/// - an informational row never merges with an actionable one, in either direction;
/// - two informational rows merge only when they are the SAME rule making the SAME kind of
///   statement (informational rows are one-per-rule by construction — A5.3/A5.5 — so
///   anything else fusing is two different statements collapsing);
/// - a HELD (needs-review) row never merges with an asserted one, in either direction: the two
///   make different statements (a fact the checker stands behind, and one it only flags for a
///   human), and fusing them either hides the asserted defect beneath a held primary or
///   launders the held one into an assertion. Firebase review, 2026-10-25 (owner call: an
///   asserted finding is never fused into, or ranked beneath, a held row);
/// - two actionable rows (Defect/Deviation/legacy-unspecified) otherwise merge exactly as before.
pub(crate) fn kinds_merge_compatible(a: &Finding, b: &Finding) -> bool {
    if is_held_row(a) != is_held_row(b) {
        return false;
    }
    let a_informational = kind_actionability_rank(a) == 0;
    let b_informational = kind_actionability_rank(b) == 0;
    if a_informational != b_informational {
        return false;
    }
    if a_informational {
        use camerata_api_types::audit::FindingKind;
        return a.rule_id == b.rule_id
            && FindingKind::tag_or_unspecified(a.kind.as_ref())
                == FindingKind::tag_or_unspecified(b.kind.as_ref());
    }
    true
}

/// The shared primary-selection ranking for BOTH merge passes (design point 3, reordered by
/// C5-1 — see the codebase-inspection hardening notes P1 for the original and
/// the REG-1/REG-2 work item for this reordering). Higher tuples win via `max_by_key`. Order:
/// (0) W7: kind actionability ([`kind_actionability_rank`]) — an actionable row
/// (Defect/Deviation/legacy) beats an informational one unconditionally, so a defect can
/// never be hidden under a `ConventionObserved`/`NoConvention`/`NotAssessed` primary (the
/// [`kinds_merge_compatible`] guards should keep such a group from ever forming; this key is
/// the belt-and-braces if one does);
/// (0b) an ASSERTED row ([`asserted_rank`]) beats a held (needs-review) one, so an asserted
/// defect is never ranked beneath a held row (same belt-and-braces role as (0));
/// (1) `FindingClass` — Security beats Hygiene unconditionally, independent of everything else,
/// so a structural/style row can never absorb and hide a security defect (the P1 regression);
/// (2) rule specificity via `origin_rank` — Deterministic beats AdoptedAi beats InventedAi.
/// C5-1 moved this key AHEAD of severity: before, an AI-tier finding's uncalibrated, routinely
/// inflated severity could outrank its own deterministic sibling describing the SAME defect at
/// the SAME site (the REG-1 symptom — a correctly-graded, high-confidence, authored-fix
/// deterministic finding gets demoted to `also_matches` behind a hedged, fix-less, inflated AI
/// guess). Origin now decides BEFORE severity within a class, so a deterministic finding always
/// wins primacy over an AI one of the same class, whatever either side's own severity says —
/// but class is still checked FIRST, so a genuine security finding (even AI-tier) still beats an
/// irrelevant deterministic structural row, preserving the P1 invariant; (3) calibrated severity
/// (now only a tiebreak within one class+origin tier — e.g. two deterministic members, or two AI
/// members); (4) confidence (a "needs-review" flag ranks last); (5) earliest appearance, the
/// final deterministic tiebreak so output never depends on hash/iteration order.
pub(crate) fn primary_rank(
    f: &Finding,
    group_len: usize,
    index: usize,
) -> (u8, u8, u8, u8, u8, u8, u8, usize) {
    (
        kind_actionability_rank(f),
        // Firebase review: an asserted row is never ranked beneath a held one, whatever the
        // class — see `kinds_merge_compatible`, which keeps such a group from forming at all.
        asserted_rank(f),
        // A row about a real secret leads a row about a placeholder (smoke run: an
        // `.env.example` placeholder took the headline and absorbed the real `.env.production`
        // key). Ahead of class and origin, which two secret rows share anyway.
        u8::from(!is_placeholder_secret_row(f)),
        class_rank(finding_class(f)),
        origin_rank(finding_origin(f)),
        severity_rank(&f.severity),
        confidence_rank(f.confidence.as_deref()),
        // Larger for earlier findings (lower `index`), so `max_by_key` resolves ties toward the
        // EARLIEST appearance — see BUG-7's original comment on this idiom.
        group_len - index,
    )
}

/// The severity a row about a placeholder is held at: a committed example value is worth a row
/// but is not a live credential.
pub(super) const PLACEHOLDER_SEVERITY_CEILING: &str = "low";

/// The floor rule about committed secret-bearing files (`.env.production`, key files): a secret
/// rule with no value shape of its own.
pub(super) const SECRET_FILE_RULE: &str = "SEC-NO-SECRET-FILE-1";

/// Whether `rule_id` is one of the secret rules: a rule the secrets crate detects values for, or
/// the secret-file rule. NOT every `SEC-` rule: the prefix also covers injection, transport and
/// sink rules, which an example file name or a template token says nothing about.
pub(crate) fn is_secret_rule(rule_id: &str) -> bool {
    rule_id == SECRET_FILE_RULE || !camerata_secrets::shapes_for_rule(rule_id).is_empty()
}

/// Whether `path` names an example or sample file: `.env.example`, `.env.sample` or anything
/// ending `.example`. Deliberately narrow: a `templates/prod.env` or `config.template.ts` may be
/// the file a deployment actually loads, so a name alone does not make it an example.
pub(super) fn is_example_file_path(path: &str) -> bool {
    let name = path
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(path)
        .to_ascii_lowercase();
    name == ".env.example" || name == ".env.sample" || name.ends_with(".example")
}

/// Whether `value` (quotes and whitespace trimmed) is one of the literal weak words a default
/// configuration ships with (`password`, `changeme`, `secret`, `token`). These are real default
/// credentials an attacker tries first, not unfilled templates: they are placeholders only inside
/// an example file, which [`is_example_file_path`] already covers.
pub(super) fn is_weak_default_word(value: &str) -> bool {
    let value = value
        .trim()
        .trim_matches(|c: char| matches!(c, '"' | '\'' | '`'));
    [
        "password",
        "secret",
        "token",
        "changeme",
        "change_me",
        "change-me",
    ]
    .iter()
    .any(|word| value.eq_ignore_ascii_case(word))
}

/// Whether one assigned value (the text after `=` or `:` on a line) is wholly an unfilled template
/// token and so leaks no credential: empty, a closed template (`${VAR}`, `{{ var }}`,
/// `<your-key>`, `[KEY]`) or a run of filler (`xxxx`, `****`). A bracketed value with a quote or a
/// comma inside it is a list of values (`["s3cr3t"]`), not a template. A literal weak word
/// (`changeme`) is NOT a template: see [`is_weak_default_word`].
pub(super) fn assigned_value_is_template(raw: &str) -> bool {
    let value = raw
        .trim()
        .trim_matches(|c: char| matches!(c, '"' | '\'' | '`' | ';' | ',') || c.is_whitespace());
    let closed = |open: &str, close: char| {
        value.len() > open.len() && value.starts_with(open) && value.ends_with(close)
    };
    let single_token = || !value.contains(['"', '\'', '`', ',']);
    value.is_empty()
        || closed("${", '}')
        || closed("{{", '}')
        || (closed("<", '>') && single_token())
        || (closed("[", ']') && single_token())
        || (value.chars().count() >= 3 && value.chars().all(|c| matches!(c, 'x' | 'X' | '*')))
}

/// Whether the value `snippet` cites is wholly an unfilled template token, in any file: either
/// every secret-shaped match the secrets crate finds is a template token (and none is a literal
/// weak word, which the crate also files under that twin), or (when it finds none) every line
/// assigns a template, an empty string or filler. A placeholder WORD inside a value that is
/// otherwise secret-shaped (`sb_secret_FAKE_...`) is not this: it proves nothing outside an
/// example file. A snippet with no assignment at all is not a template.
pub(super) fn snippet_value_is_template(snippet: &str) -> bool {
    use camerata_secrets::Twin;
    let found = camerata_secrets::detect_all(snippet);
    if !found.is_empty() {
        return found.iter().all(|m| match m.twin {
            Some(Twin::TemplateToken) => !is_weak_default_word(&snippet[m.value.clone()]),
            // A placeholder word makes the value a template only when the value is nothing but
            // filler (`xxxxxxxx`); a word inside a longer secret-shaped value is not.
            Some(Twin::PlaceholderWord(_)) => assigned_value_is_template(&snippet[m.value.clone()]),
            _ => false,
        });
    }
    let lines: Vec<&str> = snippet
        .lines()
        .filter(|line| !line.trim().is_empty())
        .collect();
    !lines.is_empty()
        && lines.iter().all(|line| {
            line.find(['=', ':'])
                .is_some_and(|at| assigned_value_is_template(&line[at + 1..]))
        })
}

/// Whether `snippet` cites a real credential value: one the secrets crate classes as a secret (a
/// raw value, as the floor sees the line before redaction) or one already redacted (the redaction
/// ellipsis is left only where a real value was). A real key pasted into `.env.example` is a
/// leak, whatever the file is called.
pub(super) fn snippet_holds_real_secret(snippet: &str) -> bool {
    snippet.contains('\u{2026}')
        || camerata_secrets::detect_all(snippet)
            .iter()
            .any(camerata_secrets::SecretMatch::is_secret)
}

/// Whether `f` is a secret row about a placeholder: a secret rule whose row cites no real value
/// and either (a) sits in an example or sample file, or (b) whose value is wholly an unfilled
/// template token wherever it sits. A placeholder word inside an otherwise secret-shaped value
/// outside an example file keeps its rating: the word is not proof the value is fake.
pub(crate) fn is_placeholder_secret_row(f: &Finding) -> bool {
    is_secret_rule(&f.rule_id)
        && !snippet_holds_real_secret(&f.snippet)
        && (is_example_file_path(&f.path) || snippet_value_is_template(&f.snippet))
}

/// A placeholder row is never above [`PLACEHOLDER_SEVERITY_CEILING`], whether it stands alone,
/// leads a merged row or is absorbed into one; the lowering is recorded in words.
pub(crate) fn cap_placeholder_severity(f: &mut Finding) {
    if severity_rank(&f.severity) <= severity_rank(PLACEHOLDER_SEVERITY_CEILING)
        || !is_placeholder_secret_row(f)
    {
        return;
    }
    let from = f.severity.clone();
    record_severity_adjustment(
        f,
        &from,
        PLACEHOLDER_SEVERITY_CEILING,
        "the value is a placeholder or sits in an example file, not a live secret",
    );
    f.severity = PLACEHOLDER_SEVERITY_CEILING.to_string();
}

/// Record that `from` became `to` on `f`, and why, as one sentence through the ONE shared
/// writer ([`crate::severity_bounds::record_adjustment`], which de-duplicates and caps); the
/// exporters read it back with `recorded_adjustments`. A no-op when the two are the same. The
/// reason is flattened to one line and capped so the entry stays one sentence.
pub(crate) fn record_severity_adjustment(f: &mut Finding, from: &str, to: &str, reason: &str) {
    if from == to {
        return;
    }
    let verb = if severity_rank(to) < severity_rank(from) {
        "lowered"
    } else {
        "raised"
    };
    let mut why: String = reason.split_whitespace().collect::<Vec<_>>().join(" ");
    if why.chars().count() > 300 {
        why = why.chars().take(297).collect::<String>() + "...";
    }
    let sentence = if why.is_empty() {
        format!("severity {verb} from {from} to {to}")
    } else {
        format!("severity {verb} from {from} to {to}: {why}")
    };
    crate::severity_bounds::record_adjustment(f, sentence);
}

/// The one-line headline of a finding: its title (the text before the first `HEADLINE_SEP` in
/// `detail`), or the start of its detail when it carries no title. Capped.
pub(crate) fn finding_headline(f: &Finding) -> String {
    let head = match f.detail.split_once(HEADLINE_SEP) {
        Some((title, _)) => title.trim(),
        None => f.detail.trim(),
    };
    // Model text reaches client prose through this headline: no em dashes, and no hyphen standing
    // in for one (house style); a dash between clauses becomes a comma.
    let head = head.replace(" \u{2014} ", ", ").replace('\u{2014}', ", ");
    let one_line: String = head.split_whitespace().collect::<Vec<_>>().join(" ");
    if one_line.chars().count() > 160 {
        one_line.chars().take(157).collect::<String>() + "..."
    } else {
        one_line
    }
}

/// The snippet as a dedup key: whitespace collapsed and lowercased, so two chunks quoting the
/// same line with different spacing still agree.
pub(crate) fn dedup_snippet_key(snippet: &str) -> String {
    snippet
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_ascii_lowercase()
}

/// Whether two normalized snippets name the same code: equal, or one is inside the other (a
/// chunk quoting the whole statement and another only the call in it) when the shorter has at
/// least [`MIN_CONTAINED_SNIPPET`] characters. An empty snippet matches only another empty one.
pub(crate) fn same_snippet(a: &str, b: &str) -> bool {
    if a.is_empty() || b.is_empty() || a == b {
        return a == b;
    }
    // Containment counts only for a substantial shorter snippet: a short token such as `id`
    // sits inside almost any line and proves nothing.
    let (short, long) = if a.len() <= b.len() { (a, b) } else { (b, a) };
    short.chars().count() >= MIN_CONTAINED_SNIPPET && long.contains(short)
}

/// Shortest normalized snippet that may match by being contained in another (M5).
pub(crate) const MIN_CONTAINED_SNIPPET: usize = 12;

/// Drop the repeats of one finding reported by several passes, keyed on
/// `(path, line, rule id)` plus the snippet (M5): two findings are repeats when their
/// normalized snippets are equal or one contains the other. A different snippet is a different
/// problem (two things one rule found at line 0 of a file, or at one unresolved line
/// estimate), so it is kept. Of a group of repeats the first is kept, at the HIGHEST severity
/// any repeat carried. Order is preserved.
/// D1, A10: after the detail and calibration steps, throw out every row the model INVENTED (rule
/// id prefixed `AI-`) whose own text says to discard it ("discard this finding"), or whose rule
/// id or headline is a placeholder. Calibration itself never drops a row; the verdict it and the
/// detail step write into the row's explanation is what is acted on here, and only for a row no
/// adopted rule backs. The row is KEPT and marked [`crate::engine_discard`] with the reason, never
/// deleted. Operator side, each is logged with its rule and place; the client report never
/// mentions it. Returns how many rows were marked.
pub(super) fn mark_discarded_invented_rows(
    repo: &str,
    findings: &mut [Finding],
    restored: &crate::engine_discard::RestoredKeys,
) -> usize {
    let mut marked = Vec::new();
    for f in findings.iter_mut() {
        // A12 item 2: a row the reviewer put back is never marked again.
        if crate::engine_discard::is_discarded(f)
            || !f.rule_id.starts_with("AI-")
            || restored.covers(f)
        {
            continue;
        }
        let headline = finding_headline(f);
        if !crate::junk_finding::is_junk_invented_row(&f.rule_id, &headline, &f.detail) {
            continue;
        }
        let reason = if crate::junk_finding::is_placeholder_rule_id(&f.rule_id) {
            crate::engine_discard::PLACEHOLDER_RULE_ID
        } else if !headline.trim().is_empty() && crate::junk_finding::is_filler(&headline) {
            crate::engine_discard::PLACEHOLDER_HEADLINE
        } else {
            crate::engine_discard::DISCARD_VERDICT
        };
        crate::engine_discard::mark(f, reason);
        marked.push(format!("{} at {}:{}", f.rule_id, f.path, f.line));
    }
    if !marked.is_empty() {
        eprintln!(
            "[camerata-server] {repo}: threw out {} finding(s) the model's own text said to discard (kept, marked): {}",
            marked.len(),
            marked.join("; ")
        );
    }
    marked.len()
}

pub(super) fn dedup_findings_keep_max_severity(findings: &mut Vec<Finding>) {
    let mut by_key: std::collections::HashMap<(String, usize, String), Vec<(String, usize)>> =
        std::collections::HashMap::new();
    let mut kept: Vec<Finding> = Vec::with_capacity(findings.len());
    for f in std::mem::take(findings) {
        let key = (f.path.clone(), f.line, f.rule_id.clone());
        let snip = dedup_snippet_key(&f.snippet);
        let entry = by_key.entry(key).or_default();
        match entry.iter().find(|(s, _)| same_snippet(s, &snip)) {
            Some(&(_, at)) => {
                if severity_rank(&f.severity) > severity_rank(&kept[at].severity) {
                    kept[at].severity = f.severity;
                }
            }
            None => {
                entry.push((snip, kept.len()));
                kept.push(f);
            }
        }
    }
    *findings = kept;
}

/// Resolve each finding's line DETERMINISTICALLY from its verbatim snippet. LLMs can't count
/// newlines, so model line numbers drift (the dogfooding run cited header cells for the
/// data-row loops); the snippet the model COPIED is reliable. For each finding we locate the
/// snippet in its file and take the matching line, disambiguating duplicate matches by
/// proximity to the model's estimate. Snippet not found (paraphrase, or a description rather
/// than code) → the model's line is kept as the fallback. The model says WHAT; code says WHERE.
pub(crate) fn resolve_finding_lines(findings: &mut [Finding], files: &[(String, String)]) {
    let by_path: std::collections::HashMap<&str, &str> = files
        .iter()
        .map(|(p, c)| (p.as_str(), c.as_str()))
        .collect();
    for f in findings.iter_mut() {
        let Some(content) = by_path.get(f.path.as_str()) else {
            continue;
        };
        resolve_finding_line(f, content);
    }
}

/// [`resolve_finding_lines`] for one finding whose file text is `content`. Also used by the
/// evidence check (`onboard::evidence_check`), which applies this correction first and then looks
/// for the snippet near the corrected line.
pub(crate) fn resolve_finding_line(f: &mut Finding, content: &str) {
    f.line = resolve_line(content, &f.snippet, f.line);
}

/// The line `snippet` is on in `content`, nearest `line`; `line` itself when the snippet is too
/// short to locate or is on no single line. The pure form of [`resolve_finding_line`], which the
/// evidence check also applies to absorbed sites.
pub(crate) fn resolve_line(content: &str, snippet: &str, line: usize) -> usize {
    let needle = snippet.trim();
    // Too short to locate reliably (single token / punctuation) — keep the model's line.
    if needle.len() < 4 {
        return line;
    }
    let matches: Vec<usize> = content
        .lines()
        .enumerate()
        .filter(|(_, l)| l.contains(needle))
        .map(|(i, _)| i + 1) // 1-based
        .collect();
    // Pick the occurrence nearest the model's (approximate) line so a snippet that appears
    // more than once resolves to the intended site. No match → leave the model's line.
    matches
        .iter()
        .copied()
        .min_by_key(|&ln| ln.abs_diff(line))
        .unwrap_or(line)
}

/// Minimum snippet length for a finding to be considered co-located with others. Below this a
/// snippet is too short to be a reliable "this is the same offending code" signal.
pub(crate) const MIN_MERGE_SNIPPET: usize = 8;

// ── Bug 3: cross-family semantic dedup ───────────────────────────────────────────────────

/// How many lines apart two same-category findings may sit and still be considered the SAME
/// defect for the semantic merge. Covers the benchmark's observed ~3-line attribution drift
/// between an AI finding and the native checker for the same policy, with margin, while staying
/// inside the harness's ±2 line-grading tolerance for the surviving primary (whose line is the
/// deterministic anchor, never the drifted sibling's).
pub(crate) const SEMANTIC_MERGE_WINDOW: usize = 5;

/// The closed semantic taxonomy (design §1a). A finding is only ever merged with another of the
/// SAME category, so the set is deliberately coarse — one bucket per defect *family*, not per
/// rule. Membership is validated (`is_known_category`) so a mis-shaped calibration value can
/// never invent a category and cause a wrong merge.
pub(super) const KNOWN_CATEGORIES: &[&str] = &camerata_api_types::edits::KNOWN_CATEGORIES;

/// True if `c` is a member of the closed taxonomy.
pub(crate) fn is_known_category(c: &str) -> bool {
    KNOWN_CATEGORIES.contains(&c)
}

/// Fallback heuristic (design §1a step 3): map a rule id to a taxonomy category by the tokens it
/// contains. This is our own finite, repo-agnostic rule vocabulary — the token set is exhaustive
/// by construction over the corpus + floor + checker ids, not fitted to any one repo. Priority
/// order matters: more specific families (RLS, TLS, secrets) are checked before the broad
/// authorization/arch buckets so a `SUPABASE-RLS-*` id lands in `rls-policy`, not `authorization`.
/// No token matches → `None` (which makes the finding un-mergeable — fail-open to over-telling).
/// `pub(crate)` (REG-3) so `report_export::export_invariants_gate` can check the same
/// also-matches-never-crosses-category invariant this module's merge predicate enforces, without
/// duplicating the taxonomy.
///
/// WORD-BOUNDARY matched (`security_token_matches`, the same helper/approach `finding_class`'s
/// `SECURITY_RULE_TOKENS` uses — see that const's doc comment), not raw substring, for the exact
/// reason that fix exists: cross-checked against every real rule id in
/// `crates/rules/principles/**`, a naive `id.contains(token)` scan produced real false positives
/// here too. The one that prompted this fix: bare `"ARCH"` matched inside `"SEARCH"` (from
/// `"SEARCH-PATH"`), so `SUPABASE-FUNC-SEARCH-PATH-1` — a SECURITY DEFINER / search-path-hijacking
/// rule (D1) — was miscategorized `arch-conformance`. The corpus audit found three more of the
/// SAME class `finding_class` already fixed, now fixed here too: bare `"RCE"` matched inside
/// `"RESOURCE"`/`"INTERCEPTORS"` (`ARCH-RESOURCE-LIFECYCLE-1`, `JAVA-RESOURCE-MANAGEMENT-1`,
/// `JAVASCRIPT-NEST-INTERCEPTORS-CROSS-CUTTING-1`, `GO-GRPC-INTERCEPTORS-AUTH-LOGGING-1` — none
/// injection); bare `"INJECT"` matched dependency-injection pattern rules
/// (`JAVA-SPRING-CONSTRUCTOR-INJECTION-1`, `JAVASCRIPT-ANGULAR-DI-CONSTRUCTOR-OR-INJECT-1`, not
/// injection vulnerabilities — replaced with the same explicit `*-INJECTION`/`*-SQL` compounds
/// `SECURITY_RULE_TOKENS` uses); bare `"SESSION"` matched `PYTHON-FASTAPI-DI-SESSION-1`, a
/// database-session lifecycle rule, not an authentication session (dropped; the real case is
/// already caught via `"GETSESSION"`). One new compound was added rather than dropping coverage:
/// `"GRAMMAR-INJECTION"`, so `SEC-NO-QUERY-GRAMMAR-INJECTION-1` (D3's query-grammar-injection
/// class) still lands in `injection` once bare `"INJECT"` is gone. And `"-DI-"` (dependency-
/// injection abbreviation) is dropped entirely rather than word-boundary-adapted: as a hyphen-
/// wrapped substring it was already boundary-safe (hyphens on both sides can only occur around a
/// standalone `DI` component), but there is no safe way to express "exact word DI, not a prefix"
/// through the shared prefix-matching single-word semantics without reintroducing a false-
/// positive class of its own (`"DIRECT"`, `"DISABLED"`, `"DIGEST"` all start with `"DI"|`) — the
/// one real corpus id that relied on it (`JAVASCRIPT-NEST-MODULES-PROVIDERS-DI-1`) simply falls
/// through to `None` now, the fail-open direction this function already commits to.
pub(crate) fn categorize_rule_id(rule_id: &str) -> Option<String> {
    let id = rule_id.to_ascii_uppercase();
    let words: Vec<&str> = id.split('-').collect();
    let has = |token: &str| security_token_matches(&words, token);
    let cat = if has("RLS") || has("POLICY") || has("ROW-LEVEL") {
        "rls-policy"
    } else if has("TLS") || has("SSL") || has("HTTPS") || has("CERT") {
        "transport-security"
    } else if has("SECRET")
        || has("HARDCODED")
        || has("CREDENTIAL")
        || has("API-KEY")
        || has("APIKEY")
        || has("PASSWORD")
    {
        // NOT the floor's private-key and vendor-token rules, nor the Supabase service-role
        // rule: a category here is a MERGE key ("same file, same category, within five lines"),
        // and two credentials a few lines apart are two defects that a redacted snippet gives
        // `objects_conflict` no way to tell apart. See
        // `a_floor_private_key_and_a_different_ai_secret_two_lines_apart_stay_two_rows`. The
        // cockpit's decision memory identifies those rows by rule id instead
        // (`FindingView::is_secret_exposure`).
        "secret-exposure"
    } else if has("RAW-SQL")
        || has("STRING-SQL")
        || has("PARAMETERIZED")
        || has("SQL-INJECTION")
        || has("COMMAND-INJECTION")
        || has("CODE-INJECTION")
        || has("GRAMMAR-INJECTION")
        || has("XSS")
        || has("SSRF")
        || has("DESERIAL")
        || has("RCE")
    {
        "injection"
    } else if has("LOGIN")
        || has("GETUSER")
        || has("GETSESSION")
        || has("AUTHN")
        || has("AUTHENTICAT")
    {
        "authentication"
    } else if has("AUTHZ")
        || has("AUTHORIZ")
        || has("BYPASS")
        || has("SERVICE-ROLE")
        || has("RBAC")
        || has("PERMISSION")
        || has("ACCESS-CONTROL")
    {
        "authorization"
    } else if has("VALIDAT") || has("SANITIZE") || has("INPUT") {
        "input-validation"
    } else if has("PANIC")
        || has("UNWRAP")
        || has("EXPECT")
        || has("ERROR-HANDLER")
        || has("ERROR-HANDLERS")
        || has("ERROR-HANDLING")
        || has("FALLIBLE")
    {
        "error-handling"
    } else if has("TEST") || has("SPEC") || has("FIXTURE") {
        "testing-style"
    } else if has("PERF")
        || has("N-PLUS")
        || has("NPLUS")
        || has("HOT-READ")
        || has("CACHE")
        || has("PAGINATION")
    {
        "performance"
    } else if has("EXPOSE") || has("EXPOSED") || has("EXPOSURE") || has("CORS") || has("PUBLIC") {
        "resource-exposure"
    } else if has("ARCH")
        || has("LAYER")
        || has("MONOLITH")
        || has("MIDDLEWARE")
        || has("REPO-PER")
        || has("ROUTE-PLACEMENT")
        || has("QUERY-LIBRARY")
        || has("VERSIONING")
    {
        "arch-conformance"
    } else {
        return None;
    };
    Some(cat.to_string())
}

/// The origin tier of a finding, for the semantic merge's primary ordering and its
/// "both-deterministic-never-merge" guard. Deterministic = produced by the floor / native
/// checkers / preview tools (exact by construction); AI findings are the model's, split into
/// `adopted` (mapped to a real corpus rule id) and `invented` (`AI-` prefix). Signal: the AI
/// pipeline is the only producer that sets `confidence` (calibration) or an `AI-` rule id;
/// floor/checker/preview findings carry neither.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Origin {
    Deterministic,
    AdoptedAi,
    InventedAi,
}

/// A held row produced by a deterministic architectural checker's adapter
/// (`onboard::arch_violation_to_finding`): it is a preview row from the `camerata-arch` tool and
/// carries the HELD marker (`needs-review`) with no calibration behind it. The calibration pass
/// only ever sees AI-pass findings, so no AI-calibrated row has this combination.
pub(super) fn is_adapter_held_row(f: &Finding) -> bool {
    f.preview
        && f.preview_tool.as_deref() == Some(crate::onboard::ARCH_PREVIEW_TOOL)
        && f.needs_review
        && f.confidence.as_deref() == Some("needs-review")
}

pub(crate) fn finding_origin(f: &Finding) -> Origin {
    if f.rule_id.starts_with("AI-") {
        Origin::InventedAi
    } else if is_adapter_held_row(f) {
        // A review-graded row from a deterministic architectural checker carries
        // `confidence = needs-review` as its HELD marker, not as a calibration verdict: the
        // AI pipeline never saw it, so it keeps the deterministic tier (and with it the
        // det+det "two distinct defects" guard).
        Origin::Deterministic
    } else if f.confidence.is_some() {
        // A non-AI- rule id that calibration touched = an adopted corpus rule from the AI tier.
        Origin::AdoptedAi
    } else {
        Origin::Deterministic
    }
}

/// Rank for choosing the primary of a semantic group: deterministic beats adopted beats
/// invented (design §1c). The deterministic side owns the exact line, which is what preserves
/// D1/D2/D7 line-grading through the merge.
pub(crate) fn origin_rank(o: Origin) -> u8 {
    match o {
        Origin::Deterministic => 2,
        Origin::AdoptedAi => 1,
        Origin::InventedAi => 0,
    }
}

/// W7 (B9): whether a dotted run extracted from prose is shaped like a real IDENTIFIER
/// (`schema.table`, `module.spec.ts`) rather than an English dotted abbreviation (`e.g`,
/// `i.e`, `U.S`). The structural property of the abbreviation class is that EVERY dotted
/// segment is too short to be an identifier component — so the test is "at least one
/// alphabetic segment of 3+ characters", never a blacklist of observed tokens (the extractor
/// was reading English; `e.g` alone caused 5 of 51 `objects_conflict` vetoes). A genuine
/// short-alias run like `t.id` is also dropped — a two-character alias carries no cross-
/// finding identity either, so losing it only returns the veto to its fail-open default.
pub(super) fn dotted_run_is_identifier(t: &str) -> bool {
    t.contains('.')
        && t.split('.')
            .any(|seg| seg.len() >= 3 && seg.chars().any(|c| c.is_alphabetic()))
}

/// Extract the structural OBJECTS a finding names — dotted identifiers (`schema.table`) and
/// quoted identifiers (`"policy name"`, `'table'`). Two findings that each name a non-empty,
/// DISJOINT object set are about different things (different tables/policies) and must not
/// merge even when same-category and adjacent — the discrimination guard that keeps
/// SELECT-vs-INSERT / table-A-vs-table-B distinct. Repo-agnostic: pure lexical extraction over
/// snippet+detail, no rule-specific parsing.
pub(super) fn structural_objects(f: &Finding) -> std::collections::HashSet<String> {
    let mut out = std::collections::HashSet::new();
    let text = format!("{} {}", f.snippet, f.detail);
    // Dotted identifiers: schema.table / a.b.c — take each dotted run as one object. A run's
    // TRAILING dot is trimmed before the dot-presence test: it is ordinary sentence punctuation
    // ("...committed to the repository." must not register the bare word "repository" as a
    // dotted object just because a period follows it), not part of the identifier. A genuine
    // dotted identifier immediately followed by a sentence-ending period (e.g. "public.orders.")
    // still keeps its real internal dot after trimming only the outer one. W7 (B9): the run
    // must also be identifier-shaped — see `dotted_run_is_identifier`.
    let mut cur = String::new();
    for ch in text.chars() {
        if ch.is_alphanumeric() || ch == '_' || ch == '.' {
            cur.push(ch);
        } else {
            let trimmed = cur.trim_end_matches('.');
            if dotted_run_is_identifier(trimmed) {
                out.insert(trimmed.to_ascii_lowercase());
            }
            cur.clear();
        }
    }
    let trimmed = cur.trim_end_matches('.');
    if dotted_run_is_identifier(trimmed) {
        out.insert(trimmed.to_ascii_lowercase());
    }
    // Quoted identifiers: "..." or '...' (policy names, quoted table names).
    for (open, close) in [('"', '"'), ('\'', '\'')] {
        let mut it = text.split(open);
        let _ = it.next();
        let mut toggle = true;
        for seg in it {
            if toggle {
                let name = seg.trim();
                if !name.is_empty() && name.len() <= 64 && !name.contains(close) {
                    // seg up to the next delimiter is the quoted content only when the split
                    // gave us the inside; guard against runaway by length + single-token shape.
                    if name
                        .chars()
                        .all(|c| c.is_alphanumeric() || c == '_' || c == ' ' || c == '.')
                    {
                        out.insert(name.to_ascii_lowercase());
                    }
                }
            }
            toggle = !toggle;
        }
    }
    out
}

/// `Finding::captures` keys that are BOOKKEEPING about the row, not objects the finding is
/// about: the producer, the commit a carried finding came from, the severity-adjustment
/// sentences and the commit-history sentence. Two unrelated findings can carry the same such
/// value (the same ceiling sentence, the same commit, the templated history sentence), so they
/// are never identity evidence for a merge, a veto, a backfill or a credential-identity match.
///
/// This is the ONE list. `onboard`'s take and restore of these keys around the merge stages
/// reads it too, so a key added here is held aside there and ignored as an object here. The keys
/// are put back on the surviving rows before `report_export` runs
/// [`root_cause_credential_identities`], which is why they must be excluded here as well and not
/// only while the merge stages run.
pub const BOOKKEEPING_CAPTURE_KEYS: &[&str] = &[
    crate::onboard::PRODUCED_TIER_CAPTURE,
    crate::report_export::CARRIED_FROM_CAPTURE,
    // The sentence a severity bound or verdict leaves when it moves a severity. The AI tier writes
    // it inside `audit_repo`, before the cross-family merge; the same sentence on two findings
    // says the same bound applied, not that they are about the same object.
    crate::severity_bounds::SEVERITY_ADJUSTMENTS_CAPTURE,
    // What stage 1 left on the row for the group reviewer and the tree: its verdict (`real` and
    // the one-line reason) and the outlier reasons it recorded (`severity_raise_held`). They say
    // the same thing about every row they sit on; they name no object.
    STAGE1_CAPTURE,
    OUTLIER_REASONS_CAPTURE,
    // The templated history sentence every secret finding gets from `attach_secret_history_capture`.
    crate::onboard::HISTORY_STATUS_CAPTURE,
    // The templated sentences `enrich_secret_context` puts on every secret-shaped floor finding
    // before the cross-family merge: two different secrets in two files share them, so they
    // would otherwise read as one object and fuse the two findings.
    "gitignore-status",
    "secret-kind",
    // Provenance and fix/detail-source marks the pipeline writes onto a row. `fix_source` and
    // its rejection text are on every Standard-fix row (D8), `detail_source` and its rejection
    // text on every row that shipped the scan's statement (R1-1), `also_seen_by` on a row that
    // absorbed a same-id preview of another tool (R1-2). The same value on two rows says the
    // same bookkeeping applied, not that they are about the same object, and carried rows
    // re-enter the cross-family merge with last run's marks.
    crate::report_export::FIX_SOURCE_CAPTURE,
    crate::report_export::FIX_REJECTION_CAPTURE,
    crate::report_export::DETAIL_SOURCE_CAPTURE,
    crate::report_export::DETAIL_REJECTION_CAPTURE,
    crate::ALSO_SEEN_BY_CAPTURE,
    // M5: the sentence that says a row's quoted snippet was not found near its line.
    crate::onboard::evidence_check::EVIDENCE_UNVERIFIED_CAPTURE,
    crate::onboard::evidence_check::EVIDENCE_UNVERIFIED_SITES_CAPTURE,
    // M4: the list of review flags the detail and fix stages wrote on the row (a wording problem
    // that shipped with a flag). The same flag on two rows says the same check fired, not that
    // the rows are about the same object.
    crate::review_flags::REVIEW_FLAGS_CAPTURE,
    // M4: the mark that says a row's text is real junk and holds it (see `junk_finding`).
    crate::junk_finding::JUNK_CONTENT_CAPTURE,
    // A10: the mark that says the engine threw the row out (the reason is the value).
    crate::engine_discard::ENGINE_DISCARDED_CAPTURE,
    // The reviewer overlay's marks: which edits and fields the reviewer changed, their headline
    // and detail, who produced the row, the merge pointer and the restore mark. None of them
    // names an object, and the same value on two rows says the same thing was done to both.
    crate::engine_discard::ENGINE_RESTORED_CAPTURE,
    crate::report_export::overlay::REVIEWER_EDITED_CAPTURE,
    crate::report_export::overlay::REVIEWER_FIELDS_CAPTURE,
    crate::report_export::overlay::REVIEWER_HEADLINE_CAPTURE,
    crate::report_export::overlay::REVIEWER_DETAIL_CAPTURE,
    crate::report_export::overlay::PROVENANCE_CAPTURE,
    crate::report_export::overlay::MERGED_INTO_CAPTURE,
    crate::report_export::overlay::MERGED_LABEL_CAPTURE,
];

/// The captures that name an object: every capture except the bookkeeping keys.
pub(super) fn object_captures(f: &Finding) -> impl Iterator<Item = (&String, &String)> {
    f.captures
        .iter()
        .filter(|(k, _)| !BOOKKEEPING_CAPTURE_KEYS.contains(&k.as_str()))
}

/// W7 (C4): a finding's normalized detector-capture VALUES — the same normalization
/// [`shared_captured_object`] applies (trimmed, lowercased, length-floored at
/// [`MIN_SHARED_CAPTURE_LEN`] so a bare `id`/`ok` value proves nothing). The STRUCTURED
/// identity a detector emitted when it matched, as opposed to whatever the prose happens to
/// mention.
pub(crate) fn normalized_capture_values(f: &Finding) -> std::collections::HashSet<String> {
    object_captures(f)
        .map(|(_, v)| v.trim().to_ascii_lowercase())
        .filter(|v| v.len() >= MIN_SHARED_CAPTURE_LEN)
        .collect()
}

/// Two findings name conflicting structural targets — the discrimination veto.
///
/// # W7 (C4): structured identity first, container-blind prose second
///
/// The pre-W7 form required the two PROSE object sets to be fully disjoint, which failed in
/// both directions at once: a single shared container token (a schema prefix, a table
/// namespace, a prose abbreviation — B9) made the sets non-disjoint and disabled the veto for
/// any family whose sites sit inside one named container, while the prose sets themselves
/// were the only identity signal even when the DETECTORS had already captured exactly which
/// object each finding is about. Per the independent review ("structured identity, never
/// keyword-list adjustment"), the veto now:
///
/// 1. Checks DETECTOR CAPTURES first: both sides carrying captured values with none in
///    common is a structured different-things proof, independent of prose. (The matching
///    positive signal, [`shared_captured_object`], already exempts a pair from this veto
///    entirely before it is consulted — see [`same_cause_signal`].)
/// 2. Falls back to prose objects, but discounts CONTAINER tokens: a shared object that is
///    merely the parent of a more specific object on either side (`public` next to
///    `public.orders`) is not identity evidence, and conflict is judged over the remaining
///    discriminating objects. `public.orders` vs `public.profiles` now conflicts even when
///    both sides also name their shared schema.
pub(crate) fn objects_conflict(a: &Finding, b: &Finding) -> bool {
    // 1. Structured identity: disjoint detector captures veto on their own.
    let ca = normalized_capture_values(a);
    let cb = normalized_capture_values(b);
    if !ca.is_empty()
        && !cb.is_empty()
        && !ca
            .iter()
            .any(|x| cb.iter().any(|y| object_names_match(x, y)))
    {
        return true;
    }
    // 2. Prose objects, container-discounted.
    let oa = structural_objects(a);
    let ob = structural_objects(b);
    if oa.is_empty() || ob.is_empty() {
        return false;
    }
    // A token is a CONTAINER when a more specific object on either side extends it — either
    // as a dotted prefix (`public` -> `public.orders`) or as a non-leaf segment of a longer
    // dotted path (`db` inside `app.db.orders`).
    let is_container = |s: &str| {
        oa.iter().chain(ob.iter()).any(|o| {
            o.as_str() != s
                && (o.starts_with(&format!("{s}.")) || {
                    let segs: Vec<&str> = o.split('.').collect();
                    segs.len() > 1 && segs[..segs.len() - 1].contains(&s)
                })
        })
    };
    // A genuinely shared object (one that is NOT just a shared container) is identity
    // evidence — no conflict.
    if oa
        .iter()
        .any(|s| !is_container(s) && ob.iter().any(|t| object_names_match(s, t)))
    {
        return false;
    }
    // Otherwise judge disjointness over each side's DISCRIMINATING objects only.
    let da: std::collections::HashSet<&String> = oa.iter().filter(|s| !is_container(s)).collect();
    let db: std::collections::HashSet<&String> = ob.iter().filter(|s| !is_container(s)).collect();
    !da.is_empty()
        && !db.is_empty()
        && !da
            .iter()
            .any(|s| db.iter().any(|t| object_names_match(s, t)))
}

/// Whether two lowercased object names can be the same object: equal, or one is the bare name
/// of the other's `public`-qualified form (`member_contacts` and `public.member_contacts`). A
/// deterministic checker captures the bare table name while the model's row names the qualified
/// one for the same table, and treating those as different objects kept the two rows apart
/// (R1B-5). Two qualified names in different schemas (`public.orders`, `private.orders`) are
/// still different objects.
pub(super) fn object_names_match(x: &str, y: &str) -> bool {
    if x == y {
        return true;
    }
    // A bare name means the `public` schema: the floor's `bare_object_name` strips exactly
    // `public.`. So `users` is not `auth.users` and `orders` is not `private.orders`.
    let bare_of = |bare: &str, qualified: &str| {
        !bare.contains('.') && qualified.split_once('.') == Some(("public", bare))
    };
    bare_of(x, y) || bare_of(y, x)
}

/// Minimum length (characters, after trimming) for a shared `Finding::captures` VALUE to count
/// as the "same root cause across files" identity signal (design point 2b). A real detector-
/// captured object (a table, function name, bucket, flag) is essentially always this long or
/// longer; a bare "id"/"ok"-shaped value below this length is too generic to prove two findings
/// share a root cause and is ignored rather than treated as a match.
pub(super) const MIN_SHARED_CAPTURE_LEN: usize = 3;

/// True when `a` and `b` each carry a non-empty `Finding::captures` map and share at least one
/// captured object VALUE (case-insensitive, trimmed) — the general "same root cause across
/// files" signal design point 2b calls for: a config-flag finding and the handler finding that
/// reads it, or an RLS-policy finding and the page finding that relies on it, each name the SAME
/// concrete object (a table, a function name, a bucket, a flag) even though they live in
/// different files and were flagged by different rule families. Deliberately compares VALUES
/// only, not keys — the two detectors are not expected to use the same capture-token name for
/// the object they each independently identified (one might key it `table`, the other
/// `object-name`); the object identity is what matters, not the label.
pub(crate) fn shared_captured_object(a: &Finding, b: &Finding) -> bool {
    if object_captures(a).next().is_none() || object_captures(b).next().is_none() {
        return false;
    }
    let a_values = normalized_capture_values(a);
    normalized_capture_values(b)
        .iter()
        .any(|v| a_values.contains(v))
}

/// C4-P1 design point 2: backfill a `captures` entry from the finding's own structural-object
/// extraction ([`structural_objects`]) for a finding whose detector never populated `captures`
/// itself. Most deterministic/floor checks and many AI findings leave `captures` empty even when
/// the object they are about IS named in their own `snippet`/`detail` text (a policy name, a
/// table) — without an entry there, such a finding can never participate in the cross-file
/// [`shared_captured_object`] merge signal (design point 2b), even when a call-site finding in
/// another file plainly relies on that SAME object. This closes exactly that gap, GENERALLY
/// (every finding gets a chance, not one keyed to a specific rule or category): when `captures`
/// is empty, take the structural objects [`structural_objects`] already extracts for the
/// `objects_conflict` veto and, when there is EXACTLY ONE, record it under a generic `"object"`
/// key. Deliberately a no-op when there are zero (nothing concrete named) or MORE THAN ONE
/// (ambiguous — which one is the actual sink? guessing risks inventing a false cross-file link)
/// candidates, which keeps this strictly additive and safe: it can only ever CREATE a new merge
/// opportunity for a finding that previously had none, never silently pick the wrong object out
/// of several. Never overwrites a `captures` entry a detector already set.
pub(crate) fn backfill_captured_object(f: &mut Finding) {
    if object_captures(f).next().is_some() {
        return;
    }
    let mut objs = structural_objects(f).into_iter();
    if let (Some(obj), None) = (objs.next(), objs.next()) {
        f.captures.insert("object".to_string(), obj);
    }
}

/// C6-B2 design point 1: keyword substrings that mark a CONSTANT_CASE identifier as naming a
/// secret/credential rather than an ordinary configuration constant — the "evidence-chain /
/// root-cause" merge signal calls for linking findings that each reference the SAME concrete
/// secret/credential, "even across files and with no line proximity". Deliberately a named list
/// rather than a bare "KEY" substring (which would also catch an ordinary identifier like
/// `PRIMARY_KEY_COLUMN`) — the token still also has to pass the CONSTANT_CASE shape test in
/// [`is_credential_identifier`] below, so this is the conservative half of a two-part test, not
/// the whole of it.
pub(super) const CREDENTIAL_IDENTIFIER_KEYWORDS: &[&str] = &[
    "SECRET",
    "API_KEY",
    "APIKEY",
    "ACCESS_KEY",
    "ACCESSKEY",
    "PRIVATE_KEY",
    "PRIVATEKEY",
    "PASSWORD",
    "PASSWD",
    "CREDENTIAL",
    "AUTH_TOKEN",
    "ACCESS_TOKEN",
];

/// Minimum length (characters) for a candidate CONSTANT_CASE token to be considered a credential
/// identifier — mirrors [`MIN_SHARED_CAPTURE_LEN`]'s "too generic to prove anything" floor, set
/// higher here because a genuine secret identifier (`STRIPE_SECRET_KEY`, `DB_PASSWORD`) is always
/// a multi-word compound; anything shorter is more likely an accidental short acronym.
pub(super) const MIN_CREDENTIAL_TOKEN_LEN: usize = 8;

/// True when `tok` is shaped like a CONSTANT_CASE identifier (uppercase ASCII letters, digits,
/// and underscores only, with at least one underscore — ruling out a bare acronym) of at least
/// [`MIN_CREDENTIAL_TOKEN_LEN`] characters, AND its text contains one of
/// [`CREDENTIAL_IDENTIFIER_KEYWORDS`] — e.g. `STRIPE_SECRET_KEY`, `DB_PASSWORD`,
/// `AWS_SECRET_ACCESS_KEY`. The shape check is intentionally strict (an ordinary sentence never
/// emits a fully-uppercase, underscore-bearing multi-word token by accident), which is what lets
/// this fire with NO file/category/distance restriction at all, unlike the looser
/// [`description_overlap_score`] prose heuristic: this is a structural identity match, not prose
/// similarity.
pub(super) fn is_credential_identifier(tok: &str) -> bool {
    if tok.len() < MIN_CREDENTIAL_TOKEN_LEN || !tok.contains('_') {
        return false;
    }
    if !tok
        .chars()
        .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
    {
        return false;
    }
    CREDENTIAL_IDENTIFIER_KEYWORDS
        .iter()
        .any(|kw| tok.contains(kw))
}

/// Extract every CONSTANT_CASE credential/secret identifier (see [`is_credential_identifier`])
/// named in free text. Repo-agnostic, pure lexical scan, no rule-specific parsing — mirrors
/// [`structural_objects`]'s shape, but targets the bare-underscore-identifier shape
/// (`STRIPE_SECRET_KEY`) that [`structural_objects`]'s dotted/quoted extraction does not cover.
pub(super) fn credential_identifier_tokens(text: &str) -> std::collections::BTreeSet<String> {
    let mut out = std::collections::BTreeSet::new();
    let mut cur = String::new();
    for ch in text.chars() {
        if ch.is_ascii_alphanumeric() || ch == '_' {
            cur.push(ch);
        } else if !cur.is_empty() {
            if is_credential_identifier(&cur) {
                out.insert(std::mem::take(&mut cur));
            } else {
                cur.clear();
            }
        }
    }
    if is_credential_identifier(&cur) {
        out.insert(cur);
    }
    out
}

/// C6-B2 design point 1: the GENERAL root-cause identity a finding carries — every credential
/// token (see [`credential_identifier_tokens`]) findable in its own `captures` VALUES plus its
/// own `snippet`/`detail` text. This is the "evidence chain" extraction: the config file that
/// holds `STRIPE_SECRET_KEY`, the module that builds a client by reading `STRIPE_SECRET_KEY`,
/// and a call site whose own explanation names that SAME `STRIPE_SECRET_KEY` as the credential
/// the client it calls was built from, all extract the identical token here — regardless of
/// which file they are in, which rule flagged them, or how far apart their lines sit.
///
/// Pure and side-effect-free (never mutates `f`); used by BOTH [`backfill_credential_identity`]
/// (to seed `captures` so the pre-existing [`shared_captured_object`] cross-file merge signal
/// picks the link up with NO further change to [`same_cause_signal`] itself — the det+det
/// guard, the `objects_conflict` veto exemption, and the AI+AI corroboration guard already treat
/// a `shared_captured_object` match as strong enough to fire regardless of file/category/
/// distance) and, independently, by `report_export`'s top-3 "three distinct root causes"
/// selector (C6-B2 design point 3), as a defense-in-depth check for the case this merge pass
/// conservatively left two same-root-cause rows distinct.
pub(crate) fn root_cause_credential_identities(f: &Finding) -> std::collections::BTreeSet<String> {
    let mut out = credential_identifier_tokens(&format!("{} {}", f.snippet, f.detail));
    for (_, v) in object_captures(f) {
        out.extend(credential_identifier_tokens(v));
    }
    out
}

/// C6-B2 design point 1: backfill `captures` with any credential identifier
/// ([`root_cause_credential_identities`]) a finding's own text names. Deliberately additive and
/// NEVER gated on `captures` already being non-empty (unlike [`backfill_captured_object`]'s
/// single-structural-object case): a credential token is matched by an explicit, narrow
/// keyword+shape test (see [`is_credential_identifier`]), so there is no "which one of several is
/// the real object" ambiguity to guard against — recording every token found is always safe.
/// Each token is inserted under its OWN key (the token itself) so it can never collide with or
/// overwrite an unrelated capture a detector already set (e.g. a `"table"` key from
/// [`backfill_captured_object`]).
pub(crate) fn backfill_credential_identity(f: &mut Finding) {
    for tok in root_cause_credential_identities(f) {
        f.captures.entry(tok.clone()).or_insert(tok);
    }
}

/// The smallest brace-delimited block containing 1-based `line`, as `(start_line, end_line)`.
/// Cheap single-pass brace matcher; returns `None` for brace-free content (SQL, YAML) so callers
/// fall back to the line-window rule. Used to unify two findings on the same handler body even
/// when they sit more than `SEMANTIC_MERGE_WINDOW` lines apart.
pub(super) fn enclosing_block(content: &str, line: usize) -> Option<(usize, usize)> {
    let mut stack: Vec<usize> = Vec::new();
    let mut cur_line = 1usize;
    let mut best: Option<(usize, usize)> = None;
    for ch in content.chars() {
        match ch {
            '\n' => cur_line += 1,
            '{' => stack.push(cur_line),
            '}' => {
                if let Some(open) = stack.pop() {
                    let close = cur_line;
                    // Inner blocks close first, so the FIRST containing block we see is the
                    // innermost (smallest) one — take it and stop updating.
                    if best.is_none() && open <= line && line <= close {
                        best = Some((open, close));
                    }
                }
            }
            _ => {}
        }
    }
    best
}

/// W7 (B7): the blank-line-delimited STANZA containing 1-based `line` — the natural
/// "construct" of brace-free content (a SQL statement, a TOML table, a YAML mapping block, an
/// `.env` section). Returns the stanza's `(start_line, end_line)`, or `None` when `line` is
/// out of range or falls ON a blank line (a blank line belongs to no stanza).
pub(super) fn enclosing_stanza(content: &str, line: usize) -> Option<(usize, usize)> {
    let lines: Vec<&str> = content.lines().collect();
    if line == 0 || line > lines.len() || lines[line - 1].trim().is_empty() {
        return None;
    }
    let mut start = line;
    while start > 1 && !lines[start - 2].trim().is_empty() {
        start -= 1;
    }
    let mut end = line;
    while end < lines.len() && !lines[end].trim().is_empty() {
        end += 1;
    }
    Some((start, end))
}

/// True when `a` and `b` fall inside the same innermost brace block of `content` — or, W7
/// (B7), inside the same blank-line-delimited stanza when the content carries NO braces at
/// all. Before B7 this was unconditionally false for brace-free content, so `.env`, `.sql`,
/// `.toml` and `.yaml` could never satisfy any in-construct merge signal — a blind spot over
/// exactly the file types config and migration defects live in. The stanza fallback is
/// deliberately scoped to FULLY brace-free content: in a brace-bearing file, "no enclosing
/// block" genuinely means top-level, which is not a construct (two unrelated top-level items
/// must not read as one), and YAML/TOML carrying inline `{...}` flow syntax conservatively
/// keeps the old semantics too.
pub(super) fn same_construct(content: &str, a: usize, b: usize) -> bool {
    match enclosing_block(content, a) {
        Some((s, e)) => b >= s && b <= e,
        None => {
            if content.contains('{') {
                return false;
            }
            match enclosing_stanza(content, a) {
                Some((s, e)) => b >= s && b <= e,
                None => false,
            }
        }
    }
}

/// Normalize a finding's headline text (its `snippet`/title plus `detail`) into a lowercase
/// alphanumeric word set for [`description_overlap_score`]. Punctuation- and case-insensitive
/// so cosmetic phrasing differences between a deterministic rule's templated wording and an AI
/// finding's free-text description of the SAME defect ("Hardcoded API key" vs "hard-coded API
/// key committed") don't suppress an otherwise-real prose match.
pub(super) fn description_word_set(f: &Finding) -> std::collections::HashSet<String> {
    let text = format!("{} {}", f.snippet, f.detail).to_ascii_lowercase();
    let mut out = std::collections::HashSet::new();
    let mut cur = String::new();
    for ch in text.chars() {
        if ch.is_alphanumeric() {
            cur.push(ch);
        } else if !cur.is_empty() {
            out.insert(std::mem::take(&mut cur));
        }
    }
    if !cur.is_empty() {
        out.insert(cur);
    }
    out
}

/// Jaccard similarity (`|intersection| / |union|`) of `a` and `b`'s normalized description word
/// sets — the GENERAL "near-identical prose describing the same defect" merge signal (design
/// MERGE gap (i)): a deterministic finding and an AI finding a few lines apart, in different
/// categories or outside the line-proximity window, whose headline text is nonetheless
/// substantively the same sentence. Symmetric; `0.0` when either side has no describable text
/// (never a spurious match off two empty sets).
pub(super) fn description_overlap_score(a: &Finding, b: &Finding) -> f64 {
    let wa = description_word_set(a);
    let wb = description_word_set(b);
    if wa.is_empty() || wb.is_empty() {
        return 0.0;
    }
    let intersection = wa.intersection(&wb).count();
    let union = wa.union(&wb).count();
    if union == 0 {
        0.0
    } else {
        intersection as f64 / union as f64
    }
}

/// Threshold for [`description_overlap_score`] to count as a same-defect merge signal.
/// Deliberately HIGH — this is a general lexical heuristic with no structural grounding of its
/// own (unlike [`shared_captured_object`]), so it is reserved for genuinely near-identical prose
/// and paired with the `path` restriction + `objects_conflict` veto in
/// [`same_cause_signal`] to avoid over-merging findings that merely share common security
/// vocabulary.
///
/// Lowered from `0.6` to `0.5` (MERGE cycle-3, observed-failure 1:
/// the cycle-3 queue-hardening notes): a real duplicate pair — one defect
/// reported under two rule ids/tiers more than [`SEMANTIC_MERGE_WINDOW`] lines apart in the same
/// file, each paraphrased in the generator's OWN words rather than copy-pasted — routinely lands
/// in the high-0.5s on raw Jaccard once ordinary paraphrase (synonyms, reordered clauses, a
/// differently-worded clause for the same fact) is accounted for; `0.6` was proven too strict for
/// that genuinely-near-identical-but-not-verbatim case (see
/// `merge_gap_i_near_identical_prose_merges_beyond_window_distance` for the measured score this
/// was calibrated against). `0.5` is still well clear of the near-zero overlap two UNRELATED
/// same-file findings produce (see `merge_gap_ii_two_deterministic_findings_with_no_shared_object_
/// stay_distinct`, `merge_gap_i_sink_and_call_site_in_different_files_stay_two_rows`), and the
/// `objects_conflict` veto in [`same_cause_signal`] still fires independently of this
/// threshold, so two findings whose prose overlaps only because they share generic
/// security/hygiene vocabulary but name DISJOINT structural objects (different tables, different
/// policies) are still kept apart.
pub(super) const DESCRIPTION_OVERLAP_THRESHOLD: f64 = 0.5;

/// Whether a row is about an exposed secret (the `secret-exposure` category the merge pass
/// backfills from the rule id, or the model or calibration set).
pub(crate) fn is_secret_exposure(f: &Finding) -> bool {
    f.category.as_deref() == Some("secret-exposure")
}

/// The redacted fingerprints of the credential values a row's snippet holds: the prefix and last
/// four characters [`camerata_secrets::redact_str`] leaves (`sk_live_…ab12`), and for a value too
/// short to show a tail the prefix and the length (`…|12`). A raw value is redacted first, so a
/// floor row that kept the raw line and an AI row that quoted the redacted line give the same
/// fingerprint. Empty when the snippet holds no credential value.
pub(super) fn credential_fingerprints(f: &Finding) -> std::collections::BTreeSet<String> {
    const ELLIPSIS: char = '\u{2026}';
    let redacted = camerata_secrets::redact_str(&f.snippet);
    let chars: Vec<char> = redacted.chars().collect();
    let is_body = |c: char| c.is_ascii_alphanumeric() || c == '_' || c == '-';
    let mut out = std::collections::BTreeSet::new();
    for (i, &c) in chars.iter().enumerate() {
        if c != ELLIPSIS {
            continue;
        }
        let mut start = i;
        while start > 0 && is_body(chars[start - 1]) {
            start -= 1;
        }
        let mut end = i + 1;
        while end < chars.len() && is_body(chars[end]) {
            end += 1;
        }
        let prefix: String = chars[start..i].iter().collect();
        let tail: String = chars[i + 1..end].iter().collect();
        // A value that shows no prefix and no tail (a password) leaves only its length, which
        // every 12-character password shares: it proves nothing, so it makes no fingerprint.
        if prefix.is_empty() && tail.is_empty() {
            continue;
        }
        let mut print = format!("{prefix}{ELLIPSIS}{tail}");
        if tail.is_empty() {
            let rest: String = chars[end..].iter().collect();
            if let Some(count) = rest.strip_prefix(" (") {
                let digits: String = count.chars().take_while(char::is_ascii_digit).collect();
                print.push_str(&format!("|{digits}"));
            }
        }
        out.insert(print);
    }
    out
}

/// Whether two secret rows are the same credential: the same exact location in one repository,
/// or a shared redacted fingerprint ([`credential_fingerprints`]). A row that cites no credential
/// value, at a different place, is not proven to be the same credential.
pub(crate) fn same_credential(a: &Finding, b: &Finding) -> bool {
    if a.repo != b.repo {
        return false;
    }
    if a.path == b.path && a.line != 0 && a.line == b.line {
        return true;
    }
    let a_prints = credential_fingerprints(a);
    !a_prints.is_empty()
        && credential_fingerprints(b)
            .iter()
            .any(|print| a_prints.contains(print))
}

/// The pairwise semantic-merge predicate (design §1b, extended by P1 design point 2, the MERGE
/// cycle-2/cycle-3 hardening passes, and C4-P1's defect-shaped merge redesign). `a` and `b`
/// cluster as the same defect when ANY of SIX GENERAL signals fires (never a seventh,
/// fixture-specific one):
///  (a) same file, same (present) category, and within-window-or-same-construct — the original
///      cross-family-at-one-site signal; or
///  (a') same file, SAME CONSTRUCT (not merely the coarser line window), AT LEAST ONE side is
///      security-tier ([`finding_class`]), AND the pair is about the SAME defect class — C4-P1
///      design point 1's generalization of (a), tightened by REG-3: a boundary-validation rule
///      and a privileged-key-bypass rule flagged on the SAME handler body are one defect wearing
///      two rule names, not two, even when their `category` strings differ, PROVIDED they are
///      still linked by either a `categorize_rule_id` class match or a shared captured object
///      ([`shared_captured_object`]) — e.g. both name the same guard flag that is at once
///      under-validated and what the bypass trusts. Scoped to `same_construct` specifically
///      (never the looser line window) because "literally the same function body" is itself
///      strong enough same-site evidence to drop the EXACT-category-string requirement, but it
///      is NOT enough on its own to fuse two genuinely unrelated rules that merely happen to
///      fire in the same block (REG-3: an indexing-strategy or pagination-convention finding
///      co-located with an injection/authorization finding must stay two rows — see
///      `p1_c4_same_construct_pair_with_unrelated_category_and_no_object_stays_two_rows`); or
///  (b) they share a captured structural object ([`shared_captured_object`]) — the "same root
///      cause across files" signal (a config flag and the handler that reads it; an RLS policy
///      and the page that relies on it), which does NOT require the same file or category; or
///  (c) same file and near-identical description prose ([`description_overlap_score`] at or
///      above [`DESCRIPTION_OVERLAP_THRESHOLD`]) — a det+AI (or any-tier) pair describing ONE
///      defect a few lines apart whose category differs or which sits outside the line window,
///      but whose headline text is substantively the same sentence. Deliberately same-PATH-only:
///      this must never fuse a sink finding in one file with an unrelated call-site finding in
///      another file just because the prose happens to overlap; or
///  (d) the IDENTICAL rule id fired on both, same file, within-window-or-same-construct OR AT THE
///      SAME SEVERITY (MERGE cycle-3, observed-failure 3; widened by C4-P1 design point 3) — the
///      literal strongest "same defect" identity signal there is, so it needs neither a matching
///      `category` (an invented `AI-` id with no taxonomy mapping still qualifies) nor prose
///      overlap. The same-severity arm is NOT distance-scoped on purpose: it is the general form
///      of the pre-existing "N location(s)" collapse ([`group_structural_needs_review`]'s
///      pattern), generalized from informational/needs-review rows to every curated row — one
///      rule firing three times across one file at one severity is one row listing three sites,
///      not three near-duplicate rows, regardless of how far apart the sites are. Still subject
///      to the `objects_conflict` veto just below, so the canonical counter-example (the same
///      RLS-missing rule firing once for `orders` and once for `profiles`) stays two rows.
/// Every wrong-fusion guard still applies on top of whichever signal fired.
pub(crate) fn same_cause_signal(
    a: &Finding,
    b: &Finding,
    content: Option<&str>,
) -> Option<CauseSignal> {
    // W7: kind compatibility is checked before ANY merge signal — a Defect/Deviation and an
    // informational row (ConventionObserved / NoConvention / NotAssessed) make different
    // kinds of statement, and no amount of proximity/prose/rule-id identity may fuse them.
    // See `kinds_merge_compatible`.
    if !kinds_merge_compatible(a, b) {
        return None;
    }
    let in_window = a.path == b.path
        && a.line != 0
        && b.line != 0
        && a.line.abs_diff(b.line) <= SEMANTIC_MERGE_WINDOW;
    let in_construct =
        a.path == b.path && content.is_some_and(|c| same_construct(c, a.line, b.line));

    // Signal (d): the identical rule id, SAME FILE, within window/construct, OR at the same
    // severity regardless of distance — see the doc comment above (the N-site collapse arm).
    // Computed up front since it also exempts the det+det guard just below: two "distinct"
    // deterministic rows that are actually the SAME rule firing twice (nearby, or at the same
    // severity anywhere in the SAME file) are one defect by construction, same as the
    // shared-captured-object case. The N-site arm REQUIRES `a.path == b.path` — the same rule
    // firing in TWO DIFFERENT files is two distinct defects (two places to fix), e.g. the same
    // architectural rule tripping a Rust handler and a TS handler; those must stay two rows.
    // (`in_window`/`in_construct` already imply same path; only the same-severity arm needs it.)
    let same_rule_adjacent = a.rule_id == b.rule_id
        && (in_window || in_construct || (a.path == b.path && a.severity == b.severity));

    // REG-3: same-construct pairs used to override the category requirement unconditionally —
    // this is the GENERAL "same defect class" check that now gates signal (a') below, computed
    // two ways: the explicit `Finding::category` field (when calibration/the rule-id heuristic
    // set it on both sides), OR — independently, so either method alone suffices — the
    // `categorize_rule_id` class derived straight from each rule id. Deliberately NOT the same
    // `same_category` binding signal (a) uses just below: that one only ever looks at the
    // `.category` field, whereas this also-tries-the-rule-id fallback is specific to the
    // same-construct-security override so it still recognizes two corpus rule ids that are
    // self-evidently the same family (e.g. two different `AUTHZ-`/`BYPASS`-flavored ids) even
    // when neither Finding happens to carry an explicit `.category`.
    let same_construct_category_class = matches!((&a.category, &b.category), (Some(ca), Some(cb)) if ca == cb)
        || categorize_rule_id(&a.rule_id)
            .is_some_and(|ca| categorize_rule_id(&b.rule_id).as_deref() == Some(ca.as_str()));

    // Signal (a'): same construct, at least one side security-tier, AND (same defect class OR a
    // shared captured object) — see the doc comment above. Computed up front for the same reason
    // as (d): a pair of DETERMINISTIC rules that both fire inside one handler body, one of them
    // security-tier and genuinely the same defect, is one defect, not two "distinct" deterministic
    // rows. REG-3: previously fired on same-construct + security-tier ALONE, with no requirement
    // the two findings be related at all — that absorbed unrelated co-located rules (an
    // indexing-strategy or pagination-convention finding) into a security finding's
    // `also_matches`, hiding them from their own category/Coverage count. Now requires the
    // category-class match above OR `shared_captured_object`, the same two "same root cause"
    // proofs the rest of this function already relies on.
    let same_construct_security_override = a.path == b.path
        && in_construct
        && (finding_class(a) == FindingClass::Security
            || finding_class(b) == FindingClass::Security)
        && (same_construct_category_class || shared_captured_object(a, b));

    // Guard: two deterministic rows are two distinct defects by construction UNLESS they share a
    // captured structural object (design MERGE gap (ii)), are the same rule id firing twice
    // nearby or at the same severity (signal (d)), or sit in the same construct with one side
    // security-tier (signal (a')) — e.g. two independent secret-detectors both naming the SAME
    // committed secret/file are the same root cause, not two. With none of these, they stay
    // distinct regardless of which clustering signal below would otherwise fire — this preserves
    // the distinct-defects invariant.
    let both_deterministic =
        finding_origin(a) == Origin::Deterministic && finding_origin(b) == Origin::Deterministic;
    if both_deterministic
        && !shared_captured_object(a, b)
        && !same_rule_adjacent
        && !same_construct_security_override
    {
        return None;
    }

    // Signal (a): same file + same category + line-window-or-construct overlap.
    let same_category = matches!((&a.category, &b.category), (Some(ca), Some(cb)) if ca == cb);
    let same_file_adjacent = a.path == b.path && same_category && (in_window || in_construct);

    // Signal (b): a shared captured object, general and cross-file (design point 2b).
    let shared_object = shared_captured_object(a, b);

    // Credential-identity guard: two secret rows are one finding only when they are the same
    // credential. "Both are a leaked secret" and "a few lines apart in one file" prove nothing,
    // and fusing two credentials lets one reviewer decision dispose of both. A shared named
    // credential (signal (b)) still links two rows; a row never links across repositories.
    if is_secret_exposure(a) && is_secret_exposure(b) {
        if a.repo != b.repo {
            return None;
        }
        if !shared_object && !same_credential(a, b) {
            return None;
        }
    }

    // Signal (c): near-identical description prose, general and same-file-only (design MERGE
    // gap (i)).
    let description_overlap =
        a.path == b.path && description_overlap_score(a, b) >= DESCRIPTION_OVERLAP_THRESHOLD;

    if !same_file_adjacent
        && !shared_object
        && !description_overlap
        && !same_rule_adjacent
        && !same_construct_security_override
    {
        return None;
    }

    // Guard: disjoint structural objects named in free text (different tables/policies) only
    // vetoes the LINE-PROXIMITY, PROSE-OVERLAP, SAME-RULE-ADJACENT and SAME-CONSTRUCT-SECURITY
    // signals — two findings that merely sit near each other, or use similar wording, or share a
    // rule id, or share a construct with one side security-tier, but visibly name different
    // things (e.g. the same RLS-missing rule firing once for `orders` and once for `profiles`,
    // nearby in one file, is two distinct violations, not one; a validation rule naming `orders`
    // and a bypass rule naming `profiles` in the same handler is two distinct defects, not one). A
    // `shared_captured_object` match is a stronger, structured same-object proof and is never
    // vetoed by this looser text heuristic.
    if !shared_object
        && (same_file_adjacent
            || description_overlap
            || same_rule_adjacent
            || same_construct_security_override)
        && objects_conflict(a, b)
    {
        return None;
    }

    // Guard: AI+AI needs corroboration beyond mere proximity — one snippet contains the other,
    // both are located (real, resolved code) inside the same construct, they share a captured
    // object, the description-overlap signal itself fired (an equally strong, deliberately
    // high-threshold structured-enough proof), or they are the same rule id firing twice nearby
    // (signal (d), its own proof). Two AI findings citing DIFFERENT real code that merely sit
    // near each other, with no shared object, no real prose match, and no shared rule id, stay
    // separate.
    let both_ai = matches!(finding_origin(a), Origin::AdoptedAi | Origin::InventedAi)
        && matches!(finding_origin(b), Origin::AdoptedAi | Origin::InventedAi);
    if both_ai {
        let sa = a.snippet.trim();
        let sb = b.snippet.trim();
        let snippet_corroborated = (!sa.is_empty() && sb.contains(sa))
            || (!sb.is_empty() && sa.contains(sb))
            || (a.located && b.located && in_construct)
            || shared_object
            || description_overlap
            || same_rule_adjacent
            || same_construct_security_override;
        if !snippet_corroborated {
            return None;
        }
    }
    // The strongest signal that fired names the cause, in the order a reviewer trusts them.
    Some(if shared_object {
        CauseSignal::SharedObject
    } else if same_rule_adjacent {
        CauseSignal::SameRule
    } else if same_construct_security_override {
        CauseSignal::SameConstruct
    } else if same_file_adjacent {
        CauseSignal::AdjacentSameCategory
    } else {
        CauseSignal::DescriptionOverlap
    })
}

/// Which of the same-cause signals fired for a pair (strongest first). It decides the cause a
/// sub-group is labelled with and the hint a possible-duplicate link carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum CauseSignal {
    /// The two rows name the same captured object.
    SharedObject,
    /// The same rule fired twice in one file, nearby or at one severity.
    SameRule,
    /// The same function or block, with a security row and a related class.
    SameConstruct,
    /// The same category on adjacent lines of one file.
    AdjacentSameCategory,
    /// The same defect described in near-identical words in one file.
    DescriptionOverlap,
}

impl CauseSignal {
    /// The sub-group cause text.
    pub(crate) fn cause(self) -> &'static str {
        match self {
            Self::SharedObject => "the same captured object",
            Self::SameRule => "the same rule repeated in one file",
            Self::SameConstruct => "the same function or block",
            Self::AdjacentSameCategory => "adjacent lines of one file in the same category",
            Self::DescriptionOverlap => "the same defect described in the same words",
        }
    }

    /// The test name a possible-duplicate link records.
    pub(crate) fn hint(self) -> &'static str {
        match self {
            Self::SharedObject => "same_cause:shared_object",
            Self::SameRule => "same_cause:same_rule",
            Self::SameConstruct => "same_cause:same_construct",
            Self::AdjacentSameCategory => "same_cause:adjacent_same_category",
            Self::DescriptionOverlap => "same_cause:description_overlap",
        }
    }
}

/// A primary with no fix of its own takes `donor`'s, with the donor's label (D8): a standard fix
/// stays a standard fix, and a model-written one stays a tailored one.
pub(crate) fn inherit_fix(primary: &mut Finding, donor: &Finding) {
    primary.fix_specific.clone_from(&donor.fix_specific);
    for key in [
        crate::report_export::FIX_SOURCE_CAPTURE,
        crate::report_export::FIX_REJECTION_CAPTURE,
    ] {
        match donor.captures.get(key) {
            Some(v) => {
                primary.captures.insert(key.to_string(), v.clone());
            }
            None => {
                primary.captures.remove(key);
            }
        }
    }
    // M4: the fix arrives with the review flags the fix stage wrote on it. The primary keeps its
    // own detail-stage flags; a clean donor clears a stale fix flag.
    let donated: Vec<String> = crate::review_flags::flags_of(donor)
        .into_iter()
        .filter(|flag| crate::review_flags::stage_of(flag) == crate::review_flags::FlagStage::Fix)
        .collect();
    crate::review_flags::set_stage_flags(primary, crate::review_flags::FlagStage::Fix, donated);
}

#[cfg(test)]
mod taxonomy_tests {
    use super::{is_known_category, KNOWN_CATEGORIES};

    /// The review UI's category select and the engine's merge read the same list.
    #[test]
    fn the_engines_taxonomy_is_the_wire_types_list() {
        assert_eq!(
            KNOWN_CATEGORIES,
            &camerata_api_types::edits::KNOWN_CATEGORIES
        );
        for c in camerata_api_types::edits::KNOWN_CATEGORIES {
            assert!(is_known_category(c), "{c}");
        }
    }
}
