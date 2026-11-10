//! VRAM-budget placement engine (the multi-GPU scheduler "brain").
//!
//! This is the config-driven per-device bin-packer. There is **no** heavy/light
//! lane any more — every GPU service is uniform. The [`crate::GpuLock`] holds one
//! async mutex over a [`Placer`] and consults it on every `acquire`; the only
//! synchronization is VRAM-budget admission.
//!
//! Model:
//! - A [`Card`] list (uuid, total_mb, fast, enabled) — loaded from koblem's DB.
//! - A per-service [`EngineDesc`] (peak_mb, home_gpu, candidate_gpus,
//!   evictable_by, relocatable, rules_json) — also loaded from the DB, editable
//!   live. Editing a descriptor re-syncs any resident placement of that kind
//!   ([`Placer::set_engine`]), so a live peak/eviction-rule edit takes effect
//!   without a restart.
//! - Placement reserves a service's **tested worst-case peak**, not its
//!   instantaneous usage: `budget_free = total − Σ(peak of resident placements)`.
//!   Admission additionally respects a **measured** free-VRAM floor when koblem
//!   has pushed one (`set_measured_free`) so an under-set peak or a non-gate VRAM
//!   consumer can't be scheduled over: `free = min(budget_free, measured)`. No
//!   headroom is taken off the measurement: each service's `peak_mb` is already
//!   padded, so a second margin only refuses work that fits.
//! - Eviction is **named + relational, not numeric**: each service declares, by
//!   service name, who may evict it (`evictable_by`).
//! - Each [`Placement`] carries a [`PlacementSource`] and a `holders` refcount.
//!   A guard-held placement is freed only when its **last** guard drops
//!   (concurrent same-kind acquires share one placement — freeing on the first
//!   drop would strand the others). `Standing` (the LLM seed) and `Observed`
//!   (out-of-band reconcile) placements are never freed by a guard drop.
//! - A single [`std::sync::Mutex`] guards the placement table — held only for the
//!   admit/evict decision, never across an `.await`.
//!
//! The chosen card UUID is handed back to the caller, which threads it into the
//! C++ server's per-request `gpu` field.

use std::collections::HashMap;
use std::fmt;
use std::sync::{Arc, Mutex};

use crate::ServiceKind;

/// A physical GPU the scheduler may place work on. Seeded from koblem's `gpu_cards`
/// table; `enabled=false` removes it from placement and drains it (the operator's
/// way to keep serving on one card while another is taken out for other work).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Card {
    /// GPU UUID (e.g. `GPU-3b9ac5cf-…`). This is what gets sent to the C++ server
    /// in the per-request `gpu` field — **switch by UUID, never index**.
    pub uuid: String,
    /// Total VRAM in MiB (reported `memory.total`).
    pub total_mb: u32,
    /// Faster card. Drives `fast`-first spill ranking.
    pub fast: bool,
    /// Operator enable flag. Disabled cards are excluded from placement and
    /// drained.
    pub enabled: bool,
}

/// Per-service placement descriptor. Loaded from koblem's `gpu_engine_config`
/// table and pushed into the gate at startup + on every edit, so tuning the peak
/// table is a form field, not a recompile.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EngineDesc {
    /// Tested worst-case peak VRAM at the configured `--max-vram` (MiB). Reserved
    /// in full while the service is resident. Wrong peaks ⇒ OOM, so this is
    /// load-bearing config, not a hint.
    pub peak_mb: u32,
    /// Home card UUID: where un-targeted / direct requests land, and the first
    /// placement candidate. Empty = "no preference" (use candidate order only).
    pub home_gpu: String,
    /// Cards the gate may place this service on, in preference order (home first,
    /// then spill). A card not in this list is never chosen for this service.
    pub candidate_gpus: Vec<String>,
    /// **Named, relational eviction rule.** The set of service kinds that are
    /// allowed to evict *this* service to make room. "flux can evict the LLM" =
    /// `llm.evictable_by` contains `Flux`. When requester `R` needs space on a
    /// card, this service is a candidate victim there iff `R ∈ evictable_by`.
    /// An **empty** set means this service is never auto-evicted — the requester
    /// blocks instead (this subsumes the old `pinned` flag). There is no numeric
    /// priority comparison anywhere in the eviction decision.
    pub evictable_by: Vec<ServiceKind>,
    /// On preempt/drain, respawn elsewhere instead of being dropped (e.g. the
    /// LLM). The C++ server relocates itself when the `gpu` field changes; for
    /// non-relocatable services preemption means drain/cancel.
    pub relocatable: bool,
    /// Reserved for future conditional eviction rules ("if only one card is
    /// enabled, any large service may evict the LLM"). Stored + surfaced in the
    /// UI; not interpreted by the placer yet.
    pub rules_json: String,
}

/// Where a resident [`Placement`] came from — decides whether a guard drop may
/// free it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlacementSource {
    /// A normal koblem acquire owns this. Freed when the last guard drops.
    Guard,
    /// A standing, always-resident reservation (the LLM seed). Never freed by a
    /// guard drop; only an operator disable or a card removal clears it.
    Standing,
    /// Recorded out-of-band by the reconcile loop from a server's announce
    /// endpoint. Freed only when reconcile later sees the worker unloaded, or an
    /// operator disable clears it.
    Observed,
}

/// A resident placement: service `kind` reserving `peak_mb` on a card.
#[derive(Clone, Debug)]
struct Placement {
    kind: ServiceKind,
    /// Card UUID this placement reserves space on.
    card_uuid: String,
    peak_mb: u32,
    /// Who may evict this resident (copied from its [`EngineDesc`] and kept in
    /// sync by [`Placer::set_engine`]). Empty = never auto-evicted.
    evictable_by: Vec<ServiceKind>,
    /// Cards this service is allowed to run on (copied from its [`EngineDesc`]).
    /// Constrains where a relocation may move it in the normal-eviction path.
    candidate_gpus: Vec<String>,
    relocatable: bool,
    source: PlacementSource,
    /// Number of live guards using this placement. A `Guard` placement is removed
    /// when this hits zero; `Standing`/`Observed` ignore it for removal.
    holders: u32,
}

// A short-lived copy of the mutable residency ledger. GPU admission takes one
// before it schedules external drain/unload side effects; if a service cannot
// prove its worker exited, the gate restores this exact ledger instead of
// admitting another peak on top of unknown VRAM residency.
pub(crate) struct PlacementCheckpoint {
    placements: Vec<Placement>,
}

impl Placement {
    /// Can requester `by` evict this resident? True iff `by` is named in this
    /// service's `evictable_by` set — the whole eviction decision in one line.
    fn evictable_by(&self, by: ServiceKind) -> bool {
        self.evictable_by.contains(&by)
    }
}

/// The always-resident services ([`ServiceKind::is_standing`]: the LLM and the
/// text embedder): however their placement is created (seed, fresh admit,
/// relocation, or observe) it is `Standing` so no guard drop ever frees its
/// budget out from under the warm worker.
fn source_for(kind: ServiceKind, requested: PlacementSource) -> PlacementSource {
    if kind.is_standing() {
        PlacementSource::Standing
    } else {
        requested
    }
}

/// Why a placement attempt cannot proceed *right now*.
///
/// The queue's dispatcher stamps this on a job row and reconsiders next tick, so
/// it is a description of a transient state, never a failure. Every variant names
/// what has to change for it to clear.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PlaceBlock {
    /// No enabled card is a candidate for this service — every configured
    /// candidate is administratively disabled (or the candidate list is empty).
    /// Clears when a card is re-enabled.
    NoEnabledCard,
    /// An operator pin named a card that is unknown or administratively disabled.
    PinUnavailable { card: String },
    /// An operator pin named a card, but the service is already resident on a
    /// different one. The gate never moves a resident behind its own back;
    /// clears when the current placement drops.
    PinnedElsewhere { resident_on: String, want: String },
    /// Every candidate is full and nothing resident there names this requester in
    /// its `evictable_by`. Clears when budget frees, a resident drops, or the
    /// service's `peak_mb` is edited down.
    NoCardFits {
        needed_mb: u32,
        /// Free budget on the roomiest candidate (may be negative once a measured
        /// clamp is applied).
        best_free_mb: i64,
        /// Residents on that candidate this requester is not allowed to evict —
        /// the answer to "waiting because of what".
        blockers: Vec<ServiceKind>,
    },
}

impl fmt::Display for PlaceBlock {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PlaceBlock::NoEnabledCard => write!(f, "no enabled card is a candidate"),
            PlaceBlock::PinUnavailable { card } => write!(f, "pinned card {card} is disabled"),
            PlaceBlock::PinnedElsewhere { resident_on, want } => {
                write!(f, "already resident on {resident_on}, pinned to {want}")
            }
            PlaceBlock::NoCardFits {
                needed_mb,
                best_free_mb,
                blockers,
            } => {
                if blockers.is_empty() {
                    write!(
                        f,
                        "no card fits {needed_mb} MiB (best free {best_free_mb} MiB)"
                    )
                } else {
                    let names: Vec<&str> = blockers.iter().map(|k| k.as_str()).collect();
                    write!(
                        f,
                        "no card fits {needed_mb} MiB (best free {best_free_mb} MiB; {} resident)",
                        names.join(", ")
                    )
                }
            }
        }
    }
}

/// Where a placement attempt may land, and under what override.
#[derive(Clone, Copy, Debug, Default)]
pub struct PlaceRequest<'a> {
    /// Operator pin: consider only this card. `None` = normal ranked candidates.
    pub pin: Option<&'a str>,
    /// Ignore the victims' `evictable_by` rule on the chosen card (the operator's
    /// "stop the LLM" button). Never overrides the never-SIGKILL-in-flight rule.
    pub force: bool,
}

/// Outcome of a [`Placer::try_place`] decision the caller must act on.
#[derive(Clone, Debug, Default)]
pub struct PlaceDecision {
    /// The card UUID the service was admitted to. `None` = could not place right
    /// now (caller blocks on the notify until budget frees).
    pub card_uuid: Option<String>,
    /// `true` when this acquire reused an existing resident placement for the same
    /// kind (no eviction side-effects to fire). The guard still releases on drop
    /// (decrementing the refcount), so the caller must NOT skip release.
    pub reused: bool,
    /// Service kinds that must be evicted (drained + unloaded) to make room.
    /// These are non-relocatable victims whose render is cancelled.
    pub evict: Vec<ServiceKind>,
    /// Relocatable victims (e.g. the LLM) that should respawn on `relocate_to`.
    /// The caller drains+kills them; the C++ server respawns on its next request
    /// targeting the new card. Each entry is `(kind, new_card_uuid_or_none)`.
    /// `None` = nowhere to relocate (treat as a plain evict).
    pub relocate: Vec<(ServiceKind, Option<String>)>,
}

/// The card-list + placement-table owner. Cheap to clone (Arc inside).
#[derive(Clone, Default)]
pub struct Placer {
    inner: Arc<Mutex<Inner>>,
}

#[derive(Default)]
struct Inner {
    cards: Vec<Card>,
    engines: HashMap<ServiceKind, EngineDesc>,
    placements: Vec<Placement>,
    /// Per-card measured free VRAM (MiB) pushed by koblem's reconcile loop. When
    /// present it clamps admission below the pure-budget free. Absent = fall back
    /// to pure budget (fully back-compatible when no measurement source exists).
    measured_free_mb: HashMap<String, u32>,
    /// Exclusive-mode hold: a keeper that spans cards the ledger cannot describe.
    /// `None` = normal scheduling, today's arithmetic exactly.
    hold: Option<Hold>,
}

/// A keeper whose real footprint is a per-card operator number, not its ledger row.
///
/// 🔴 Beefy is ONE service spread across cards, and a [`Placement`] carries one
/// `card_uuid` — so the keeper's ledger row is wrong in both directions (its whole
/// peak on one card, nothing on the others). While a hold is set, each card's used
/// budget is `reserved + Σ(every OTHER resident's peak)`: the reservation stands in
/// for the keeper there, so the keeper's own row is not counted twice.
///
/// 🔴 A card with NO entry is reserved WHOLE. Fail-safe by construction: a
/// mis-typed UUID or a card added mid-hold reproduces "the box is locked", never
/// an open door.
#[derive(Clone, Debug)]
struct Hold {
    keeper: ServiceKind,
    reserved_mb: HashMap<String, u32>,
}

impl Placer {
    pub fn new() -> Self {
        Self::default()
    }

    pub(crate) fn checkpoint(&self) -> PlacementCheckpoint {
        PlacementCheckpoint {
            placements: self.inner.lock().unwrap().placements.clone(),
        }
    }

    pub(crate) fn restore_checkpoint(&self, checkpoint: PlacementCheckpoint) {
        self.inner.lock().unwrap().placements = checkpoint.placements;
    }

    /// Replace the card list wholesale (koblem loads this from the `gpu_cards`
    /// table at startup + on edit). Placements on a card that disappears are
    /// dropped from the ledger (the operator-disable path drains them first).
    pub fn set_cards(&self, cards: Vec<Card>) {
        let mut inner = self.inner.lock().unwrap();
        let live: Vec<String> = cards.iter().map(|c| c.uuid.clone()).collect();
        inner.placements.retain(|p| live.contains(&p.card_uuid));
        inner.measured_free_mb.retain(|uuid, _| live.contains(uuid));
        inner.cards = cards;
    }

    /// Replace the per-service config table wholesale.
    pub fn set_engines(&self, engines: HashMap<ServiceKind, EngineDesc>) {
        let mut inner = self.inner.lock().unwrap();
        for (kind, desc) in &engines {
            inner.sync_placement(*kind, desc);
        }
        inner.engines = engines;
    }

    /// Upsert one service's descriptor (live edit from the settings page). Also
    /// re-syncs any resident placement of that kind so a peak / eviction-rule /
    /// candidate edit takes effect on the live ledger immediately (the LLM's
    /// standing placement in particular — otherwise it keeps its boot-time rule).
    pub fn set_engine(&self, kind: ServiceKind, desc: EngineDesc) {
        let mut inner = self.inner.lock().unwrap();
        inner.sync_placement(kind, &desc);
        inner.engines.insert(kind, desc);
    }

    /// Set (`Some`) or clear (`None`) the exclusive-mode hold: `keeper`'s footprint
    /// on each card is `reserved_mb[card]`, and a card absent from the map is
    /// reserved whole. While set, `keeper` is never chosen as an eviction victim
    /// and its own admission skips the budget check. See [`Hold`].
    pub fn set_hold(&self, hold: Option<(ServiceKind, HashMap<String, u32>)>) {
        self.inner.lock().unwrap().hold = hold.map(|(keeper, reserved_mb)| Hold {
            keeper,
            reserved_mb,
        });
    }

    /// Push a card's measured free VRAM (MiB) from koblem's reconcile poll.
    pub fn set_measured_free(&self, uuid: &str, free_mb: u32) {
        let mut inner = self.inner.lock().unwrap();
        if inner.card(uuid).is_some() {
            inner.measured_free_mb.insert(uuid.to_string(), free_mb);
        }
    }

    /// Snapshot the card list (drives the settings UI + occupancy endpoint).
    pub fn cards(&self) -> Vec<Card> {
        self.inner.lock().unwrap().cards.clone()
    }

    /// True only when the card is known AND administratively disabled. An unknown
    /// UUID is not "disabled" (the reconcile backstop leaves unrecognized cards
    /// alone, matching `observe`'s unknown-card no-op).
    pub fn is_card_disabled(&self, uuid: &str) -> bool {
        let inner = self.inner.lock().unwrap();
        inner
            .cards
            .iter()
            .find(|c| c.uuid == uuid)
            .is_some_and(|c| !c.enabled)
    }

    /// Snapshot one service's descriptor.
    pub fn engine(&self, kind: ServiceKind) -> Option<EngineDesc> {
        self.inner.lock().unwrap().engines.get(&kind).cloned()
    }

    /// Snapshot every configured service descriptor.
    pub fn engines(&self) -> Vec<(ServiceKind, EngineDesc)> {
        self.inner
            .lock()
            .unwrap()
            .engines
            .iter()
            .map(|(k, d)| (*k, d.clone()))
            .collect()
    }

    /// Is the placement layer active? It is a no-op (returns `None` card, callers
    /// fall back to env defaults) until koblem seeds at least one card AND a
    /// config for the requesting service.
    pub fn is_configured(&self) -> bool {
        let inner = self.inner.lock().unwrap();
        !inner.cards.is_empty() && !inner.engines.is_empty()
    }

    /// Whether a service is configured for placement at all.
    pub fn has_engine(&self, kind: ServiceKind) -> bool {
        self.inner.lock().unwrap().engines.contains_key(&kind)
    }

    /// Default (home) card UUID for a service — used for un-targeted requests and
    /// as the env-default mirror. Falls back to the first candidate, then the
    /// first enabled card. `None` if nothing is configured.
    pub fn default_gpu(&self, kind: ServiceKind) -> Option<String> {
        let inner = self.inner.lock().unwrap();
        let desc = inner.engines.get(&kind)?;
        if !desc.home_gpu.is_empty() && inner.card_enabled(&desc.home_gpu) {
            return Some(desc.home_gpu.clone());
        }
        desc.candidate_gpus
            .iter()
            .find(|u| inner.card_enabled(u))
            .cloned()
            .or_else(|| {
                inner
                    .cards
                    .iter()
                    .find(|c| c.enabled)
                    .map(|c| c.uuid.clone())
            })
    }

    /// Try to place a service. Three outcomes:
    /// - `None` — placement layer unconfigured for this kind: caller proceeds
    ///   with no card target (server uses its env default, fully back-compatible).
    /// - `Some(decision)` with `card_uuid == Some` — admitted (possibly after
    ///   recording evictions/relocations the caller must fire); a [`Placement`] is
    ///   in the ledger (or its refcount bumped on reuse) and the caller must
    ///   [`Placer::release`] on guard drop.
    /// - `Some(decision)` with `card_uuid == None` — configured but no room even
    ///   after evicting every resident that names this requester in its
    ///   `evictable_by`: caller **blocks** on the notify and retries.
    pub fn try_place(&self, kind: ServiceKind) -> Option<PlaceDecision> {
        self.try_place_with(kind, PlaceRequest::default())
            .map(|r| r.unwrap_or_default())
    }

    /// [`Self::try_place`] with an operator pin / force override.
    ///
    /// Outer `None` still means "unconfigured for this kind". The inner
    /// `Result` distinguishes an admission (`Ok`) from a *reasoned* refusal
    /// (`Err`), which is what the queue stamps on a blocked row — the plain
    /// `try_place` collapses `Err` back to an empty decision for the legacy
    /// block-and-retry callers.
    pub fn try_place_with(
        &self,
        kind: ServiceKind,
        req: PlaceRequest<'_>,
    ) -> Option<Result<PlaceDecision, PlaceBlock>> {
        let mut inner = self.inner.lock().unwrap();
        inner.plan(kind, req)
    }

    /// Pure query: would `kind` place right now, and if not, why?
    ///
    /// Runs the *same* planner as [`Self::try_place_with`] against the live ledger
    /// and then rolls back, so the answer can never drift from what an admission
    /// would actually do. Reserves nothing.
    pub fn why_blocked(&self, kind: ServiceKind, req: PlaceRequest<'_>) -> Option<PlaceBlock> {
        let mut inner = self.inner.lock().unwrap();
        let snapshot = inner.placements.clone();
        let outcome = inner.plan(kind, req);
        inner.placements = snapshot;
        match outcome {
            // Unconfigured for placement: acquire is a no-op guard, never a block.
            None => None,
            Some(Ok(_)) => None,
            Some(Err(block)) => Some(block),
        }
    }

    /// Live guard count for a service's placement (0 when idle or absent). The
    /// queue planner uses this to tell a warm-but-idle resident (safe to unload
    /// early) from one that is mid-render.
    pub fn holders(&self, kind: ServiceKind) -> u32 {
        self.inner
            .lock()
            .unwrap()
            .placements
            .iter()
            .find(|p| p.kind == kind)
            .map_or(0, |p| p.holders)
    }

    /// Would any of `by` be allowed to evict a resident `kind`? Answers the
    /// queue's "is the work I am about to dispatch going to boot this service off
    /// its card anyway" question without attempting a placement.
    pub fn evictable_by_any(&self, kind: ServiceKind, by: &[ServiceKind]) -> bool {
        let inner = self.inner.lock().unwrap();
        let rule = match inner.placements.iter().find(|p| p.kind == kind) {
            Some(p) => &p.evictable_by,
            None => match inner.engines.get(&kind) {
                Some(d) => &d.evictable_by,
                None => return false,
            },
        };
        by.iter().any(|k| rule.contains(k))
    }

    /// Release one guard's hold on a service's placement (called from the guard's
    /// Drop). Decrements the refcount; removes the placement only when the last
    /// guard drops AND it is `Guard`-sourced. Returns `true` if a placement was
    /// actually removed (so the caller knows to wake waiters).
    pub fn release(&self, kind: ServiceKind) -> bool {
        let mut inner = self.inner.lock().unwrap();
        let Some(pos) = inner.placements.iter().position(|p| p.kind == kind) else {
            return false;
        };
        let p = &mut inner.placements[pos];
        p.holders = p.holders.saturating_sub(1);
        if p.holders == 0 && p.source == PlacementSource::Guard {
            inner.placements.remove(pos);
            true
        } else {
            false
        }
    }

    /// Force-remove any placement for `kind` regardless of source or refcount
    /// (the operator explicitly disabled the service / took its card). Returns
    /// `true` if one was removed.
    pub fn force_remove(&self, kind: ServiceKind) -> bool {
        let mut inner = self.inner.lock().unwrap();
        let before = inner.placements.len();
        inner.placements.retain(|p| p.kind != kind);
        inner.placements.len() != before
    }

    /// Out-of-band reconcile: record that `kind` is resident on `card_uuid` (a
    /// worker observed via a server's announce endpoint). Reserves the service's
    /// peak so the scheduler won't admit on top of it. No-op if the kind is
    /// unconfigured or the card is unknown. Never clobbers a `Guard`/`Standing`
    /// placement (koblem owns those); only adds/moves an `Observed` one.
    pub fn observe(&self, kind: ServiceKind, card_uuid: &str) {
        let mut inner = self.inner.lock().unwrap();
        let Some(desc) = inner.engines.get(&kind).cloned() else {
            return;
        };
        if inner.card(card_uuid).is_none() {
            return;
        }
        // Never legitimize a placement on an administratively disabled card. A
        // worker can only land there by bypassing admission (a request path that
        // omitted the `gpu` field → the worker's WORKER_DEFAULT_GPU). Leaving the
        // ledger clean lets the reconcile backstop drain it off the card.
        if !inner.card_enabled(card_uuid) {
            return;
        }
        match inner.placements.iter_mut().find(|p| p.kind == kind) {
            Some(p) if p.card_uuid == card_uuid => {}
            Some(p) if p.source == PlacementSource::Observed => {
                // An out-of-band worker moved cards — follow it.
                p.card_uuid = card_uuid.to_string();
            }
            Some(_) => {
                // A Guard/Standing placement owns this kind — don't override
                // koblem's view from a stale announce.
            }
            None => inner.record(kind, card_uuid, &desc, PlacementSource::Observed, 0),
        }
    }

    /// Out-of-band reconcile: a service's announce endpoint reports it is NOT
    /// loaded → drop its placement ONLY if it is `Observed` (never a live
    /// guard-held or standing reservation). Returns `true` if removed.
    pub fn unobserve_observed(&self, kind: ServiceKind) -> bool {
        let mut inner = self.inner.lock().unwrap();
        let Some(pos) = inner
            .placements
            .iter()
            .position(|p| p.kind == kind && p.source == PlacementSource::Observed)
        else {
            return false;
        };
        inner.placements.remove(pos);
        true
    }

    /// Establish a *standing* (always-resident) placement for a service on its
    /// home card, reserving its peak even though no guard holds it. Used for the
    /// LLM: it's resident, so its peak must count against the home card's budget
    /// or a spilling heavy would admit on top of it and OOM. A no-op if the
    /// service isn't configured or its home card isn't enabled. Re-callable
    /// (replaces the existing standing placement). Returns the card UUID.
    pub fn seed_resident(&self, kind: ServiceKind) -> Option<String> {
        let mut inner = self.inner.lock().unwrap();
        let desc = inner.engines.get(&kind)?.clone();
        let card = if !desc.home_gpu.is_empty() && inner.card_enabled(&desc.home_gpu) {
            desc.home_gpu.clone()
        } else {
            desc.candidate_gpus
                .iter()
                .find(|u| inner.card_enabled(u))
                .cloned()?
        };
        // Preserve an in-flight guard's refcount if one is already holding it.
        let holders = inner
            .placements
            .iter()
            .find(|p| p.kind == kind)
            .map(|p| p.holders)
            .unwrap_or(0);
        inner.record(kind, &card, &desc, PlacementSource::Standing, holders);
        Some(card)
    }

    /// Reclaim the **home** card for a dropped standing service (the LLM). When the
    /// service is not resident anywhere (an evictor booted it off with nowhere to
    /// relocate) and its home card is enabled with room again, re-seed its standing
    /// reservation there — so it reclaims its keep-warm home as soon as the
    /// contention clears, without waiting for the next request. Deliberately
    /// **home-only**: if the home is still busy it stays dropped (it does not roam
    /// to a spill card on its own — an actual request still can, on demand). No-op
    /// if it's already resident (warm) or the home has no room. Returns the card.
    pub fn rehome_standing(&self, kind: ServiceKind) -> Option<String> {
        let mut inner = self.inner.lock().unwrap();
        let desc = inner.engines.get(&kind)?.clone();
        if inner.placements.iter().any(|p| p.kind == kind) {
            return None; // still resident (warm) — nothing to reclaim
        }
        if desc.home_gpu.is_empty() || !inner.card_enabled(&desc.home_gpu) {
            return None;
        }
        if inner.free_mb(&desc.home_gpu) < desc.peak_mb as i64 {
            return None; // home still contended — stay dropped
        }
        inner.record(
            kind,
            &desc.home_gpu.clone(),
            &desc,
            PlacementSource::Standing,
            0,
        );
        Some(desc.home_gpu)
    }

    /// Toggle a card's enabled flag. Disabling returns the placements that must
    /// be drained off it (with relocation targets where possible); the caller
    /// performs the drains. Card-disable drains *every* resident regardless of its
    /// `evictable_by` rule (the operator explicitly took the card away). Refuses to
    /// disable the last enabled card (returns `Err`). Re-enabling is a no-op beyond
    /// flipping the flag.
    pub fn set_card_enabled(&self, uuid: &str, enabled: bool) -> Result<PlaceDecision, String> {
        let mut inner = self.inner.lock().unwrap();
        let Some(idx) = inner.cards.iter().position(|c| c.uuid == uuid) else {
            return Err(format!("unknown card: {uuid}"));
        };
        if !enabled {
            let enabled_count = inner.cards.iter().filter(|c| c.enabled).count();
            if inner.cards[idx].enabled && enabled_count <= 1 {
                return Err("cannot disable the last enabled GPU".to_string());
            }
        }
        inner.cards[idx].enabled = enabled;

        if enabled {
            return Ok(PlaceDecision::default());
        }

        inner.measured_free_mb.remove(uuid);

        // Drain everything resident on the disabled card. Relocate what fits onto
        // another enabled card; cancel the rest. (The `evictable_by` rule does NOT
        // apply here — the operator explicitly took the card away, so every
        // resident is drained. Relocation is NOT candidate-restricted here: the
        // operator forced the move, so any enabled card that fits is fair game.)
        let resident: Vec<Placement> = inner
            .placements
            .iter()
            .filter(|p| p.card_uuid == uuid)
            .cloned()
            .collect();
        let mut decision = PlaceDecision::default();
        for v in resident {
            inner
                .placements
                .retain(|p| !(p.kind == v.kind && p.card_uuid == v.card_uuid));
            if v.relocatable {
                let new_home = inner.relocation_target(&v, uuid, false);
                if let Some(ref nh) = new_home {
                    inner.record_raw(&v, nh);
                }
                decision.relocate.push((v.kind, new_home));
            } else {
                decision.evict.push(v.kind);
            }
        }
        Ok(decision)
    }

    /// Live occupancy snapshot for the UI + the announce/reconcile story: per-card
    /// total, free budget, measured free VRAM (when known), and the resident
    /// services (kind + reserved peak).
    pub fn occupancy(&self) -> Vec<CardOccupancy> {
        let inner = self.inner.lock().unwrap();
        inner
            .cards
            .iter()
            .map(|c| {
                let residents: Vec<(ServiceKind, u32)> = inner
                    .placements
                    .iter()
                    .filter(|p| p.card_uuid == c.uuid)
                    .map(|p| (p.kind, p.peak_mb))
                    .collect();
                // The same number admission uses, so the page cannot disagree with
                // the gate — under a hold that is NOT the sum of `residents`.
                let used = u32::try_from(inner.used_mb(&c.uuid).max(0)).unwrap_or(u32::MAX);
                CardOccupancy {
                    uuid: c.uuid.clone(),
                    total_mb: c.total_mb,
                    fast: c.fast,
                    enabled: c.enabled,
                    used_mb: used,
                    free_mb: c.total_mb.saturating_sub(used),
                    measured_free_mb: inner.measured_free_mb.get(&c.uuid).copied(),
                    reserved_mb: inner.hold.as_ref().map(|h| inner.reserved_on(h, c)),
                    residents,
                }
            })
            .collect()
    }

    /// Service kinds that are **actively serving a request** right now — a
    /// placement with a live guard (`holders > 0`). This is the true "busy"
    /// signal: it excludes an idle-but-loaded `Observed` worker and the warm
    /// `Standing` LLM seed, both of which reserve budget while doing nothing.
    pub fn active_kinds(&self) -> Vec<ServiceKind> {
        self.inner
            .lock()
            .unwrap()
            .placements
            .iter()
            .filter(|p| p.holders > 0)
            .map(|p| p.kind)
            .collect()
    }
}

/// One card's live occupancy (drives the settings page + occupancy endpoint).
#[derive(Clone, Debug)]
pub struct CardOccupancy {
    pub uuid: String,
    pub total_mb: u32,
    pub fast: bool,
    pub enabled: bool,
    /// `Σ(resident peaks)`, or under an exclusive hold `reserved + Σ(every other
    /// resident's peak)` — in that case NOT the sum of `residents`.
    pub used_mb: u32,
    /// Pure-budget free: `total − used_mb`.
    pub free_mb: u32,
    /// Measured real free VRAM (MiB), when koblem has pushed a reading. `None`
    /// means no measurement source — admission falls back to pure budget.
    pub measured_free_mb: Option<u32>,
    /// The exclusive keeper's reservation on this card while a hold is set
    /// (`total_mb` when the card was not given one). `None` = no hold.
    pub reserved_mb: Option<u32>,
    /// Resident services and the peak each reserves.
    pub residents: Vec<(ServiceKind, u32)>,
}

impl Inner {
    /// The one placement algorithm. Both admission and the "why is this waiting"
    /// query run it; the query rolls the ledger back afterwards, so there is no
    /// second implementation to drift.
    ///
    /// Outer `None` = unconfigured for placement.
    fn plan(
        &mut self,
        kind: ServiceKind,
        req: PlaceRequest<'_>,
    ) -> Option<Result<PlaceDecision, PlaceBlock>> {
        let desc = self.engines.get(&kind)?.clone();

        // 1. Already resident → reuse its card, bump the refcount (no new budget,
        //    no side-effects). The guard still releases on drop; the placement is
        //    removed only when the LAST guard drops (and it's Guard-sourced).
        if let Some(pos) = self.placements.iter().position(|p| p.kind == kind) {
            let card = self.placements[pos].card_uuid.clone();
            // An operator pin never silently lands somewhere else. The gate does
            // not move a resident behind its own back (that would mean unloading a
            // possibly-busy worker as a side effect of a *query*), so say so and
            // let the caller wait for the current placement to drop.
            if let Some(pin) = req.pin
                && pin != card
            {
                return Some(Err(PlaceBlock::PinnedElsewhere {
                    resident_on: card,
                    want: pin.to_string(),
                }));
            }
            self.placements[pos].holders += 1;
            return Some(Ok(PlaceDecision {
                card_uuid: Some(card),
                reused: true,
                ..Default::default()
            }));
        }

        // Candidate cards in preference order: home/fast first, then by most free.
        // A pin replaces that ranking entirely — the operator asked for one card.
        // It must still be an ENABLED card: a disabled card is off limits to every
        // path, override or not.
        let candidates = match req.pin {
            Some(pin) => {
                if !self.card_enabled(pin) {
                    return Some(Err(PlaceBlock::PinUnavailable {
                        card: pin.to_string(),
                    }));
                }
                vec![pin.to_string()]
            }
            None => self.ranked_candidates(&desc),
        };
        if candidates.is_empty() {
            // No enabled candidate at all — caller waits until one comes back
            // (e.g. a disabled home card is re-enabled).
            return Some(Err(PlaceBlock::NoEnabledCard));
        }

        // 2. Admit on the first candidate that already has room. The held keeper
        //    skips the check: its footprint IS the hold's reservation, and a keeper
        //    whose row was dropped mid-hold must not park forever behind its own
        //    reservation.
        let is_keeper = self.hold.as_ref().is_some_and(|h| h.keeper == kind);
        for card in &candidates {
            if is_keeper || self.free_mb(card) >= desc.peak_mb as i64 {
                self.record(kind, card, &desc, PlacementSource::Guard, 1);
                return Some(Ok(PlaceDecision {
                    card_uuid: Some(card.clone()),
                    ..Default::default()
                }));
            }
        }

        // 3. Preempt on the best candidate: among the residents that NAME this
        //    requester in their `evictable_by`, can their removal cover the
        //    deficit? Prefer relocatable victims (cheap respawn) over cancelling a
        //    running non-relocatable render. No numeric priority compare — the
        //    eviction relation is entirely the named `evictable_by` set, except
        //    under an operator `force`, which overrides that rule and nothing else.
        let keeper = self.hold.as_ref().map(|h| h.keeper);
        for card in &candidates {
            let mut victims: Vec<Placement> = self
                .placements
                .iter()
                // 🔴 Never the held keeper, force or not. Its row is a stand-in (see
                // [`Hold`]), so "evicting" it frees no budget here — and its unload
                // URL is the large model's, so firing it would take down the model the hold
                // exists to protect. The LLM names stt/vision/tts in `evictable_by`,
                // so without this line every allowed co-tenant would do exactly that.
                .filter(|p| Some(p.kind) != keeper)
                // Never KILL in-flight work: a NON-relocatable placement with a live guard
                // (`holders > 0`) is mid-render/mid-job and can't be moved, so evicting it =
                // SIGKILL the render. Skip it — the requester blocks until that guard drops
                // (the render finishes), then evicts the now-idle resident. Relocatable
                // victims (the LLM) may still be moved even in-flight (relocation ≠ kill), and
                // idle residents (`holders == 0`, e.g. a warm-but-idle Standing placement like
                // the seeded LTX-2B or LLM) are always evictable. `force` deliberately does
                // NOT relax this clause: it overrides the operator's own eviction policy, not
                // the physical fact that a render is running.
                .filter(|p| {
                    p.card_uuid == *card
                        && (req.force || p.evictable_by(kind))
                        && (p.holders == 0 || p.relocatable)
                })
                .cloned()
                .collect();
            // A standing service that cannot move (the always-on text embedder)
            // goes LAST: every other caller on the box depends on it, so it is the
            // victim of last resort. Then relocatable first (cheapest to evict —
            // move not kill), then largest peak first so the greedy loop frees the
            // deficit by evicting the FEWEST residents.
            let last_resort = |p: &Placement| p.kind.is_standing() && !p.relocatable;
            victims.sort_by(|a, b| {
                last_resort(a)
                    .cmp(&last_resort(b))
                    .then(b.relocatable.cmp(&a.relocatable))
                    .then(b.peak_mb.cmp(&a.peak_mb))
            });

            let freeable: i64 = victims.iter().map(|v| v.peak_mb as i64).sum();
            if self.free_mb(card) + freeable < desc.peak_mb as i64 {
                continue; // even evicting everyone evictable here isn't enough
            }

            // Evict victims greedily until we fit.
            let mut decision = PlaceDecision {
                card_uuid: Some(card.clone()),
                ..Default::default()
            };
            for v in victims {
                if self.free_mb(card) >= desc.peak_mb as i64 {
                    break;
                }
                self.placements
                    .retain(|p| !(p.kind == v.kind && p.card_uuid == v.card_uuid));
                if v.relocatable {
                    // Normal-eviction relocation must respect the victim's own
                    // candidate list — never move a service onto a card the
                    // operator excluded for it.
                    let new_home = self.relocation_target(&v, card, true);
                    if let Some(ref nh) = new_home {
                        self.record_raw(&v, nh);
                    }
                    decision.relocate.push((v.kind, new_home));
                } else {
                    decision.evict.push(v.kind);
                }
            }

            self.record(kind, card, &desc, PlacementSource::Guard, 1);
            return Some(Ok(decision));
        }

        // 4. Couldn't place anywhere right now → report against the roomiest
        //    candidate, naming the residents this requester may not evict. That
        //    string is what the queue page shows as "waiting: …".
        let best = candidates
            .iter()
            .max_by_key(|c| self.free_mb(c))
            .cloned()
            .unwrap_or_default();
        let blockers: Vec<ServiceKind> = self
            .placements
            .iter()
            .filter(|p| p.card_uuid == best)
            .map(|p| p.kind)
            .collect();
        Some(Err(PlaceBlock::NoCardFits {
            needed_mb: desc.peak_mb,
            best_free_mb: self.free_mb(&best),
            blockers,
        }))
    }

    fn card_enabled(&self, uuid: &str) -> bool {
        self.cards.iter().any(|c| c.uuid == uuid && c.enabled)
    }

    fn card(&self, uuid: &str) -> Option<&Card> {
        self.cards.iter().find(|c| c.uuid == uuid)
    }

    /// Sum of resident peaks on a card — under a [`Hold`], the reservation plus
    /// every resident's peak EXCEPT the keeper's (the reservation stands in for it).
    fn used_mb(&self, uuid: &str) -> i64 {
        let skip = self.hold.as_ref().map(|h| h.keeper);
        let peaks: i64 = self
            .placements
            .iter()
            .filter(|p| p.card_uuid == uuid && Some(p.kind) != skip)
            .map(|p| p.peak_mb as i64)
            .sum();
        let reserved = match (&self.hold, self.card(uuid)) {
            (Some(h), Some(c)) => self.reserved_on(h, c) as i64,
            _ => 0,
        };
        reserved + peaks
    }

    /// A card's reservation under `hold`: its own entry, or the WHOLE card.
    fn reserved_on(&self, hold: &Hold, card: &Card) -> u32 {
        hold.reserved_mb
            .get(&card.uuid)
            .copied()
            .unwrap_or(card.total_mb)
    }

    /// Effective free VRAM for admission: the pure budget (`total − Σ peaks`)
    /// clamped by the measured real free when a measurement
    /// exists. Disabled / unknown card → 0 free.
    fn free_mb(&self, uuid: &str) -> i64 {
        let budget_free = match self.card(uuid) {
            Some(c) if c.enabled => c.total_mb as i64 - self.used_mb(uuid),
            _ => return 0,
        };
        match self.measured_free_mb.get(uuid) {
            Some(&measured) => budget_free.min(measured as i64),
            None => budget_free,
        }
    }

    /// Candidate card UUIDs for a service, in preference order: home first (if
    /// enabled + in candidates), then the configured candidates ranked by
    /// (fast desc, free desc). Only enabled cards.
    fn ranked_candidates(&self, desc: &EngineDesc) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        if !desc.home_gpu.is_empty()
            && desc.candidate_gpus.contains(&desc.home_gpu)
            && self.card_enabled(&desc.home_gpu)
        {
            out.push(desc.home_gpu.clone());
        }
        let mut rest: Vec<String> = desc
            .candidate_gpus
            .iter()
            .filter(|u| self.card_enabled(u) && **u != desc.home_gpu)
            .cloned()
            .collect();
        rest.sort_by(|a, b| {
            let fa = self.card(a).map(|c| c.fast).unwrap_or(false);
            let fb = self.card(b).map(|c| c.fast).unwrap_or(false);
            fb.cmp(&fa).then(self.free_mb(b).cmp(&self.free_mb(a)))
        });
        out.extend(rest);
        out
    }

    /// Best enabled card (other than `exclude`) that can host a relocatable
    /// victim at its peak. When `respect_candidates`, only the victim's own
    /// `candidate_gpus` are eligible (normal eviction); when not, any enabled
    /// card is (operator-forced card-disable drain). `None` = nowhere fits (the
    /// victim is cancelled instead).
    fn relocation_target(
        &self,
        victim: &Placement,
        exclude: &str,
        respect_candidates: bool,
    ) -> Option<String> {
        self.cards
            .iter()
            .filter(|c| c.enabled && c.uuid != exclude)
            .filter(|c| !respect_candidates || victim.candidate_gpus.contains(&c.uuid))
            .filter(|c| self.free_mb(&c.uuid) >= victim.peak_mb as i64)
            .max_by_key(|c| self.free_mb(&c.uuid))
            .map(|c| c.uuid.clone())
    }

    fn record(
        &mut self,
        kind: ServiceKind,
        card_uuid: &str,
        desc: &EngineDesc,
        source: PlacementSource,
        holders: u32,
    ) {
        // One placement per kind: a re-place replaces the old ledger entry.
        self.placements.retain(|p| p.kind != kind);
        self.placements.push(Placement {
            kind,
            card_uuid: card_uuid.to_string(),
            peak_mb: desc.peak_mb,
            evictable_by: desc.evictable_by.clone(),
            candidate_gpus: desc.candidate_gpus.clone(),
            relocatable: desc.relocatable,
            source: source_for(kind, source),
            holders,
        });
    }

    /// Re-home an existing placement (relocation) preserving its peak, rule,
    /// source, and refcount.
    fn record_raw(&mut self, victim: &Placement, card_uuid: &str) {
        self.placements.retain(|p| p.kind != victim.kind);
        self.placements.push(Placement {
            kind: victim.kind,
            card_uuid: card_uuid.to_string(),
            peak_mb: victim.peak_mb,
            evictable_by: victim.evictable_by.clone(),
            candidate_gpus: victim.candidate_gpus.clone(),
            relocatable: victim.relocatable,
            source: victim.source,
            holders: victim.holders,
        });
    }

    /// Re-sync a resident placement's mutable fields to a new descriptor (a live
    /// config edit). Leaves the card + refcount + source alone.
    fn sync_placement(&mut self, kind: ServiceKind, desc: &EngineDesc) {
        if let Some(p) = self.placements.iter_mut().find(|p| p.kind == kind) {
            p.peak_mb = desc.peak_mb;
            p.evictable_by = desc.evictable_by.clone();
            p.candidate_gpus = desc.candidate_gpus.clone();
            p.relocatable = desc.relocatable;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: &str = "GPU-aaaa";
    const B: &str = "GPU-bbbb";

    fn cards() -> Vec<Card> {
        vec![
            Card {
                uuid: A.into(),
                total_mb: 12000,
                fast: false,
                enabled: true,
            },
            Card {
                uuid: B.into(),
                total_mb: 16000,
                fast: true,
                enabled: true,
            },
        ]
    }

    /// Build a descriptor. `evicts` = the service kinds allowed to evict THIS
    /// service (its `evictable_by` set) — the named, relational eviction rule.
    fn desc(
        peak: u32,
        home: &str,
        cands: &[&str],
        evicts: &[ServiceKind],
        reloc: bool,
    ) -> EngineDesc {
        EngineDesc {
            peak_mb: peak,
            home_gpu: home.into(),
            candidate_gpus: cands.iter().map(|s| s.to_string()).collect(),
            evictable_by: evicts.to_vec(),
            relocatable: reloc,
            rules_json: String::new(),
        }
    }

    #[test]
    fn admits_on_home_when_room() {
        let p = Placer::new();
        p.set_cards(cards());
        p.set_engine(ServiceKind::Flux, desc(7500, B, &[B, A], &[], false));
        let d = p.try_place(ServiceKind::Flux).unwrap();
        assert_eq!(d.card_uuid.as_deref(), Some(B));
        assert!(d.evict.is_empty() && d.relocate.is_empty());
    }

    #[test]
    fn checkpoint_restores_a_failed_eviction_admission() {
        let p = Placer::new();
        p.set_cards(vec![Card {
            uuid: A.into(),
            total_mb: 12000,
            fast: false,
            enabled: true,
        }]);
        p.set_engine(
            ServiceKind::Ltx,
            desc(8000, A, &[A], &[ServiceKind::Flux], false),
        );
        p.set_engine(ServiceKind::Flux, desc(7500, A, &[A], &[], false));
        // An observed worker is warm but idle, so Flux is allowed to evict it.
        p.observe(ServiceKind::Ltx, A);
        let checkpoint = p.checkpoint();
        let decision = p.try_place(ServiceKind::Flux).unwrap();
        assert_eq!(decision.evict, vec![ServiceKind::Ltx]);
        assert!(
            p.occupancy()[0]
                .residents
                .iter()
                .any(|(kind, _)| *kind == ServiceKind::Flux)
        );
        p.restore_checkpoint(checkpoint);
        let residents = &p.occupancy()[0].residents;
        assert!(residents.iter().any(|(kind, _)| *kind == ServiceKind::Ltx));
        assert!(!residents.iter().any(|(kind, _)| *kind == ServiceKind::Flux));
    }

    #[test]
    fn resident_reuses_card() {
        let p = Placer::new();
        p.set_cards(cards());
        p.set_engine(ServiceKind::Flux, desc(7500, B, &[B, A], &[], false));
        let _ = p.try_place(ServiceKind::Flux).unwrap();
        let d = p.try_place(ServiceKind::Flux).unwrap();
        assert_eq!(d.card_uuid.as_deref(), Some(B));
        assert!(d.reused, "second acquire of a resident service reuses");
    }

    /// Two concurrent same-kind acquires share one placement; the budget frees
    /// only when the LAST guard releases (the first release must NOT strand the
    /// second still-running request).
    #[test]
    fn same_kind_refcount_frees_on_last_release() {
        let p = Placer::new();
        p.set_cards(cards());
        p.set_engine(ServiceKind::Vision, desc(2000, A, &[A], &[], false));
        let _ = p.try_place(ServiceKind::Vision).unwrap(); // holder 1
        let _ = p.try_place(ServiceKind::Vision).unwrap(); // holder 2 (reuse)
        assert_eq!(p.occupancy()[0].used_mb, 2000);
        assert!(
            !p.release(ServiceKind::Vision),
            "first release keeps budget"
        );
        assert_eq!(
            p.occupancy()[0].used_mb,
            2000,
            "still reserved for 2nd holder"
        );
        assert!(p.release(ServiceKind::Vision), "last release frees budget");
        assert_eq!(p.occupancy()[0].used_mb, 0);
    }

    #[test]
    fn spills_to_second_card_when_home_full() {
        let p = Placer::new();
        p.set_cards(cards());
        p.set_engine(ServiceKind::Flux, desc(7500, B, &[B, A], &[], false));
        // Songgen's peak can't co-reside with Flux on B (7500 + 10000 > 16000), so
        // it must spill to its second candidate A.
        p.set_engine(ServiceKind::Songgen, desc(10000, B, &[B, A], &[], false));
        let d1 = p.try_place(ServiceKind::Flux).unwrap();
        assert_eq!(d1.card_uuid.as_deref(), Some(B));
        let d2 = p.try_place(ServiceKind::Songgen).unwrap();
        assert_eq!(d2.card_uuid.as_deref(), Some(A));
    }

    /// `flux` evicts `llm` because the LLM declares `evictable_by = [flux, songgen]`
    /// — named, not numeric. The LLM is relocatable but its candidate list is
    /// `[A]` only, so it can't move → relocate target is `None` (cancel).
    #[test]
    fn flux_preempts_and_relocates_llm() {
        let p = Placer::new();
        p.set_cards(cards());
        p.set_engine(
            ServiceKind::Llm,
            desc(
                10000,
                A,
                &[A],
                &[ServiceKind::Flux, ServiceKind::Songgen],
                true,
            ),
        );
        p.set_engine(ServiceKind::Flux, desc(7500, B, &[B, A], &[], false));
        p.set_engine(ServiceKind::Songgen, desc(12000, B, &[B], &[], false));
        let _ = p.try_place(ServiceKind::Llm).unwrap();
        let _ = p.try_place(ServiceKind::Songgen).unwrap();
        let d = p.try_place(ServiceKind::Flux).unwrap();
        assert_eq!(d.card_uuid.as_deref(), Some(A));
        assert_eq!(d.relocate.len(), 1);
        assert_eq!(d.relocate[0].0, ServiceKind::Llm);
        assert_eq!(d.relocate[0].1, None);
    }

    /// Relocation respects the victim's `candidate_gpus`: an LLM allowed on both
    /// cards relocates to B; one restricted to A only is cancelled.
    #[test]
    fn relocation_respects_candidate_gpus() {
        let p = Placer::new();
        p.set_cards(cards());
        // LLM allowed on A and B, relocatable, evictable by flux.
        p.set_engine(
            ServiceKind::Llm,
            desc(6000, A, &[A, B], &[ServiceKind::Flux], true),
        );
        p.set_engine(ServiceKind::Flux, desc(9000, A, &[A], &[], false));
        let _ = p.try_place(ServiceKind::Llm).unwrap();
        let d = p.try_place(ServiceKind::Flux).unwrap();
        assert_eq!(d.card_uuid.as_deref(), Some(A));
        assert_eq!(d.relocate.len(), 1);
        assert_eq!(
            d.relocate[0].1.as_deref(),
            Some(B),
            "LLM moves to B (a candidate)"
        );
    }

    /// A service with an EMPTY `evictable_by` is never auto-evicted (subsumes the
    /// old `pinned`): even a renderer that wants the card must block.
    #[test]
    fn empty_evictable_by_is_never_evicted() {
        let p = Placer::new();
        p.set_cards(vec![Card {
            uuid: A.into(),
            total_mb: 12000,
            fast: false,
            enabled: true,
        }]);
        p.set_engine(ServiceKind::Llm, desc(10000, A, &[A], &[], false));
        p.set_engine(ServiceKind::Flux, desc(7500, A, &[A], &[], false));
        let _ = p.try_place(ServiceKind::Llm).unwrap();
        let d = p.try_place(ServiceKind::Flux).unwrap();
        assert_eq!(d.card_uuid, None);
    }

    /// A resident that doesn't name the requester is not a victim, even if other
    /// services could evict it: avatar can't evict the LLM (only flux/songgen can).
    #[test]
    fn non_naming_requester_cannot_evict() {
        let p = Placer::new();
        p.set_cards(vec![Card {
            uuid: A.into(),
            total_mb: 12000,
            fast: false,
            enabled: true,
        }]);
        p.set_engine(
            ServiceKind::Llm,
            desc(
                10000,
                A,
                &[A],
                &[ServiceKind::Flux, ServiceKind::Songgen],
                true,
            ),
        );
        p.set_engine(ServiceKind::Avatar, desc(7500, A, &[A], &[], false));
        let _ = p.try_place(ServiceKind::Llm).unwrap();
        let d = p.try_place(ServiceKind::Avatar).unwrap();
        assert_eq!(d.card_uuid, None);
    }

    #[test]
    fn blocks_when_no_qualifying_victims() {
        let p = Placer::new();
        p.set_cards(vec![Card {
            uuid: A.into(),
            total_mb: 12000,
            fast: false,
            enabled: true,
        }]);
        p.set_engine(ServiceKind::Avatar, desc(7500, A, &[A], &[], false));
        p.set_engine(ServiceKind::Ltx, desc(7500, A, &[A], &[], false));
        let _ = p.try_place(ServiceKind::Avatar).unwrap();
        let d = p.try_place(ServiceKind::Ltx).unwrap();
        assert_eq!(d.card_uuid, None);
    }

    #[test]
    fn disable_card_drains_and_refuses_last() {
        let p = Placer::new();
        p.set_cards(cards());
        p.set_engine(ServiceKind::Vision, desc(2000, A, &[A, B], &[], true));
        let _ = p.try_place(ServiceKind::Vision).unwrap();
        let d = p.set_card_enabled(A, false).unwrap();
        assert_eq!(d.relocate.len(), 1);
        assert_eq!(d.relocate[0].1.as_deref(), Some(B));
        assert!(p.set_card_enabled(B, false).is_err());
    }

    /// Card-disable relocation is NOT candidate-restricted: the operator forced
    /// the move, so a victim pinned to A by candidates still relocates to B.
    #[test]
    fn card_disable_relocates_outside_candidates() {
        let p = Placer::new();
        p.set_cards(cards());
        p.set_engine(ServiceKind::Llm, desc(6000, A, &[A], &[], true));
        let _ = p.try_place(ServiceKind::Llm).unwrap();
        let d = p.set_card_enabled(A, false).unwrap();
        assert_eq!(d.relocate.len(), 1);
        assert_eq!(
            d.relocate[0].1.as_deref(),
            Some(B),
            "forced move to B despite candidates=[A]"
        );
    }

    #[test]
    fn release_frees_budget() {
        let p = Placer::new();
        p.set_cards(vec![Card {
            uuid: A.into(),
            total_mb: 12000,
            fast: false,
            enabled: true,
        }]);
        p.set_engine(ServiceKind::Flux, desc(7500, A, &[A], &[], false));
        let _ = p.try_place(ServiceKind::Flux).unwrap();
        assert_eq!(p.occupancy()[0].used_mb, 7500);
        assert!(p.release(ServiceKind::Flux));
        assert_eq!(p.occupancy()[0].used_mb, 0);
    }

    #[test]
    fn observe_reserves_budget() {
        let p = Placer::new();
        p.set_cards(cards());
        p.set_engine(
            ServiceKind::Llm,
            desc(
                10000,
                A,
                &[A],
                &[ServiceKind::Flux, ServiceKind::Songgen],
                true,
            ),
        );
        p.observe(ServiceKind::Llm, A);
        assert_eq!(
            p.occupancy().iter().find(|c| c.uuid == A).unwrap().used_mb,
            10000
        );
        // Force-remove (operator disable) clears any source.
        assert!(p.force_remove(ServiceKind::Llm));
        assert_eq!(
            p.occupancy().iter().find(|c| c.uuid == A).unwrap().used_mb,
            0
        );
    }

    /// `unobserve_observed` removes an Observed placement but never a live
    /// guard-held one.
    #[test]
    fn unobserve_only_touches_observed() {
        let p = Placer::new();
        p.set_cards(cards());
        p.set_engine(ServiceKind::Vision, desc(2000, A, &[A], &[], false));
        // Guard-held: unobserve_observed must NOT remove it.
        let _ = p.try_place(ServiceKind::Vision).unwrap();
        assert!(!p.unobserve_observed(ServiceKind::Vision));
        assert_eq!(p.occupancy()[0].used_mb, 2000);
        let _ = p.release(ServiceKind::Vision);
        // Observed: unobserve_observed removes it.
        p.observe(ServiceKind::Vision, A);
        assert_eq!(p.occupancy()[0].used_mb, 2000);
        assert!(p.unobserve_observed(ServiceKind::Vision));
        assert_eq!(p.occupancy()[0].used_mb, 0);
    }

    /// A worker that lands on a card the operator disabled (via any request path
    /// that bypassed admission) must NOT be legitimized by `observe`: the ledger
    /// stays clean so the reconcile backstop can evict it off the disabled card.
    #[test]
    fn observe_refuses_disabled_card() {
        let p = Placer::new();
        p.set_cards(cards());
        p.set_engine(ServiceKind::Music, desc(2000, A, &[A, B], &[], true));
        p.set_card_enabled(A, false).unwrap();
        assert!(p.is_card_disabled(A));
        assert!(!p.is_card_disabled(B));
        assert!(
            !p.is_card_disabled("GPU-unknown"),
            "unknown card is not 'disabled'"
        );
        // A stray worker announces itself resident on the disabled card.
        p.observe(ServiceKind::Music, A);
        assert_eq!(
            p.occupancy().iter().find(|c| c.uuid == A).unwrap().used_mb,
            0,
            "observe must not record a placement on a disabled card"
        );
    }

    /// A live `set_engine` edit re-syncs the resident placement's peak + eviction
    /// rule, so the edit changes the very next victim decision.
    #[test]
    fn set_engine_syncs_resident_placement() {
        let p = Placer::new();
        p.set_cards(vec![Card {
            uuid: A.into(),
            total_mb: 12000,
            fast: false,
            enabled: true,
        }]);
        // LLM not evictable by anyone yet.
        p.set_engine(ServiceKind::Llm, desc(10000, A, &[A], &[], true));
        p.seed_resident(ServiceKind::Llm);
        p.set_engine(ServiceKind::Flux, desc(7500, A, &[A], &[], false));
        // Flux can't evict the LLM → blocks.
        assert_eq!(p.try_place(ServiceKind::Flux).unwrap().card_uuid, None);
        // Operator edits the LLM to be evictable by Flux — must apply live.
        p.set_engine(
            ServiceKind::Llm,
            desc(10000, A, &[A], &[ServiceKind::Flux], true),
        );
        let d = p.try_place(ServiceKind::Flux).unwrap();
        assert_eq!(
            d.card_uuid.as_deref(),
            Some(A),
            "edit made the LLM evictable"
        );
    }

    /// The measured-free clamp blocks admission the pure budget would allow.
    #[test]
    fn measured_free_clamp_blocks_admission() {
        let p = Placer::new();
        p.set_cards(vec![Card {
            uuid: A.into(),
            total_mb: 12000,
            fast: false,
            enabled: true,
        }]);
        p.set_engine(ServiceKind::Flux, desc(7500, A, &[A], &[], false));
        // Budget says 12000 free, but the card really only has 1000 MiB free.
        p.set_measured_free(A, 1000);
        assert_eq!(p.try_place(ServiceKind::Flux).unwrap().card_uuid, None);
        // Measured free exactly equal to the peak admits: the peak is the padding.
        p.set_measured_free(A, 7500);
        assert_eq!(
            p.try_place(ServiceKind::Flux).unwrap().card_uuid.as_deref(),
            Some(A)
        );
        // Measurement clears → pure budget admits.
        p.set_measured_free(A, 12000);
        assert_eq!(
            p.try_place(ServiceKind::Flux).unwrap().card_uuid.as_deref(),
            Some(A)
        );
    }

    /// `active_kinds` reports only services with a live guard — the true "busy"
    /// signal. An idle-but-loaded `Observed` worker and the warm `Standing` LLM
    /// seed reserve budget (show as residents in `occupancy`) but are NOT active.
    #[test]
    fn active_kinds_tracks_holders_not_residency() {
        let p = Placer::new();
        p.set_cards(cards());
        p.set_engine(ServiceKind::Flux, desc(7500, B, &[B, A], &[], false));
        p.set_engine(ServiceKind::Llm, desc(10000, A, &[A], &[], true));

        // A flux render holds a guard → active.
        let _ = p.try_place(ServiceKind::Flux).unwrap();
        // The idle LLM standing seed reserves budget but isn't serving.
        p.seed_resident(ServiceKind::Llm);
        assert!(p.active_kinds().contains(&ServiceKind::Flux));
        assert!(
            !p.active_kinds().contains(&ServiceKind::Llm),
            "idle standing LLM is a resident but not busy"
        );

        // Render finishes → flux guard drops. The worker is still loaded in VRAM,
        // so reconcile re-observes it: a resident (shows in occupancy) but idle.
        assert!(p.release(ServiceKind::Flux));
        p.observe(ServiceKind::Flux, B);
        assert!(
            p.occupancy()
                .iter()
                .any(|c| c.residents.iter().any(|(k, _)| *k == ServiceKind::Flux)),
            "observed flux still reserves budget"
        );
        assert!(
            !p.active_kinds().contains(&ServiceKind::Flux),
            "observed-but-idle flux must NOT be busy"
        );

        // A fresh flux request reuses the observed placement, bumping holders → busy.
        let _ = p.try_place(ServiceKind::Flux).unwrap();
        assert!(p.active_kinds().contains(&ServiceKind::Flux));
    }

    /// The LLM placement is Standing however it's created, so a guard drop never
    /// frees its budget.
    #[test]
    fn llm_placement_is_standing() {
        let p = Placer::new();
        p.set_cards(vec![Card {
            uuid: A.into(),
            total_mb: 12000,
            fast: false,
            enabled: true,
        }]);
        p.set_engine(ServiceKind::Llm, desc(10000, A, &[A], &[], true));
        // Fresh admit (not via seed) still Standing.
        let _ = p.try_place(ServiceKind::Llm).unwrap();
        assert_eq!(p.occupancy()[0].used_mb, 10000);
        assert!(
            !p.release(ServiceKind::Llm),
            "guard drop does not free the LLM"
        );
        assert_eq!(
            p.occupancy()[0].used_mb,
            10000,
            "LLM budget survives guard drop"
        );
    }

    /// The text embedder is the other standing service: an observe (its worker
    /// announcing itself) records it Standing, so a later `loaded:false` announce
    /// does not drop the reservation the always-on service depends on.
    #[test]
    fn text_embed_placement_is_standing() {
        let p = Placer::new();
        p.set_cards(cards());
        p.set_engine(ServiceKind::TextEmbed, desc(1600, B, &[B], &[], false));
        p.observe(ServiceKind::TextEmbed, B);
        assert!(!p.unobserve_observed(ServiceKind::TextEmbed));
        assert_eq!(
            p.occupancy()[1].residents,
            vec![(ServiceKind::TextEmbed, 1600)]
        );
    }

    /// Evicted last: when evicting the other qualifying residents covers the
    /// deficit, the always-on text embedder stays, even though peak-first order
    /// alone would have picked it.
    #[test]
    fn text_embed_is_the_victim_of_last_resort() {
        let p = Placer::new();
        p.set_cards(cards());
        p.set_engine(
            ServiceKind::TextEmbed,
            desc(3000, B, &[B], &[ServiceKind::Flux], false),
        );
        p.set_engine(
            ServiceKind::Stt,
            desc(1500, B, &[B], &[ServiceKind::Flux], false),
        );
        p.set_engine(ServiceKind::Flux, desc(13000, B, &[B], &[], false));
        p.seed_resident(ServiceKind::TextEmbed);
        let _ = p.try_place(ServiceKind::Stt).unwrap();
        p.release(ServiceKind::Stt);
        p.observe(ServiceKind::Stt, B);
        // 16000 − 3000 − 1500 = 11500 free; flux needs 13000. Evicting stt alone
        // frees enough.
        let d = p.try_place(ServiceKind::Flux).unwrap();
        assert_eq!(d.card_uuid.as_deref(), Some(B));
        assert_eq!(d.evict, vec![ServiceKind::Stt]);

        // With no other victim left, it does go.
        p.release(ServiceKind::Flux);
        p.set_engine(ServiceKind::Flux, desc(15000, B, &[B], &[], false));
        let d = p.try_place(ServiceKind::Flux).unwrap();
        assert_eq!(d.evict, vec![ServiceKind::TextEmbed]);
    }

    // ── exclusive-mode hold ──────────────────────────────────────────────────

    /// The Beefy shape: the keeper standing on A with stt/tts in its `evictable_by`,
    /// A reserved to 8700, B given no entry (so reserved whole).
    fn held() -> Placer {
        let p = Placer::new();
        p.set_cards(cards());
        p.set_engine(
            ServiceKind::Llm,
            desc(9000, A, &[A], &[ServiceKind::Stt, ServiceKind::Tts], true),
        );
        p.set_engine(ServiceKind::Stt, desc(1500, A, &[A, B], &[], false));
        p.set_engine(ServiceKind::Tts, desc(3000, B, &[B, A], &[], false));
        p.seed_resident(ServiceKind::Llm);
        p.set_hold(Some((
            ServiceKind::Llm,
            HashMap::from([(A.to_string(), 8700)]),
        )));
        p
    }

    /// The reservation REPLACES the keeper's row rather than stacking on it, and a
    /// card with no entry is reserved whole.
    #[test]
    fn hold_replaces_the_keepers_row_and_reserves_unlisted_cards_whole() {
        let p = held();
        let occ = p.occupancy();
        let a = occ.iter().find(|c| c.uuid == A).unwrap();
        let b = occ.iter().find(|c| c.uuid == B).unwrap();
        assert_eq!(
            (a.reserved_mb, a.used_mb, a.free_mb),
            (Some(8700), 8700, 3300)
        );
        assert_eq!(
            (b.reserved_mb, b.used_mb, b.free_mb),
            (Some(16000), 16000, 0)
        );
        // Its home is reserved whole, so TTS spills to the room beside the hold.
        assert_eq!(
            p.try_place(ServiceKind::Tts).unwrap().card_uuid.as_deref(),
            Some(A)
        );
    }

    /// 🔴 The keeper names stt in `evictable_by`. Under a hold that must NOT make it
    /// a victim — not even under a forced pin — because its unload URL is the held
    /// model's.
    #[test]
    fn hold_never_evicts_the_keeper() {
        let p = held();
        let _ = p.try_place(ServiceKind::Tts).unwrap(); // A: 8700 + 3000 → 300 free
        let d = p.try_place(ServiceKind::Stt).unwrap();
        assert_eq!(
            d.card_uuid, None,
            "no room, and the keeper is not a way to make some"
        );
        assert!(d.evict.is_empty() && d.relocate.is_empty());
        let block = p.why_blocked(
            ServiceKind::Stt,
            PlaceRequest {
                pin: Some(A),
                force: true,
            },
        );
        assert!(
            matches!(block, Some(PlaceBlock::NoCardFits { .. })),
            "{block:?}"
        );
    }

    /// A keeper whose row was dropped mid-hold is admitted, not parked behind its
    /// own reservation — and re-admitting it does not add its peak back.
    #[test]
    fn hold_admits_the_keeper_past_its_own_reservation() {
        let p = held();
        p.force_remove(ServiceKind::Llm);
        assert_eq!(
            p.try_place(ServiceKind::Llm).unwrap().card_uuid.as_deref(),
            Some(A)
        );
        let a = p.occupancy().into_iter().find(|c| c.uuid == A).unwrap();
        assert_eq!(a.used_mb, 8700);
    }

    /// The measured clamp still binds under a hold — the tighter number wins.
    #[test]
    fn hold_composes_with_the_measured_clamp() {
        let p = held();
        p.set_measured_free(A, 2500); // under the 3300 budget
        assert_eq!(p.try_place(ServiceKind::Tts).unwrap().card_uuid, None);
        assert_eq!(
            p.try_place(ServiceKind::Stt).unwrap().card_uuid.as_deref(),
            Some(A)
        );
    }

    #[test]
    fn clearing_the_hold_restores_plain_arithmetic() {
        let p = held();
        p.set_hold(None);
        let a = p.occupancy().into_iter().find(|c| c.uuid == A).unwrap();
        assert_eq!((a.reserved_mb, a.used_mb), (None, 9000));
    }
}
