# Work-Tracker Integration (VISION follow-up on the tracker-integration design)

Status: design memo, decision-first. Phase 0 (the engine thin slice) stays the
one input box (§2.1) and does not depend on this port. UPDATE 2026-06-13: one
SLICE of this memo, the async CLARIFY-BRIDGE, is promoted to V1 collaboration
architecture (VISION: V1 collaboration) and is NO LONGER purely post-Phase-0. See §0.5. The
rest (full status sync, multi-provider, the native tracker) remains a later
expansion behind the same port.

> **Implementation status (2026-06-14):** this is now partly BUILT, not just
> designed. The `camerata-worktracker` crate implements the canonical shapes, the
> `WorkItemProvider` port, the `native` provider (Phase A), and the Jira, Azure
> DevOps Boards, and GitHub adapters (Phases B-E) with the mapping + request/response
> logic behind an injectable HTTP transport seam (tested with a fake client; the live
> `reqwest` transport is real and type-checked). The async clarify-bridge and the two
> loop-avoidance guards (per-field `SyncPolicy` enforcement + echo suppression) are
> built and tested, with an end-to-end Tier-1 flow test. What REMAINS is live
> execution against real boards: per-provider auth (Jira OAuth 3LO / the ~25-day
> webhook refresh, ADO PAT/Entra, GitHub App), webhook ingress (the opt-in upgrade
> over the poll default), and field-discovery against live projects. The shapes and
> mappings below are the spec those live integrations fulfill.

Researched against current (2026-06-13) provider docs. Every load-bearing
claim below was adversarially verified; see "Unverified assumptions and open
risks" for what was NOT confirmed.

---

## 0. The one-paragraph answer

Build one `WorkItemProvider` port (`native | jira | azure-devops | github`)
behind a single canonical `Story` / `FeatureStatus` shape. Core orchestration
never imports a provider; per-provider auth, webhook signature verification,
field mapping, and rate-limit handling live inside each adapter. Our Story
spine is ALWAYS canonical internally; the external tracker is a MIRROR
(projection) by default, with the authoritative side configurable PER FIELD so
brownfield enterprise can let their board own intake/status while we always own
provenance, gate results, PR links, and sign-off. Inbound is webhook-primary
with a reconciliation poll as the safety net; loops are broken by two
independent guards (per-field direction + echo suppression). Build order runs on
TWO independent axes (refined 2026-06-13, see §3): `native` first (it forces the
canonical shapes), then on the CODE-HOST axis `github` then `azure-devops repos`,
and on the BOARD axis (where the PO lives) `jira` and `azure-devops boards` first
with `github issues` deprioritized (underused as a formal enterprise board). Code
host and product tracker are chosen independently per deployment.

---

## 0.5. V1 elevation: the async clarify-bridge (the reason a slice of this is not post-V1)

Decision (2026-06-13, VISION: V1 collaboration). V1 collaboration runs with NO shared cloud: the Principal Architect
is the single local node, and the tracker is the asynchronous bridge that carries the clarify loop to and
from a non-technical Product Owner who never leaves their board. This promotes ONE slice of this memo into
V1, ahead of the full status-sync / multi-provider / native-tracker work.

The slice that becomes V1, expressed on the existing port (no new architecture):
- **Outbound: post a clarifying-question comment.** When a Story enters `AWAITING_CLARIFICATION`, the
  orchestrator calls the provider to post a formatted comment with the agent's PRODUCT clarifying
  questions, @-mentioning the PO. (Technical tradeoffs and the RuleSet are NOT posted; they stay with the
  Architect locally at architect altitude.) This reuses the same comment-writeback channel already
  specified for provenance (an editable issue comment), with a distinct purpose.
- **Inbound: ingest the answer comment.** The webhook-primary + reconciliation-poll path (§4.1) already
  pulls new comments; the new comment kind is `answer` (the PO's reply), normalized into the
  `InboundWorkItemEvent` and matched to the open question by issue ref + thread. Echo suppression and the
  delivery-id idempotency from §4.2 apply unchanged.
- **Provenance:** the PO's comment (id / url / author / timestamp) is recorded as the `human_decision`
  source on the Investigation answer, giving an auditable external sign-off trail.

Privilege boundary: the PO can ANSWER and (later) sign off via the tracker; they can never trigger
execution. The Architect reviews the ingested answer locally, approves tradeoffs, and runs the agents.
This is exactly why V1 needs no central OAuth, no multi-tenant DB, and no hosted compute.

Provider is the Architect's CHOICE, not a default (clarified 2026-06-13). The clarify-bridge connects to
whichever ONE tracker the Architect/PO primarily live in: Jira, Azure DevOps Boards, GitHub Issues, or the
native tracker. There is no presumed link between trackers, and no provider is privileged at the product
level. Because the bridge targets where the PO lives (the PRODUCT tracker), and a team's code host may
differ from its product tracker (code on GitHub, product in Jira is common), the bridge provider is chosen
independently of the code host. All four are first-class behind the one `WorkItemProvider` port; only the
ORDER in which adapters get implemented is a build pick.

Build-order consequence (answers the "how does this reprioritize" question): the async clarify-bridge (over
the connected provider, whichever it is) moves AHEAD of the full multi-feature dashboard in the V1 plan, but
stays BEHIND the minimal local cockpit (the Architect must be able to drive the engine solo first).
Recommended V1 order: engine (PHASE0 T0-T14) -> minimal local cockpit (proves the cross-stack seam) ->
async clarify-bridge for the prioritized board adapters -> full dashboard. The clarify-bridge runs on the
BOARD axis (the PO's habitat), so its first shipping adapters are Jira and Azure DevOps Boards (§3), not
GitHub Issues. GitHub leads the separate CODE-HOST axis (PR links, gate-result writeback), with Azure
DevOps Repos second. The two axes are chosen independently per deployment; remaining adapters on each axis
follow incrementally behind the same port. (GitHub Issues may serve as a cheap mechanical test-harness for
the bridge during development, but is not a first shipping board adapter.)

---

## 1. The port (recommended architecture)

Question: what is the shape that lets one orchestrator talk to native, Jira,
Azure DevOps Boards, and GitHub Issues/Projects without core knowing which?

Recommendation: a single TypeScript interface, `WorkItemProvider`, that core
depends on. Core holds a `WorkItemProvider`; it never imports `jira`,
`azure-devops`, `github`, or `native` code. The native tracker is just the
in-process implementation of this same interface (see §6).

Why: the central design tension (is the tracker source-of-truth or a
mirror?) is only tractable if the canonical Story vocabulary is defined ONCE,
ours, and providers map to and from it. The provider variance (Jira ADF custom
fields, Azure DevOps work-item fields, GitHub Projects v2 single-select option
ids) is exactly what the adapter's field-mapping layer is for. If core ever
imported a provider shape, every provider quirk would leak into the spine.

Alternatives rejected: (a) per-provider bespoke integrations with no shared
port. Rejected: re-implements intake, status mapping, and loop avoidance N
times and couples the Story DAG to whichever tracker shipped first. (b) Adopt a
provider's schema as our canonical model (e.g. model everything as a Jira
issue). Rejected: rents our canonical execution/provenance state to a schema we
do not control, and no single provider can model Provenance / RuleSet / Gate.

### Canonical shapes (ours; providers map to/from these)

```ts
// Our vocabulary. Providers never see this leak; they map to/from it.
type FeatureStatusValue =
  | 'INTAKE' | 'INVESTIGATING' | 'AWAITING_CLARIFICATION'
  | 'PLANNED' | 'EXECUTING' | 'GATING'
  | 'AWAITING_QA' | 'SIGNED_OFF' | 'DONE'
  | 'BLOCKED' | 'REJECTED';

interface CanonicalStory {
  id: string;                 // our Story id (canonical spine)
  externalRef?: ExternalRef;  // present when linked to a tracker
  title: string;
  description: string;
  status: FeatureStatusValue; // derived from Task/Gate roll-up
  createdBy: string;
  // per-field provenance of last write, for echo suppression
  fieldOrigins: Partial<Record<keyof CanonicalStory, 'ours' | 'tracker'>>;
}

interface ExternalRef {
  provider: 'native' | 'jira' | 'azure-devops' | 'github';
  externalId: string;         // issue key / work-item id / node id
  url: string;
  revision?: string;          // etag / version / rev for echo suppression
}

interface PrLink {
  repo: string;
  url: string;
  title: string;
  status: 'open' | 'merged' | 'closed';
}

interface FeatureStatusReport {     // what we push back
  status: FeatureStatusValue;
  prLinks: PrLink[];                // N PRs for a multi-repo feature
  gateResults: { ruleId: string; result: 'pass' | 'fail'; message?: string }[];
  signOff?: { by: string; at: string };
  provenanceUrl: string;            // link to the FULL trail in our store
}

// Normalized inbound event (adapter produces this from a raw webhook/poll row)
interface InboundWorkItemEvent {
  ref: ExternalRef;
  kind: 'created' | 'updated' | 'commented' | 'status-changed';
  fields: Partial<Pick<CanonicalStory, 'title' | 'description' | 'status'>>;
  deliveryId: string;        // idempotency key (X-GitHub-Delivery, etc.)
  isEcho: boolean;           // matched against expected-echo table
  occurredAt: string;
}
```

### The interface

```ts
interface WorkItemProvider {
  readonly kind: 'native' | 'jira' | 'azure-devops' | 'github';

  // Intake: pull an external issue in as a canonical Story.
  ingestStory(ref: ExternalRef): Promise<CanonicalStory>;

  // Outbound: status / column transition + the minimum-credible payload.
  pushStatus(ref: ExternalRef, report: FeatureStatusReport): Promise<void>;

  // Outbound: post/refresh the structured provenance summary (one editable comment).
  postProvenance(ref: ExternalRef, report: FeatureStatusReport): Promise<void>;

  // State translation, both directions, driven by per-provider config.
  mapStateInbound(native: string): FeatureStatusValue;
  mapStateOutbound(status: FeatureStatusValue): string;

  // Inbound. subscribe() registers a webhook where supported and returns a
  // disposer; handleWebhook() verifies signature + normalizes; poll() is the
  // reconciliation fallback driven by a server-side cursor.
  subscribe?(opts: SubscribeOpts): Promise<Subscription>;
  handleWebhook?(raw: RawRequest): Promise<InboundWorkItemEvent | null>;
  poll(cursor: string | null): Promise<{ events: InboundWorkItemEvent[]; cursor: string }>;
}

interface Subscription { id: string; refreshAt?: string; dispose(): Promise<void>; }
```

Three notes fall straight out of the research:

- `subscribe` / `handleWebhook` are OPTIONAL because the native provider has no
  webhook; it raises events in-process. Native implements `poll` trivially
  against our own store and is a no-op for signature verification.
- `Subscription.refreshAt` is load-bearing FOR JIRA ONLY: Jira dynamic webhooks
  expire after 30 days (verified), so the Jira adapter sets `refreshAt` ~25 days
  out and a timer calls the Extend Webhook Life API before it lapses. No other
  provider populates this field.
- `mapStateInbound` / `mapStateOutbound` read a per-provider CONFIG table
  because tracker columns/statuses are user-defined on all three external
  providers (verified for each: Jira statusCategory, ADO stateCategory, GitHub
  single-select option ids).

---

## 2. Source-of-truth vs mirror

Question (central tension): is the external tracker the source of truth (we
sync to it) or a mirror (our Story spine is canonical, the tracker is a
projection)?

Recommendation: our Story spine is ALWAYS canonical internally. The tracker is a
MIRROR by default. But the authoritative side is configurable PER FIELD via a
`SyncPolicy` map. Provenance, gate results, PR links, and sign-off are ALWAYS
ours and never configurable: a tracker must never overwrite a gate result.

```ts
type AuthoritativeSide = 'ours' | 'tracker';
interface SyncPolicy {
  title: AuthoritativeSide;        // brownfield -> 'tracker'
  description: AuthoritativeSide;  // brownfield -> 'tracker'
  status: AuthoritativeSide;       // brownfield -> 'tracker', greenfield -> 'ours'
  // ALWAYS 'ours', never configurable:
  //   provenance, gateResults, prLinks, signOff
}
```

When it flips:

- Greenfield / native / solo: every field `ours`. The Story originates in
  Camerata; the tracker (if any) is a pure projection.
- Brownfield enterprise: intake fields (`title`, `description`, `status`) flip
  to `tracker`. The issue ORIGINATES on their board; we ingest it; their board
  stays process-of-record for the fields their team lives in. Provenance, gates,
  PR link, and sign-off stay ours and we push them onto the issue.

Why this and not a global "source of truth" flag: a per-field direction is the
structural loop-breaker. A field that both sides could edit is impossible
because each field has exactly one authoritative side, so neither side
overwrites the other. A global flag cannot express "their status, our gate
results," which is exactly the brownfield intake case.

Alternatives rejected: (a) tracker is always source of truth. Rejected:
provenance / RuleSet / Gate have no home in any tracker schema, so it is not
even expressible. (b) we are always source of truth, tracker is write-only.
Rejected: kills the brownfield "ingest a Story FROM Jira" direction and
ignores that enterprise teams will not abandon their board.

Note on the per-provider lean (does not change the rule, informs the default
`SyncPolicy`): Azure DevOps leans toward staying authoritative (it is a team's
process of record, reconciled via the work-item `rev`), so its default flips
intake fields to `tracker`. GitHub leans "tracker authoritative for intake +
sign-off, ours for execution/provenance." Jira defaults to mirror with
`statusCategory` as the safe two-way seam. All three are just different default
`SyncPolicy` rows behind the same port.

---

## 3. Per-provider comparison and build order

Question: which provider do we build first, and what does each cost?

Recommendation (REFINED 2026-06-13 into TWO INDEPENDENT AXES). The provider
model is not one list; it is two, and they get different priorities because the
code host and the product tracker are different concerns that often live in
different systems (code on GitHub + product in Jira is a common enterprise
shape). A Story records BOTH a code-host ref and a board ref, independently.

- **Code-host axis** (repos, branches, PRs, CI; the governed-diff -> PR link +
  gate-result writeback): build **GitHub first**, then **Azure DevOps Repos**.
  GitHub is the safe default because most teams host code there, and its App
  webhook auth is the cleanest of all providers (native HMAC, distinct bot
  identity, per-installation rate budget). ADO Repos follows at low marginal
  cost because the ADO Boards adapter is being built on the other axis anyway.
- **Board axis** (the PRODUCT tracker where the PO lives: story intake, the
  clarify-bridge of §0.5, status sync): build **Jira AND Azure DevOps Boards
  first** (the two most-used enterprise story trackers), with the **native**
  tracker always available, and **GitHub Issues/Projects DEPRIORITIZED**. GitHub
  Issues skews OSS / small-team and is underused as a formal enterprise board,
  so building it first would prove the bridge mechanically but not where real
  POs work.

Honest engineering note: the board adapters prioritized here (Jira, ADO Boards)
are HEAVIER to build than GitHub Issues (Jira needs OAuth 3LO + a ~25-day
webhook-refresh cron; ADO Service Hooks carry no HMAC, see the table). GitHub
Issues stays available as a cheap mechanical TEST-HARNESS for the clarify-bridge
during development if wanted, but the first SHIPPING board adapters are Jira and
ADO Boards. `native` (§6) is still built first of all, because it forces the
canonical shapes both axes map to.

| Dimension | GitHub (Issues/Projects v2) | Jira Cloud | Azure DevOps Boards |
|---|---|---|---|
| Recommended auth | GitHub App (installation token, auto-rotating, distinct bot identity, per-install rate budget) | API token + Basic auth for solo/dogfood; OAuth 2.0 3LO for shareable/webhooks | PAT for solo/dogfood; Microsoft Entra ID OAuth for product |
| App registration needed? | Yes, one-time (PEM private key + permissions + webhook secret) | No for API-token path; yes for 3LO | No for PAT; yes for Entra app |
| Inbound webhook | `issues` event, native HMAC `X-Hub-Signature-256`. BUT `projects_v2_item` is ORG-only (not user accounts) | Dynamic webhook needs 3LO + `manage:jira-webhook`; expires every 30 days, mandatory refresh cron; API-token path CANNOT register one | Service Hooks subscription (`workitem.created/updated`); NO built-in HMAC, auth is a secret you set on the subscription |
| Inbound poll fallback | REST `GET /issues?since=` with ETag (304s are free) or GraphQL connection | `GET /rest/api/3/search/jql` with `updated >= -Nm`; the ONLY inbound path on the API-token auth model | WIQL `[Changed Date] > lastPoll`; costs TSTUs, needs no admin grant |
| Outbound writeback | Editable issue comment + labels (always available); Projects v2 single-select via GraphQL (mirror) | ADF comment (one editable rollup) + custom fields; status via transitions API; remote link for PR | JSON-Patch `System.State` + Comments API; `ArtifactLink`/`Hyperlink` relations for PR |
| Status-mapping seam | User-defined single-select; map by option NAME -> option id, discovered per project, cached | Map to `statusCategory` key (`new` / `indeterminate` / `done`), NOT status name; pick a legal transition into that category | Map to `stateCategory` token (`Proposed`/`InProgress`/`Resolved`/`Completed`/`Removed`), NOT state name; resolve concrete name at runtime |
| Status-mapping friction | Low: GraphQL field discovery, but `projects_v2_item` inbound reconciliation is org-only | Medium: ADF builder + ADF->text extractor required; custom fields are id-addressed; "no legal transition" must degrade to comment | Medium: Resolved category absent for Scrum PBI / Basic Issue, must degrade gracefully (tag + comment) |
| Rate limits | 5,000 pts/hr/install REST; GraphQL 5,000 pts/hr + 2,000 pts/min secondary, each Projects mutation = 5 pts | Points-based hourly quota (order-of-magnitude ~65k/hr, tune to live headers); 429 with `Retry-After` + `RateLimit-Reason` | 200 TSTUs per sliding 5-min window per identity; `Retry-After` honored |

Why GitHub leads the CODE-HOST axis (beyond "code lives there"):

- The PR/diff link and CI gate results are the highest-value writeback, and on
  GitHub they post into the exact issue/PR the work already lives on. PR and
  Issue share the node namespace, so the same comment/label tooling works on
  both.
- The GitHub App gives a distinct bot identity, which keeps the provenance trail
  clean (FeatureStatus comments authored by the bot, not by the human), and a
  per-installation rate budget that does not steal from the human's quota.
- Webhook security is native HMAC (`X-Hub-Signature-256`), the strongest of the
  three. Jira API-token path cannot register webhooks at all; ADO has no HMAC.

Verified GitHub caveat that shapes the dogfood: `projects_v2_item` webhooks are
delivered only for ORGANIZATION-owned Projects v2; user-account projects do not
emit them to a GitHub App, and the org "Projects" permission cannot even be
granted on a personal account. The `issues` webhook, by contrast, fires on any
repo (user or org). Consequence for a personally-owned repo: the inbound
"human dragged a card / changed Projects status" path will NOT fire. The fix is
either (a) treat GitHub Projects as WRITE-ONLY outbound (canonical inbound is
the `issues` webhook + comments + labels, which all work on personal repos), or
(b) move the repos under a free GitHub org, which unlocks `projects_v2_item`
and the full bidirectional Projects loop. Recommendation: (b) if a
Projects-board-as-source-of-truth is ever desired; (a) is fine for the dogfood
because intake-via-issue and status-via-comment/label are the load-bearing
channels anyway.

---

## 4. Open design questions

### 4.1 Webhook vs poll (plus reconciliation)

**Local-deployment reality (architect decision, 2026-06-13). Outbound is never a
problem; only INBOUND has the wrinkle.** Camerata is the architect's LOCAL tool; it
reaches OUT to the trackers. Everything outbound, commenting on a story, editing it,
tagging the PO, managing git remote/origin via the GitHub API, is just the local machine
making API calls, and works everywhere with zero infra. The only piece that hits a wall is
INBOUND notifications (the PO replied): a webhook is the tracker calling IN to a URL, and a
local machine has no public URL. Therefore:

- **Inbound default = POLL** (no public ingress needed, works on any machine; the PO's
  answer arrives within the poll cadence, a couple of minutes, rather than instantly).
- **Inbound webhook = OPT-IN upgrade** for users who run a tunnel (ngrok / cloudflared) or
  later host a relay. Webhooks lower latency but require reachability the base local tool
  does not have, so they are not the V1 default.

Recommendation (transport, once reachability is satisfied): webhook-primary with a
low-frequency reconciliation poll as the safety net. For the V1 LOCAL tool, reachability
is usually NOT satisfied, so poll is the operating default and the safety net collapses
into the primary path; the webhook path is the documented upgrade. The poll runs on a slow
cadence (every few minutes) and catches missed deliveries and expiry gaps regardless.

Why: webhooks give low latency and do not burn the rate budget on idle poll
loops, but each provider has a failure mode that a poll covers:

- GitHub explicitly RETRIES on 5xx/timeout and exposes a manual Redeliver
  button, so duplicate deliveries are guaranteed, not theoretical. Idempotency
  on the `X-GitHub-Delivery` GUID (unique constraint; seen id returns 200 and is
  dropped) is mandatory.
- Jira dynamic webhooks EXPIRE after 30 days and die SILENTLY (verified). If the
  refresh cron misses, only the reconciliation poll catches the gap. And on the
  simplest Jira auth (API token + Basic), webhooks cannot be registered at all
  (verified), so polling is the ONLY inbound path on that auth model.
- ADO has no HMAC; verification is the secret you set on the subscription. Orgs
  that block hook creation (needs project-admin) force the WIQL poll fallback.

Poll cursor per provider: GitHub `since` / updated filter, Jira JQL
`updated >= cursor`, ADO WIQL `[Changed Date] >= cursor`. Each adapter owns its
own cursor and high-water mark.

Alternatives rejected: (a) poll-only everywhere AS A GENERAL RULE. Rejected for
hosted/reachable deployments: needless latency and rate-budget burn where webhooks are
free and reliable (GitHub). NOTE this is the general transport rule; for the V1 LOCAL tool
poll-default is correct precisely because reachability is absent (see the local-deployment
reality above), so the two are not in tension. (b) webhook-only. Rejected: Jira's silent
30-day death and guaranteed GitHub redeliveries make a reconciliation poll non-optional,
and a local tool may have no webhook path at all.

### 4.2 Bidirectional status mapping and loop avoidance

Recommendation: map our `FeatureStatusValue` to each provider's STABLE category
abstraction, never to user-renamed status names, via a per-provider config
table. Break sync loops with TWO independent guards.

The stable seam per provider (all three verified):

- Jira: `statusCategory`, a FIXED non-configurable set: `new` (To Do, id 2),
  `indeterminate` (In Progress, id 4), `done` (Done, id 3). Map to the category
  key, then `GET /issue/{key}/transitions` and pick a legal transition whose
  target status is in the desired category. If no legal transition exists
  (workflow guards), degrade to a comment/field and flag the human; never force
  an illegal transition. Defensive note: the raw `/statuscategory` endpoint also
  returns a sentinel `undefined` / "No Category" (id 1); treat any unexpected
  key as unmapped rather than assuming exactly three.
- Azure DevOps: `stateCategory`, process-independent:
  `Proposed | InProgress | Resolved | Completed | Removed` (match the API enum
  tokens exactly, no spaces, "Completed" not "Complete"). Resolve concrete state
  names at runtime from the states-list API per work-item type and cache them.
  Resolved is ABSENT for Scrum PBI and Basic Issue, so "awaiting QA" must
  degrade to InProgress + a tag/comment when the process lacks Resolved.
- GitHub Projects v2: the Status field is a user-defined single-select; each
  option carries its own node id. There is no fixed category set, so map by
  option NAME (operator-overridable) to the discovered option id, cached per
  project. Where the board lacks a column we want (e.g. no "Blocked"), labels +
  the status comment are the lossless channel and the Projects field is
  best-effort.

Canonical mapping (our vocabulary -> category, defaults, overridable):

| Camerata FeatureStatus | Jira category | ADO stateCategory | GitHub option (default) |
|---|---|---|---|
| INTAKE / not started | `new` | Proposed | Todo / Backlog |
| INVESTIGATING / PLANNED / EXECUTING / GATING | `indeterminate` | InProgress | In Progress |
| Gate FAIL (bounced to agent) | stay `indeterminate` + comment | stay InProgress + comment | In Progress + `camerata:gate-failed` label |
| AWAITING_QA | `indeterminate` (custom "In Review" lives here) | Resolved (degrade to InProgress + tag if absent) | In Review |
| SIGNED_OFF / DONE | `done` | Completed | Done |

Loop avoidance, two independent guards:

1. Per-field direction (from §2): a field has exactly one authoritative side, so
   a field both sides could edit is impossible by construction. This is the
   structural loop-breaker, independent of timing.
2. Echo suppression via expected-revision: every outbound write records
   `{ ref, expectedRevision, writtenAt }` in an expected-echo table. When the
   resulting webhook/poll row arrives, the adapter sets `isEcho = true` if it
   matches the expected revision (GitHub delivery id, Jira issue version, ADO
   `rev`), and core drops it. Replays are deduped on `deliveryId` with a unique
   constraint.

Why two guards and not one: per-field direction stops the "both sides own the
field" war; echo suppression stops "our own write bounces back in as a new
external edit." They cover different failure modes, so both are needed.

### 4.3 Multi-repo -> single issue rollup

Recommendation: a feature spanning N repos produces N PRs that roll up onto ONE
tracker work item. `FeatureStatusReport.prLinks` is an array; `pushStatus`
renders all N as a checklist in the single editable status comment (per PR:
repo, link, open/merged/closed). The repo set is derived downstream during
plan/execute, NOT read from the tracker; the tracker holds one issue = one
Story.

Why: the issue is the unit of human intake; the multi-repo fan-out is an
orchestration detail our Story spine owns. FeatureStatus is DONE only when every
PR is merged and every gate passed; that roll-up logic is ours, the tracker
shows the projection. Each PR carries a back-link to the canonical Story id so
provenance reconciles from either direction.

Provider mechanics: GitHub closing keywords (`Closes #123`) only link on the
DEFAULT branch and do NOT span repos, so for cross-repo features post an
explicit cross-reference comment and let the Story spine hold the real
multi-repo-to-one-Story map. ADO supports multiple `ArtifactLink` / `Hyperlink`
relations on one work item (one PR link per repo). Jira uses a remote link per
PR plus the rollup comment.

### 4.4 What gets posted back (PR link vs full provenance)

Recommendation: post the MINIMUM-CREDIBLE trail onto the external item, with the
full provenance addressable behind ONE link. The minimum-credible payload is:
(1) the governed PR/diff link(s), (2) per-gate pass/fail (rule id + result), and
(3) the human sign-off (who, when). The full trail (which agent/session produced
each change, every rule passed, full gate messages) lives in OUR store and is
linked via `provenanceUrl`, not dumped inline.

Why: enough on the board to be trustworthy as process-of-record, without
flooding the issue or coupling our internal schema to theirs. All of it goes in
ONE editable "Camerata status" comment that the adapter updates in place (GitHub
`updateComment`, Jira `PUT .../comment/{id}`, ADO Comments API), never a new
comment per tick. Status transitions use the provider's transition API (Jira
workflow transitions, ADO `System.State` PATCH, GitHub Projects single-select /
issue open-close), never a raw illegal state write. Sign-off is recorded both as
a status transition (the process-of-record form) AND an audit comment naming who
approved.

---

## 5. Why the native tracker is not separate work

Question: is the in-built native Story board a second product to build?

Recommendation: no. Direction B (the native Story board) IS the `native`
implementation of the same `WorkItemProvider` port. It backs onto our own Story
/ Provenance store (per VISION: the provenance store design), raises events in-process (no webhook, no
signature path), and its `poll` reads our own store.

Why this is not extra work, and actually de-risks the external adapters:

- Because core only ever talks to the port, the native provider forces the
  canonical `CanonicalStory` / `FeatureStatusReport` shapes and the per-field
  `SyncPolicy` to be correct BEFORE any provider-specific auth/webhook mess is
  introduced. Getting native right is getting the contract right.
- The native provider is the greenfield/solo "ours is source of truth" case
  (every `SyncPolicy` field `ours`), which is the simplest configuration of the
  same machinery. The external providers are then just different `SyncPolicy`
  defaults plus an auth + webhook + field-mapping adapter.
- It keeps the tool self-sufficient (greenfield/solo) and keeps the canonical
  Story / Provenance / RuleSet state ours, which the native-board design goal explicitly wants.

Alternatives rejected: a standalone native board built outside the port.
Rejected: it would duplicate the Story lifecycle and let the canonical shapes
drift from what the external adapters need, reintroducing exactly the coupling
the port exists to prevent.

---

## 6. Phased rollout

This is post-Phase-0. It does NOT alter the thin slice; Phase 0 stays the one
input box (§2.1). The port and providers below are sequenced AFTER the thin
slice is working.

- Phase A: native provider. Implement `WorkItemProvider` for the in-process
  store. Lock the canonical `CanonicalStory` / `FeatureStatusReport` shapes and
  the per-field `SyncPolicy`. Direction: ours-canonical, every field `ours`. No
  webhook, no auth. This is the contract-defining phase.
- Phase B: GitHub provider, OUTBOUND first. GitHub App auth (PEM + permissions +
  webhook secret). Push FeatureStatus + PR link + gate results + sign-off as one
  editable issue comment + labels. Default `SyncPolicy`: tracker authoritative
  for intake + sign-off, ours for execution/provenance. This is the highest-value
  first external slice because the code, PRs, and CI already live there.
- Phase C: GitHub provider, INBOUND. `issues` webhook (HMAC verified) ingesting
  an issue as a Story, plus the reconciliation poll. Projects v2 mirroring
  outbound via GraphQL; reconcile inbound only if the repo moves under an org
  (`projects_v2_item` is org-only). Add echo suppression + delivery-id
  idempotency here, where the first real two-way loop appears.
- Phase D: Jira provider. Start on the API-token + Basic path (zero app
  registration) with JQL-polling inbound, since that auth model cannot register
  webhooks. Add the OAuth 2.0 3LO app + dynamic webhook + the mandatory ~25-day
  refresh cron only when shareability/low-latency is needed. Map to
  `statusCategory`; build the ADF builder + extractor.
- Phase E: Azure DevOps provider. PAT auth (scopes Work Items RW + Code R for
  Service Hook creation) with Service Hooks inbound and WIQL-poll fallback. Map
  to `stateCategory`, resolve concrete names at runtime, degrade gracefully where
  Resolved is absent. Plan the Entra ID OAuth migration for any hosted/multi-tenant
  future; do NOT build on the dead Azure DevOps OAuth app model.

Build order rationale: native de-risks the contract; GitHub is the dogfood
provider and the cleanest auth; Jira and ADO follow because their auth and
status-mapping friction is higher and they are not where the code lives.

---

## 7. Unverified assumptions and open risks

These are the claims the verdicts did NOT confirm, plus anything that could not
be verified. Treat each as a runtime check or a flagged assumption, not a fact.

- GitHub user-vs-org Projects webhook scope (load-bearing for the dogfood). The
  live docs still say `projects_v2_item` is organization-level only, but the
  verifier could not load the full 2025-2026 tail of the tracking discussion and
  relied on the absence of a reversal. If GitHub silently shipped user-project
  webhooks, the personal-account inbound Projects loop might work. Re-verify
  before committing the bidirectional Projects loop on a personal account.
- Whether the repos currently sit under a GitHub org or a personal account.
  This determines whether the full Projects sync is available out of the box. If
  personal, the recommended unlock is a free org (zero cost).
- Whether fine-grained GitHub PATs now cover the Projects v2 GraphQL API in
  2026. On GitHub's roadmap, GA not confirmed. Design assumes GitHub App for org
  boards and classic PAT (`project` scope) as the fallback for user boards;
  verify before relying on a fine-grained PAT for Projects.
- GitHub read-cost formula is "divide by 100 and round to NEAREST," not strict
  ceiling. The "ceil(nodes/100), min 1" shorthand is close enough for budgeting
  but is not the exact wording. The 2,000-pts/min secondary cap is GitHub's
  documented limit and may be adjusted; treat as a budget, not a contract.
- Jira global point-pool number (~65k/hr) is widely cited but Atlassian tunes
  it. Treat as order-of-magnitude; read live `X-RateLimit-*` / `RateLimit-Reason`
  headers at runtime. Some Jira 429s historically lacked `Retry-After`, so keep
  a fixed backoff floor; do not depend on the header being present.
- Jira 3LO authorization URL params (`prompt=consent`, exact `audience`) are from
  the standard flow; confirm against the console-generated URL for the specific
  app, which Atlassian pre-builds.
- Jira webhook refresh endpoint path (`PUT /rest/api/3/webhook/refresh`): the
  +30-day refresh SEMANTICS are confirmed, but the verifier could not fully
  render the api-group reference page. The behavior is confirmed; double-check
  the exact path string at build time.
- Precision note for the integration doc (defensibility): the Jira docs do NOT
  contain a literal sentence "an API token cannot register a webhook." They gate
  the operation to Connect / OAuth 2.0 app identities, which excludes Basic-auth
  API tokens by construction. State it that way under scrutiny.
- Azure DevOps OAuth registration cutoff is April 23, 2025 (NOT "March 2025" as
  one finding stated); existing apps reach full deprecation in 2026 but Microsoft
  has NOT published the exact end-of-life day. Do not hard-code a 2026 cutoff
  date.
- Azure DevOps PAT end-of-life: none found. PATs are documented as
  supported-but-discouraged ("Maintenance mode"), not scheduled for removal as
  of the Apr 2026 Entra auth doc. Flagged as not having a hard EOL date.
- Azure DevOps `stateCategory` is typed as a plain `string`, not a frozen enum.
  Code defensively: route any unrecognized value to a safe default rather than
  throwing. A work-item payload carries only `System.State`; you must join that
  string to the per-work-item-type states-list to derive its category.
- Azure DevOps Comments API version is pinned at `7.1-preview.4` in current docs;
  confirm the GA version at build time. The GitHub-PR-relation `rel`/url shape
  depends on the Azure Boards-to-GitHub connection being present; the plain
  Hyperlink relation always works as a fallback.
- Azure DevOps webhook signature: ADO has NO GitHub-style HMAC. Verification is
  the basic-auth creds / custom header you set on the subscription. The exact
  secret mechanism should be confirmed against current docs before implementing
  the verifier; the adapter pins a shared secret at subscription time as the
  working assumption. The May 2026 Service Hooks "update" is a docs/security
  refresh, not a breaking change to payloads or auth.
