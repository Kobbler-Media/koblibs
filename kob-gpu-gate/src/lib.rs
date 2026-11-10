//! Pure VRAM-budget GPU scheduler for the koblem stack.
//!
//! There is **no** heavy/light lane and **no** global serialization. Every GPU
//! service is uniform: it is admitted onto a physical card iff its tested
//! worst-case peak fits the card's remaining budget, possibly after evicting /
//! relocating residents that NAME it in their `evictable_by` set (the eviction
//! relation is named + relational, not a numeric priority compare). Two services
//! on different cards run concurrently; two on the same card co-reside iff their
//! peaks fit; the ONLY wait is "every candidate card is full and nothing there
//! names this requester as an evictor" — then the acquire blocks until budget
//! frees.
//!
//! Design:
//!
//! - A [`Placer`] (in `placement.rs`) owns the card list, the per-service
//!   [`EngineDesc`] config, and the live ledger of resident placements. It is
//!   guarded by a synchronous `Mutex` held only for ledger mutations. A separate
//!   async decision lock holds an eviction transaction until its unload RPCs
//!   confirm that the reclaimed VRAM is actually gone.
//! - [`GpuLock::acquire`] is the single acquire API. It asks the placer to place
//!   the service; on success it fires the decision's eviction/relocation HTTP
//!   side-effects (drain + SIGKILL the victims' workers), confirms them, and
//!   returns an
//!   [`Acquisition`] whose [`Acquisition::chosen_gpu`] is the chosen card UUID
//!   (threaded into the C++ server's per-request `gpu` field) and whose Drop
//!   frees the budget + notifies blocked waiters. On "no room" it awaits a
//!   notification and retries.
//! - The [`ServiceUrls`] registry + the per-service unload / drain URLs are kept
//!   (we still SIGKILL/drain a worker to free its VRAM on eviction) along with the
//!   administrative `service_enabled` kill-switch.
//! - Config (cards + `EngineDesc`s) is loaded from koblem at startup and updated
//!   at runtime via the setters — it is never hardcoded.
//!
//! The legacy `acquire_for_*` methods are thin compatibility wrappers over
//! [`GpuLock::acquire`] so existing koblem call sites keep compiling; they no
//! longer carry any lane semantics. The legacy TTS-preempt cancel token, the
//! heavy-busy latch, and the hold window are GONE — the budget is the only
//! coordination. The old cooperative-cancel predicates (`is_tts_requested`,
//! `is_heavy_busy`, …) remain as stable `false`-returning stubs so render loops
//! that poll them simply never self-cancel (co-residency is now budget-managed).

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio::sync::Notify;
use tracing::{info, warn};

mod placement;
pub use placement::{
    Card, CardOccupancy, EngineDesc, PlaceBlock, PlaceDecision, PlaceRequest, Placer,
};

/// Why a non-blocking acquire did not admit.
///
/// [`GpuLock::acquire`] expresses all of this as "park on the notify"; the queue
/// dispatcher cannot park — it has to move on to the next job and say why this one
/// did not start. Every variant is transient and every variant names what clears
/// it, because a job the gate refuses is skipped and reconsidered, never failed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AdmitFailure {
    /// Administratively disabled ([`GpuLock::set_service_enabled`]). Clears when
    /// an operator re-enables it.
    ServiceDisabled,
    /// The placer has no room. Clears when budget frees or config is edited.
    Place(PlaceBlock),
    /// A victim's drain+unload could not be confirmed, so the ledger was rolled
    /// back rather than admit on top of VRAM that may still be resident. Clears on
    /// retry — this is the eviction-cooldown case and must never be reordered
    /// around.
    EvictionUnconfirmed,
    /// The box is in exclusive ("Beefy") mode for `keep`: one service owns EVERY
    /// card and nothing outside its allow-list may be admitted. Clears when an
    /// operator leaves the mode ([`GpuLock::set_exclusive`]). An ALLOWED kind that
    /// does not fit the reservation gets [`AdmitFailure::Place`] instead — that one
    /// is a resource state and clears on its own.
    ///
    /// 🔴 Like `ServiceDisabled` this is an OPERATOR state, not a resource state:
    /// it must never earn a head reservation, or one switch left on freezes the
    /// whole queue behind a job that cannot start until a human intervenes.
    BoxLocked { keep: ServiceKind },
}

impl std::fmt::Display for AdmitFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AdmitFailure::ServiceDisabled => write!(f, "service is disabled"),
            AdmitFailure::Place(b) => write!(f, "{b}"),
            AdmitFailure::EvictionUnconfirmed => write!(f, "waiting for an eviction to confirm"),
            AdmitFailure::BoxLocked { keep } => {
                write!(f, "the box is reserved for {}", keep.as_str())
            }
        }
    }
}

/// One TTS engine the gate can unload during eviction. `tag` identifies the
/// engine so a tagged acquire skips unloading the engine that's acquiring.
#[derive(Clone, Debug)]
pub struct TtsService {
    pub tag: String,
    pub url: String,
    /// HTTP path appended to `url` for the unload call.
    pub unload_path: String,
}

/// A music engine the gate can SIGKILL-unload during eviction (acestep / sa3).
#[derive(Clone, Debug)]
pub struct MusicService {
    pub tag: String,
    pub url: String,
    pub unload_path: String,
}

/// A drain+unload-capable GPU consumer (the LLM and the former "heavy" renderers).
/// `unload_path` SIGKILLs the worker to floor; `drain_path` is the pre-unload
/// "stop accepting new work, finish in-flight" call.
#[derive(Clone, Debug)]
pub struct HeavyService {
    pub kind: HeavyKind,
    pub url: String,
    pub unload_path: String,
    pub drain_path: String,
}

/// Kept only to key the [`HeavyService`] registry (the drain/unload endpoints per
/// service). There is no "heavy" behaviour any more — these are plain services.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum HeavyKind {
    Llm,
    Avatar,
    Flux,
    Songgen,
    Ltx,
    WanVace,
    Ltx2b,
    Krea2,
    MinimaxH3,
}

/// Every gate-tracked GPU service. This is the placement `service_id` and the
/// index for the per-service disable flag.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum ServiceKind {
    Llm,
    Tts,
    Avatar,
    Music,
    Vision,
    Stt,
    // NOTE: append new kinds at the END — `service_enabled` indexes the array by
    // `kind as usize`, so appending (not inserting) preserves existing indices.
    Flux,
    Songgen,
    Ltx,
    Matting,
    WanVace,
    Ltx2b,
    // `Sa3` is the split-out peer of `Music` (acestep): its own budget line,
    // not a shared Music slot.
    Sa3,
    /// Krea-2-Turbo text→image (`krea2` container, the same sd.cpp fork as
    /// `Flux`). Its OWN budget line, not a second flux slot: a ~7.75 GiB nvfp4
    /// DiT that cannot co-reside with FLUX.2 on a 16 GB card.
    Krea2,
    /// MiniMax-H3 joint audio+video (`minimax-h3` container, the same sd.cpp
    /// fork as `Ltx`). Its OWN budget line rather than a second LTX slot: it is
    /// a separate deployment with its own weights (~20 GB DiT + a ~14.5 GB
    /// Qwen3-VL text encoder that swap within a job), so registering it under
    /// `HeavyKind::Ltx` would overwrite ltx-video's own drain/unload URL and the
    /// gate would evict the wrong container.
    MinimaxH3,
    /// SPAR3D + RigAnything image-to-rig worker. Separate budget line: it
    /// peaks on demand and must not be accounted as a vision/matting request.
    Spar3d,
    /// longcat-rig `avatar_server` — photo → rigged LOD ladder → prompted
    /// animation. Its OWN budget line rather than a second `Spar3d` slot: the
    /// two solve the same problem and share nothing, and this one peaks around
    /// 8.7 GiB against spar3d's 2.6 — accounting a rig against spar3d's
    /// reservation would let the scheduler admit a co-tenant onto a 12 GB card
    /// that has no room for one. Its deployment usually pins it to one card by UUID.
    Pixal3d,
    /// The always-on EmbeddingGemma-2 text embedder (`embed-text`). Its requests
    /// never acquire: like the LLM it holds a STANDING reservation on its home
    /// card, and it is evicted only as a last resort (see the victim ordering in
    /// `placement.rs`).
    TextEmbed,
}

impl ServiceKind {
    pub const ALL: [ServiceKind; 18] = [
        ServiceKind::Llm,
        ServiceKind::Tts,
        ServiceKind::Avatar,
        ServiceKind::Music,
        ServiceKind::Vision,
        ServiceKind::Stt,
        ServiceKind::Flux,
        ServiceKind::Songgen,
        ServiceKind::Ltx,
        ServiceKind::Matting,
        ServiceKind::WanVace,
        ServiceKind::Ltx2b,
        ServiceKind::Sa3,
        ServiceKind::Krea2,
        ServiceKind::MinimaxH3,
        ServiceKind::Spar3d,
        ServiceKind::Pixal3d,
        ServiceKind::TextEmbed,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            ServiceKind::Llm => "llm",
            ServiceKind::Tts => "tts",
            ServiceKind::Avatar => "avatar",
            ServiceKind::Music => "music",
            ServiceKind::Vision => "vision",
            ServiceKind::Stt => "stt",
            ServiceKind::Flux => "flux",
            ServiceKind::Songgen => "songgen",
            ServiceKind::Ltx => "ltx",
            ServiceKind::Matting => "matting",
            ServiceKind::WanVace => "wan_vace",
            ServiceKind::Ltx2b => "ltx_2b",
            ServiceKind::Sa3 => "sa3",
            ServiceKind::Krea2 => "krea2",
            ServiceKind::MinimaxH3 => "minimax_h3",
            ServiceKind::Spar3d => "spar3d",
            ServiceKind::Pixal3d => "pixal3d",
            ServiceKind::TextEmbed => "text_embed",
        }
    }

    /// Parse from the wire string used by the per-service API. Mirrors
    /// [`Self::as_str`].
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "llm" => Some(ServiceKind::Llm),
            "tts" => Some(ServiceKind::Tts),
            "avatar" => Some(ServiceKind::Avatar),
            "music" => Some(ServiceKind::Music),
            "vision" => Some(ServiceKind::Vision),
            "stt" => Some(ServiceKind::Stt),
            "flux" => Some(ServiceKind::Flux),
            "songgen" => Some(ServiceKind::Songgen),
            "ltx" => Some(ServiceKind::Ltx),
            "matting" => Some(ServiceKind::Matting),
            "wan_vace" => Some(ServiceKind::WanVace),
            "ltx_2b" => Some(ServiceKind::Ltx2b),
            "sa3" => Some(ServiceKind::Sa3),
            "krea2" => Some(ServiceKind::Krea2),
            "minimax_h3" => Some(ServiceKind::MinimaxH3),
            "spar3d" => Some(ServiceKind::Spar3d),
            "pixal3d" => Some(ServiceKind::Pixal3d),
            "text_embed" => Some(ServiceKind::TextEmbed),
            _ => None,
        }
    }

    /// Services that keep a standing reservation with no guard held: seeded at
    /// boot, re-seeded on their home card once an eviction's contention clears,
    /// and never reclaimed as idle by the queue planner.
    pub fn is_standing(self) -> bool {
        matches!(self, ServiceKind::Llm | ServiceKind::TextEmbed)
    }

    /// For a light TTS/music engine, the `ServiceUrls` registry tag(s) whose
    /// worker this kind owns. The music family still shares one URL `Vec`, so
    /// the split kind (`Sa3`) filters that Vec by tag to unload/reconcile only
    /// its OWN worker. Empty for other kinds.
    ///
    /// A tag that does not match here makes `reconcile_targets` and the unload
    /// arm silently find nothing and return false — the GPU never frees — so
    /// every tag a caller registers must appear in this list.
    fn light_tags(self) -> &'static [&'static str] {
        match self {
            // "breeze" is koblem's engine tag; "chatterbox" is the legacy
            // single-TTS tag registered by `set_service_urls`, which kobbler
            // still uses when it drives a TTS service directly.
            ServiceKind::Tts => &["breeze", "chatterbox"],
            ServiceKind::Music => &["acestep"],
            ServiceKind::Sa3 => &["sa3"],
            _ => &[],
        }
    }

    /// The drain/unload-registry key for this service, if it has one.
    fn heavy_kind(self) -> Option<HeavyKind> {
        match self {
            ServiceKind::Llm => Some(HeavyKind::Llm),
            ServiceKind::Avatar => Some(HeavyKind::Avatar),
            ServiceKind::Flux => Some(HeavyKind::Flux),
            ServiceKind::Songgen => Some(HeavyKind::Songgen),
            ServiceKind::Ltx => Some(HeavyKind::Ltx),
            ServiceKind::WanVace => Some(HeavyKind::WanVace),
            ServiceKind::Ltx2b => Some(HeavyKind::Ltx2b),
            ServiceKind::Krea2 => Some(HeavyKind::Krea2),
            ServiceKind::MinimaxH3 => Some(HeavyKind::MinimaxH3),
            _ => None,
        }
    }
}

/// Service URLs + drain/unload endpoints. Used by the scheduler to free a card's
/// VRAM (SIGKILL the worker) when a service is evicted, relocated, or disabled.
#[derive(Default)]
struct ServiceUrls {
    stt: Option<String>,
    tts: Vec<TtsService>,
    vision: Option<String>,
    text_embed: Option<String>,
    matting: Option<String>,
    music: Vec<MusicService>,
    /// Drain/unload registry for the LLM + the former heavies.
    heavy: Vec<HeavyService>,
}

/// Length of the per-service `service_enabled` array. Kept in lockstep with the
/// `ServiceKind` enum + `ServiceKind::ALL` by the const assert below, so adding a
/// variant without extending both fails to compile instead of panicking at runtime.
const SERVICE_COUNT: usize = 18;
const _: () = assert!(
    ServiceKind::ALL.len() == SERVICE_COUNT,
    "ServiceKind::ALL and the service_enabled array must stay in lockstep"
);

/// Grace period AFTER an eviction's drain+unload completes during which the
/// reconcile loop still refuses to re-`observe` the service — bridges the last gap
/// between the SIGKILL landing and the worker's announce endpoint catching up.
const EVICT_COOLDOWN: Duration = Duration::from_secs(15);

/// How long an OPERATOR DISABLE waits for its eviction before answering.
///
/// This is a backstop, NOT the drain budget: the wait for in-flight work to
/// finish still belongs to the engine (`SD_DRAIN_WAIT_SECONDS`, default 1800s),
/// and blowing this bound does not cancel it — see below. Sized so a normal
/// image render completes inline and only a genuinely long video drain detaches.
///
/// `evict_for_placement` drains on the 900s `drain_http` client so an in-flight
/// multi-minute LLM review is never cut short. That budget is right for an
/// automatic eviction — the acquirer has nothing better to do than wait — but
/// `set_service_enabled` runs inside an HTTP handler, so the same wait leaves
/// `POST /api/v1/gpu/services/{name}` hanging for up to 15 minutes with no
/// response. Observed on krea2: the toggle never answered, and the operator
/// re-fired it assuming the first had been lost.
///
/// Bounding only THIS path keeps eviction semantics identical everywhere else.
/// Timing out is not a correctness problem: the flag has already flipped (the
/// service rejects new work immediately), and the timeout falls into the same
/// branch as a failed unload — the VRAM reservation is RETAINED, so no peer can
/// spend budget that may still be resident. The eviction is not cancelled, only
/// detached: it finishes in the background and drops the reservation then.
///
/// Also caps `disable_all`, which serialises this over all 15 services.
const DISABLE_EVICT_TIMEOUT: Duration = Duration::from_secs(90);

/// How long the eviction's DRAIN leg may wait before the force unload fires anyway.
///
/// The drain is a courtesy, not a requirement: it lets in-flight work finish so the
/// force unload that follows does not have to cancel it. Its result is discarded
/// either way (`evict_for_placement`), because `force` is authoritative.
///
/// That courtesy is worth minutes for the LLM, whose in-flight unit is ONE review.
/// It is worth nothing for a video engine, whose in-flight unit is the render you are
/// trying to stop: MiniMax-H3's own `drain_wait_seconds` is 1800 s and a full Director
/// cut is ~16 generations of ~30 min — **~8 hours in one job**, so its drain cannot
/// succeed while it is busy, at any budget. Every second spent waiting on it is a
/// second the card does not come back, and with [`DISABLE_EVICT_TIMEOUT`] at 90 s the
/// operator's Disable answered long before the 900 s drain client even gave up.
///
/// So: bounded per service, and short for everything that is not the LLM. An IDLE
/// engine still drains instantly (its endpoint returns at once), so this costs nothing
/// in the normal case and only cuts the wait that could never have paid off.
fn drain_budget(kind: ServiceKind) -> Duration {
    match kind {
        ServiceKind::Llm => Duration::from_secs(900),
        _ => Duration::from_secs(30),
    }
}

/// How long a FORCE unload may take.
///
/// Not the 10 s of the general unload client. A forced unload is not a SIGKILL on
/// every engine: MiniMax-H3's cancels the running render, waits for the worker to
/// notice (up to one diffusion step — **~45 s at production shape**) and only then
/// releases the model. At 10 s koblem hung up before the shim could answer, took the
/// transport error as a failed unload, and RETAINED the VRAM reservation for a card
/// the shim had in fact just cleared — permanently, since nothing retries.
///
/// Sized for that step plus the release. It is longer than [`DISABLE_EVICT_TIMEOUT`]
/// on purpose: an operator Disable that outruns it detaches and answers, and the
/// unload still lands and still drops the reservation — which is the whole reason
/// that path spawns rather than cancels.
const FORCE_UNLOAD_TIMEOUT: Duration = Duration::from_secs(300);

/// Where a service is in the eviction lifecycle, so the reconcile loop never
/// re-`observe`s it back onto the card its evictor just took. `InProgress` covers
/// the whole drain+unload (which for a long LLM review can be minutes — a fixed
/// timeout wouldn't); `DoneAt` starts the post-completion grace window.
#[derive(Clone, Copy, Debug)]
enum EvictPhase {
    InProgress,
    DoneAt(Instant),
}

/// The GPU scheduler. Cheap to clone (everything behind `Arc`).
#[derive(Clone)]
pub struct GpuLock {
    /// Global on/off. When `false` the gate is a no-op: every acquire returns a
    /// guard with no card target (servers use their env default) and no eviction
    /// ever fires — the "disable preemption" benchmarking switch.
    enabled: Arc<AtomicBool>,
    service_urls: Arc<Mutex<ServiceUrls>>,
    /// Short-timeout client for the SIGKILL-style unload calls (eviction must not
    /// wait on a flaky unload RPC).
    http: reqwest::Client,
    /// Long-timeout client for the pre-unload *drain* call, which legitimately
    /// blocks until an in-flight LLM request finishes — a multi-minute review must
    /// be allowed to complete before the worker is unloaded ("competing workload
    /// waits, does not preempt"), not cut off by the short unload timeout.
    drain_http: reqwest::Client,
    /// Client for the FORCE unload, which cancels an in-flight render and waits for
    /// the engine to notice. See [`FORCE_UNLOAD_TIMEOUT`] — the short `http` client
    /// hung up before MiniMax-H3 could answer and the gate then retained a VRAM
    /// reservation for a card that had actually been cleared.
    force_http: reqwest::Client,
    /// Per-service operator-facing kill-switch, indexed by `ServiceKind as usize`.
    service_enabled: Arc<[AtomicBool; SERVICE_COUNT]>,
    /// VRAM-budget placement engine (the scheduler brain).
    placer: Placer,
    /// One async mutex serialising the *decision* critical section of an acquire
    /// (the synchronous placer decide) so two acquirers can't race the ledger.
    /// The eviction HTTP side-effects run OUTSIDE this lock (the ledger already
    /// reflects the eviction once `try_place` returns), so a slow drain on one
    /// card never blocks an unrelated acquire on another.
    decision_lock: Arc<tokio::sync::Mutex<()>>,
    /// Woken whenever budget frees (a guard drops, a service is unobserved, a card
    /// is re-enabled). Blocked acquirers re-attempt placement on each notify.
    vacancy: Arc<Notify>,
    /// Per-kind eviction lifecycle. The reconcile loop consults this (via
    /// [`GpuLock::recently_evicted`]) so it can't resurrect a service the gate is
    /// evicting — or just evicted — before its worker has actually unloaded.
    evict_cooldown: Arc<Mutex<HashMap<ServiceKind, EvictPhase>>>,
    /// What the queue is about to ask for, pushed each dispatcher tick.
    ///
    /// Without it [`Self::rehome_standing`] re-seeds the LLM onto its home card in
    /// the gap between every pair of queued jobs, so job N+1 pays an eviction
    /// (drain, up to a 900 s budget) that job N had already paid. Empty when
    /// nothing is queued, which is the pre-queue behaviour exactly.
    pending_demand: Arc<Mutex<Vec<ServiceKind>>>,
    /// Exclusive ("Beefy") mode: `Some(keep)` means one service owns the whole
    /// box and every other kind is refused admission until an operator clears it.
    ///
    /// 🔴 A MODE THAT SUSPENDS THE SCHEDULER, not a placement. A two-card
    /// reservation is not representable — [`Placement`] carries exactly one
    /// `card_uuid` and `free_mb` is per-card — so a model split across cards
    /// cannot be a ledger row, and the ledger stays a harmless lie while this is
    /// set.
    ///
    /// 🔴 And the measured-free clamp is NOT sufficient safety on its own: it is
    /// blind between the keeper's load and its first deep prefill, when the model
    /// has claimed only part of what it will hold. So an allow-listed co-tenant is
    /// admitted only against the placer's STATIC per-card reservation
    /// ([`Self::set_exclusive_with`]), and everything else is refused outright.
    exclusive: Arc<Mutex<Option<Exclusive>>>,
    /// Services whose announce endpoint (`GET /v1/gpu/status`) reports `busy: true`:
    /// a direct-API caller is using a warm worker koblem never placed (a live STT
    /// stream, a file transcription called straight at the server). They hold no
    /// guard, so without this they look idle and the queue planner reclaims them
    /// mid-use. [`Self::unload_if_idle`] treats busy like held.
    busy: Arc<Mutex<HashSet<ServiceKind>>>,
}

/// Exclusive mode's admission rule: `keep` owns the box; `allow` may still be
/// placed, but only inside the budget the placer's hold leaves on each card.
#[derive(Clone, Debug)]
struct Exclusive {
    keep: ServiceKind,
    allow: Vec<ServiceKind>,
}

/// The acquire result: the chosen physical card (a GPU UUID, threaded into the
/// C++ `gpu` request field) plus an RAII handle that frees the reserved VRAM
/// budget on drop and wakes any blocked acquirer.
pub struct Acquisition {
    chosen: Option<String>,
    /// `Some((placer, kind, vacancy))` only when this guard actually recorded a
    /// fresh placement — so Drop releases it exactly once. `None` for a reused
    /// (already-resident) acquire or a disabled/unconfigured no-op.
    release: Option<(Placer, ServiceKind, Arc<Notify>)>,
}

impl Acquisition {
    /// The chosen card UUID, if the placement layer is configured for this kind.
    pub fn chosen_gpu(&self) -> Option<&str> {
        self.chosen.as_deref()
    }

    fn noop() -> Self {
        Acquisition {
            chosen: None,
            release: None,
        }
    }
}

impl Drop for Acquisition {
    fn drop(&mut self) {
        if let Some((placer, kind, vacancy)) = self.release.take()
            && placer.release(kind)
        {
            // Budget freed — wake one blocked acquirer to re-attempt. We use
            // notify_waiters so every parked acquirer re-evaluates (a freed
            // slot may unblock several small services or one large one).
            vacancy.notify_waiters();
        }
    }
}

// ── Back-compat guard aliases ────────────────────────────────────────────────
// All the old per-service guard types are now the single `Acquisition`. Existing
// koblem call sites that name these types or call `chosen_gpu()` keep compiling.
pub type GpuTtsGuard = Acquisition;
pub type GpuSttGuard = Acquisition;
pub type GpuVisionGuard = Acquisition;
pub type GpuMusicGuard = Acquisition;
pub type GpuHeavyGuard = Acquisition;
pub type GpuAvatarGuard = Acquisition;
pub type GpuFluxGuard = Acquisition;
pub type GpuSonggenGuard = Acquisition;
pub type GpuLtxGuard = Acquisition;
pub type GpuWanVaceGuard = Acquisition;
pub type GpuLtx2bGuard = Acquisition;
pub type GpuKrea2Guard = Acquisition;
pub type GpuMinimaxH3Guard = Acquisition;
pub type GpuLlmGuard = Acquisition;

impl Default for GpuLock {
    fn default() -> Self {
        Self::new()
    }
}

impl GpuLock {
    pub fn new() -> Self {
        // Short timeout for the internal unload calls — eviction shouldn't wait on
        // a flaky unload RPC. 10s is plenty for a SIGKILL-style unload.
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .build()
            .expect("failed to build HTTP client for GPU lock");
        // Drain waits for the in-flight LLM request to finish — a long review can
        // take many minutes. Cap generously (15 min) so a genuinely wedged server
        // can't hang an eviction forever, but never cut a legitimate review short.
        let drain_http = reqwest::Client::builder()
            .timeout(Duration::from_secs(900))
            .build()
            .expect("failed to build drain HTTP client for GPU lock");
        // A forced unload cancels an in-flight render and waits for the engine to
        // notice; on MiniMax-H3 that is up to one diffusion step. See
        // [`FORCE_UNLOAD_TIMEOUT`].
        let force_http = reqwest::Client::builder()
            .timeout(FORCE_UNLOAD_TIMEOUT)
            .build()
            .expect("failed to build force-unload HTTP client for GPU lock");
        Self {
            enabled: Arc::new(AtomicBool::new(true)),
            service_urls: Arc::new(Mutex::new(ServiceUrls::default())),
            http,
            drain_http,
            force_http,
            service_enabled: Arc::new([
                AtomicBool::new(true), // Llm
                AtomicBool::new(true), // Tts
                AtomicBool::new(true), // Avatar
                AtomicBool::new(true), // Music
                AtomicBool::new(true), // Vision
                AtomicBool::new(true), // Stt
                AtomicBool::new(true), // Flux
                AtomicBool::new(true), // Songgen
                AtomicBool::new(true), // Ltx
                AtomicBool::new(true), // Matting
                AtomicBool::new(true), // WanVace
                AtomicBool::new(true), // Ltx2b
                AtomicBool::new(true), // Sa3
                AtomicBool::new(true), // Krea2
                AtomicBool::new(true), // MinimaxH3
                AtomicBool::new(true), // Spar3d
                AtomicBool::new(true), // Pixal3d
                AtomicBool::new(true), // TextEmbed
            ]),
            placer: Placer::new(),
            decision_lock: Arc::new(tokio::sync::Mutex::new(())),
            vacancy: Arc::new(Notify::new()),
            evict_cooldown: Arc::new(Mutex::new(HashMap::new())),
            pending_demand: Arc::new(Mutex::new(Vec::new())),
            exclusive: Arc::new(Mutex::new(None)),
            busy: Arc::new(Mutex::new(HashSet::new())),
        }
    }

    /// Global enable/disable of the scheduler. When disabled the gate is a no-op
    /// (every acquire returns immediately with no card target, no eviction).
    pub fn set_enabled(&self, enabled: bool) {
        self.enabled.store(enabled, Ordering::Relaxed);
    }

    // ── Service-URL registry (kept from the legacy gate) ─────────────────────

    /// Update STT / single-TTS / Vision URLs. Single-TTS shape kept for callers
    /// that front one TTS engine; registers it under tag `"chatterbox"` with
    /// unload path `/unload`.
    pub fn set_service_urls(
        &self,
        stt: Option<String>,
        tts: Option<String>,
        vision: Option<String>,
    ) {
        let mut urls = self.service_urls.lock().unwrap();
        urls.stt = stt;
        urls.vision = vision;
        urls.tts = match tts {
            Some(url) if !url.is_empty() => vec![TtsService {
                tag: "chatterbox".to_string(),
                url,
                unload_path: "/unload".to_string(),
            }],
            _ => Vec::new(),
        };
    }

    /// Register the TTS engines the gate should unload during eviction.
    pub fn set_tts_services(&self, services: Vec<TtsService>) {
        self.service_urls.lock().unwrap().tts = services;
    }

    /// Register the always-on text embedder so the gate can unload it
    /// (`POST {url}/unload`) when a heavy job has nowhere else to go, and so the
    /// reconcile loop polls its `/v1/gpu/status`.
    pub fn set_text_embed_url(&self, url: Option<String>) {
        self.service_urls.lock().unwrap().text_embed = url.filter(|u| !u.is_empty());
    }

    /// Register the matting service URL so the gate can SIGKILL its worker when it
    /// is evicted/relocated/disabled.
    pub fn set_matting_url(&self, url: Option<String>) {
        self.service_urls.lock().unwrap().matting = url.filter(|u| !u.is_empty());
    }

    /// Upsert a music engine (`tag`: "acestep" / "sa3"). `unload_path` is the
    /// engine's worker-kill endpoint. `None`/empty `url` removes it.
    pub fn set_music_service(&self, tag: &str, url: Option<String>, unload_path: &str) {
        let mut urls = self.service_urls.lock().unwrap();
        urls.music.retain(|m| m.tag != tag);
        if let Some(u) = url.filter(|u| !u.is_empty()) {
            urls.music.push(MusicService {
                tag: tag.to_string(),
                url: u,
                unload_path: unload_path.to_string(),
            });
        }
    }

    /// Register a drain/unload-capable service (LLM + former heavies). Replaces
    /// any prior registration for the same kind.
    pub fn register_heavy(&self, svc: HeavyService) {
        let mut urls = self.service_urls.lock().unwrap();
        urls.heavy.retain(|s| s.kind != svc.kind);
        urls.heavy.push(svc);
    }

    pub fn set_avatar_service(&self, svc: HeavyService) {
        debug_assert!(matches!(svc.kind, HeavyKind::Avatar));
        self.register_heavy(svc);
    }
    pub fn set_llm_service(&self, svc: HeavyService) {
        debug_assert!(matches!(svc.kind, HeavyKind::Llm));
        self.register_heavy(svc);
    }
    pub fn set_flux_service(&self, svc: HeavyService) {
        debug_assert!(matches!(svc.kind, HeavyKind::Flux));
        self.register_heavy(svc);
    }
    pub fn set_songgen_service(&self, svc: HeavyService) {
        debug_assert!(matches!(svc.kind, HeavyKind::Songgen));
        self.register_heavy(svc);
    }
    pub fn set_ltx_service(&self, svc: HeavyService) {
        debug_assert!(matches!(svc.kind, HeavyKind::Ltx));
        self.register_heavy(svc);
    }
    pub fn set_wan_vace_service(&self, svc: HeavyService) {
        debug_assert!(matches!(svc.kind, HeavyKind::WanVace));
        self.register_heavy(svc);
    }
    pub fn set_ltx2b_service(&self, svc: HeavyService) {
        debug_assert!(matches!(svc.kind, HeavyKind::Ltx2b));
        self.register_heavy(svc);
    }
    pub fn set_krea2_service(&self, svc: HeavyService) {
        debug_assert!(matches!(svc.kind, HeavyKind::Krea2));
        self.register_heavy(svc);
    }
    pub fn set_minimax_h3_service(&self, svc: HeavyService) {
        debug_assert!(matches!(svc.kind, HeavyKind::MinimaxH3));
        self.register_heavy(svc);
    }

    fn heavy_service(&self, kind: HeavyKind) -> Option<HeavyService> {
        self.service_urls
            .lock()
            .unwrap()
            .heavy
            .iter()
            .find(|s| s.kind == kind)
            .cloned()
    }

    /// Enumerate `(ServiceKind, base_url)` for every registered service so the
    /// koblem reconcile loop can poll each one's announce endpoint. Only services
    /// that are configured for placement are returned (the only ones whose
    /// residency affects the budget). TTS/music collapse to their first registered
    /// engine URL (the gate tracks one budget line per ServiceKind).
    pub fn reconcile_targets(&self) -> Vec<(ServiceKind, String)> {
        // Collect (kind, url) candidates while holding the registry lock, then
        // filter by has_engine() afterward (avoids borrowing self across the
        // lock-held closure).
        let mut candidates: Vec<(ServiceKind, String)> = Vec::new();
        {
            let urls = self.service_urls.lock().unwrap();
            let mut add = |kind: ServiceKind, url: Option<&str>| {
                if let Some(u) = url.filter(|u| !u.is_empty()) {
                    candidates.push((kind, u.to_string()));
                }
            };
            add(ServiceKind::Stt, urls.stt.as_deref());
            add(ServiceKind::Vision, urls.vision.as_deref());
            add(ServiceKind::TextEmbed, urls.text_embed.as_deref());
            add(ServiceKind::Matting, urls.matting.as_deref());
            // Music keeps one shared URL Vec across two budget lines: resolve
            // each kind's own worker URL by its registry tag.
            add(
                ServiceKind::Tts,
                urls.tts
                    .iter()
                    .find(|t| ServiceKind::Tts.light_tags().contains(&t.tag.as_str()))
                    .map(|t| t.url.as_str()),
            );
            add(
                ServiceKind::Music,
                urls.music
                    .iter()
                    .find(|m| ServiceKind::Music.light_tags().contains(&m.tag.as_str()))
                    .map(|m| m.url.as_str()),
            );
            add(
                ServiceKind::Sa3,
                urls.music
                    .iter()
                    .find(|m| ServiceKind::Sa3.light_tags().contains(&m.tag.as_str()))
                    .map(|m| m.url.as_str()),
            );
            for h in &urls.heavy {
                let kind = match h.kind {
                    HeavyKind::Llm => ServiceKind::Llm,
                    HeavyKind::Avatar => ServiceKind::Avatar,
                    HeavyKind::Flux => ServiceKind::Flux,
                    HeavyKind::Songgen => ServiceKind::Songgen,
                    HeavyKind::Ltx => ServiceKind::Ltx,
                    HeavyKind::WanVace => ServiceKind::WanVace,
                    HeavyKind::Ltx2b => ServiceKind::Ltx2b,
                    HeavyKind::Krea2 => ServiceKind::Krea2,
                    HeavyKind::MinimaxH3 => ServiceKind::MinimaxH3,
                };
                add(kind, Some(h.url.as_str()));
            }
        }
        candidates
            .into_iter()
            .filter(|(kind, _)| self.placer.has_engine(*kind))
            .collect()
    }

    // ── The single acquire API ───────────────────────────────────────────────

    /// Acquire a physical card for `service`. The ONLY acquire API.
    ///
    /// Algorithm (see [`Placer::try_place`]):
    /// 1. resident → reuse its card;
    /// 2. else first candidate (home, then fastest/most-free) with `free ≥ peak`;
    /// 3. else evict the residents that NAME this service in their `evictable_by`
    ///    (relocate the ones that fit elsewhere, SIGKILL the rest), then place;
    /// 4. else the request **blocks** on the vacancy notify and retries when
    ///    budget frees.
    ///
    /// Returns:
    /// - `None` — the service is administratively disabled
    ///   ([`Self::set_service_enabled`]); the caller should 503.
    /// - `Some(Acquisition)` — placed (or a no-op guard when the gate is globally
    ///   disabled / the service is unconfigured for placement). The chosen card is
    ///   on the guard; drop frees the budget.
    pub async fn acquire(&self, service: ServiceKind) -> Option<Acquisition> {
        // Administratively disabled → reject (caller 503s, no reload).
        if !self.is_service_enabled(service) {
            return None;
        }
        // 🔴 Exclusive mode: refuse like a disable rather than parking on the
        // vacancy notify. Beefy lasts minutes to hours; a parked interactive
        // caller is a hung request, not a queued one — and `None` already means
        // "the caller should 503".
        if self.box_locked_against(service).is_some() {
            return None;
        }
        // Gate globally off, or placement unconfigured for this kind → no-op guard
        // (server uses its env-default card; fully backward compatible).
        if !self.enabled.load(Ordering::Relaxed) || !self.placer.has_engine(service) {
            return Some(Acquisition::noop());
        }

        loop {
            // 🔴 Re-checked at the TOP, before the placement attempt — not only
            // after one fails. Beefy is `set_exclusive` followed by
            // `evict_all_except`, which frees VRAM on purpose; a waiter woken by
            // the first call would otherwise get its placement in on the card the
            // second one just cleared, which is the exact admission the flag
            // exists to prevent.
            if !self.is_service_enabled(service) || self.box_locked_against(service).is_some() {
                return None;
            }

            // Register the vacancy waiter BEFORE we attempt placement so a
            // `notify_waiters` that fires between our failed attempt and the await
            // isn't lost. `notified()` alone does NOT join the waitlist — it must
            // be pinned + enabled first (else the pre-registration the old comment
            // promised never happened and the 2 s poll was load-bearing).
            let mut notified = std::pin::pin!(self.vacancy.notified());
            notified.as_mut().enable();

            // An eviction is a transaction: retain the decision lock while the
            // victim proves it has unloaded. `try_place` temporarily changes the
            // ledger, but an unload failure restores the exact pre-decision
            // snapshot. This is deliberately fail-closed: admitting another
            // service while a failed victim may still own VRAM is worse than
            // waiting for the next retry.
            let outcome = {
                let _guard = self.decision_lock.lock().await;
                let checkpoint = self.placer.checkpoint();
                let Some(decision) = self.placer.try_place(service) else {
                    // Unconfigured race (engine removed mid-flight) → no-op guard.
                    return Some(Acquisition::noop());
                };
                if let Some(card) = decision.card_uuid.clone() {
                    if !decision.reused && !self.apply_decision_side_effects(&decision).await {
                        self.placer.restore_checkpoint(checkpoint);
                        None
                    } else {
                        Some(card)
                    }
                } else {
                    None
                }
            };

            if let Some(card) = outcome {
                // Every guard (fresh admit OR reuse) releases on drop: release now
                // decrements the placement's refcount and frees the budget only
                // when the LAST guard drops (and it's a Guard-sourced placement —
                // the Standing LLM seed is never freed this way).
                return Some(Acquisition {
                    chosen: Some(card),
                    release: Some((self.placer.clone(), service, Arc::clone(&self.vacancy))),
                });
            }

            // Blocked: budget full, nothing here names us as an evictor. Wait for a
            // vacancy then retry; the guard at the top of the loop is what turns a
            // disable-while-parked or a Beefy-while-parked into a prompt `None`
            // instead of a hang. A short timeout backstops a missed wake-up.
            tokio::select! {
                _ = notified.as_mut() => {}
                _ = tokio::time::sleep(Duration::from_secs(2)) => {}
            }
        }
    }

    /// Non-blocking acquire: the body of [`Self::acquire`] minus the retry loop.
    ///
    /// [`Self::acquire`] parks on the vacancy notify, which is right for the
    /// interactive services — an inline HTTP request has nothing better to do than
    /// wait. It is wrong for the queue: the dispatcher must be free to skip this
    /// job, say why, and try the next one (that skip *is* the backfill). So this
    /// answers immediately, and answers with a reason.
    pub async fn try_acquire(&self, service: ServiceKind) -> Result<Acquisition, AdmitFailure> {
        self.admit(service, PlaceRequest::default()).await
    }

    /// Operator pin: place `service` on `card`, and with `force` ignore the
    /// residents' `evictable_by` rule there (the "stop the LLM" button).
    ///
    /// `force` overrides the operator's own eviction *policy*; it does not
    /// override the never-SIGKILL-in-flight rule, so a card whose resident is
    /// mid-render still refuses.
    pub async fn try_acquire_on(
        &self,
        service: ServiceKind,
        card: &str,
        force: bool,
    ) -> Result<Acquisition, AdmitFailure> {
        self.admit(
            service,
            PlaceRequest {
                pin: Some(card),
                force,
            },
        )
        .await
    }

    /// Pure query — would `service` place right now, and if not, why?
    ///
    /// Reserves nothing and fires no side-effects, so it is safe to call for every
    /// queued row on every tick. That is what it is for: the queue page's
    /// "waiting: …" line, computed once per tick from one snapshot so the reason
    /// does not flap between rows.
    ///
    /// `None` = it would place (or the service is unconfigured for placement, in
    /// which case an acquire is a no-op guard and never blocks).
    pub fn can_place(&self, service: ServiceKind) -> Option<AdmitFailure> {
        if !self.is_service_enabled(service) {
            return Some(AdmitFailure::ServiceDisabled);
        }
        // 🔴 BEFORE the globally-disabled early return: with the gate disabled
        // that returns `None` = "it would place", and the dispatcher would then
        // start jobs straight into a box held by Beefy.
        if let Some(keep) = self.box_locked_against(service) {
            return Some(AdmitFailure::BoxLocked { keep });
        }
        if !self.enabled.load(Ordering::Relaxed) {
            return None;
        }
        self.placer
            .why_blocked(service, PlaceRequest::default())
            .map(AdmitFailure::Place)
    }

    /// [`Self::can_place`] under an operator pin.
    pub fn can_place_on(
        &self,
        service: ServiceKind,
        card: &str,
        force: bool,
    ) -> Option<AdmitFailure> {
        if !self.is_service_enabled(service) {
            return Some(AdmitFailure::ServiceDisabled);
        }
        // Same ordering rule as `can_place`, and a forced pin does not override it.
        if let Some(keep) = self.box_locked_against(service) {
            return Some(AdmitFailure::BoxLocked { keep });
        }
        if !self.enabled.load(Ordering::Relaxed) {
            return None;
        }
        self.placer
            .why_blocked(
                service,
                PlaceRequest {
                    pin: Some(card),
                    force,
                },
            )
            .map(AdmitFailure::Place)
    }

    /// The shared admit path for both non-blocking acquires.
    async fn admit(
        &self,
        service: ServiceKind,
        req: PlaceRequest<'_>,
    ) -> Result<Acquisition, AdmitFailure> {
        if !self.is_service_enabled(service) {
            return Err(AdmitFailure::ServiceDisabled);
        }
        // 🔴 BEFORE the no-op short-circuit, and it deliberately also refuses a
        // FORCED operator pin (`try_acquire_on(.., force = true)`): force
        // overrides the residents' `evictable_by` policy, it must not override
        // the box lock.
        if let Some(keep) = self.box_locked_against(service) {
            return Err(AdmitFailure::BoxLocked { keep });
        }
        if !self.enabled.load(Ordering::Relaxed) || !self.placer.has_engine(service) {
            return Ok(Acquisition::noop());
        }

        // Same transaction as `acquire`: hold the decision lock while the victim
        // proves it has unloaded, and restore the pre-decision ledger if it can't.
        let outcome = {
            let _guard = self.decision_lock.lock().await;
            let checkpoint = self.placer.checkpoint();
            let Some(planned) = self.placer.try_place_with(service, req) else {
                // Unconfigured race (engine removed mid-flight) → no-op guard.
                return Ok(Acquisition::noop());
            };
            match planned {
                Err(block) => Err(AdmitFailure::Place(block)),
                Ok(decision) => match decision.card_uuid.clone() {
                    None => Err(AdmitFailure::EvictionUnconfirmed),
                    Some(card) => {
                        if !decision.reused && !self.apply_decision_side_effects(&decision).await {
                            self.placer.restore_checkpoint(checkpoint);
                            Err(AdmitFailure::EvictionUnconfirmed)
                        } else {
                            Ok(card)
                        }
                    }
                },
            }
        };

        outcome.map(|card| Acquisition {
            chosen: Some(card),
            release: Some((self.placer.clone(), service, Arc::clone(&self.vacancy))),
        })
    }

    /// Tell the gate what the queue is about to ask for. Called every dispatcher
    /// tick with the VRAM claims of the jobs at the head of the queue; an empty
    /// slice restores the pre-queue behaviour.
    ///
    /// Its only consumer is [`Self::rehome_standing`] — see [`Self::pending_demand`].
    pub fn set_pending_demand(&self, kinds: &[ServiceKind]) {
        *self.pending_demand.lock().unwrap() = kinds.to_vec();
    }

    /// Snapshot of the current pending demand (drives the occupancy UI's
    /// "not re-homing, work is queued" note).
    pub fn pending_demand(&self) -> Vec<ServiceKind> {
        self.pending_demand.lock().unwrap().clone()
    }

    /// Enter (`Some(keep)`) or leave (`None`) exclusive ("Beefy") mode.
    ///
    /// Entering refuses admission to every kind except `keep`; leaving restores
    /// normal scheduling. Call this BEFORE [`Self::evict_all_except`] so nothing
    /// re-admits behind the evictions, and clear it LAST on the way out.
    ///
    /// Reserves nothing and evicts nothing by itself — set this and stop, and you
    /// have a locked box with its previous residents still on it.
    ///
    /// `Some(keep)` is the whole-box lock: no allow-list. For co-tenants, use
    /// [`Self::set_exclusive_with`].
    pub fn set_exclusive(&self, keep: Option<ServiceKind>) {
        match keep {
            Some(k) => self.set_exclusive_with(k, &[], HashMap::new()),
            None => {
                // 🔴 Flag FIRST, hold second — the mirror of entering. With the flag
                // gone and the hold still set, every kind admits against the TIGHTER
                // budget for an instant; the other order would briefly admit
                // allow-listed kinds against no reservation at all.
                *self.exclusive.lock().unwrap() = None;
                self.placer.set_hold(None);
                info!("GPU: exclusive mode OFF");
                // Leaving frees the whole box; every parked acquirer must
                // re-evaluate rather than sit on a stale notify.
                self.vacancy.notify_waiters();
            }
        }
    }

    /// Enter exclusive mode for `keep`, still admitting the kinds in `allow` —
    /// but only inside what `reserved_mb` leaves on each card.
    ///
    /// `reserved_mb` is `keep`'s measured footprint per card UUID. It replaces the
    /// keeper's single-card ledger row for budgeting, and 🔴 **a card with no entry
    /// is reserved whole**, so an empty map is exactly the old whole-box lock even
    /// with a non-empty `allow`. The measured-free clamp still applies on top.
    ///
    /// While set, `keep` is never an eviction victim — for any requester, forced
    /// or not.
    pub fn set_exclusive_with(
        &self,
        keep: ServiceKind,
        allow: &[ServiceKind],
        reserved_mb: HashMap<String, u32>,
    ) {
        // 🔴 Hold FIRST, flag second: the flag is what lets `allow` in, and it
        // must never be visible before the reservation that bounds it.
        self.placer.set_hold(Some((keep, reserved_mb.clone())));
        let allow: Vec<ServiceKind> = allow.iter().copied().filter(|k| *k != keep).collect();
        *self.exclusive.lock().unwrap() = Some(Exclusive {
            keep,
            allow: allow.clone(),
        });
        info!(
            service = ?keep,
            allow = ?allow,
            reserved_mb = ?reserved_mb,
            "GPU: exclusive mode ON — only the allow-list may co-reside"
        );
        // Entering can only block; a parked acquirer must learn that now.
        self.vacancy.notify_waiters();
    }

    /// The service exclusive mode is held for, if any.
    pub fn exclusive(&self) -> Option<ServiceKind> {
        self.exclusive.lock().unwrap().as_ref().map(|e| e.keep)
    }

    /// The kinds exclusive mode still admits besides the keeper (empty when off).
    pub fn exclusive_allow(&self) -> Vec<ServiceKind> {
        self.exclusive
            .lock()
            .unwrap()
            .as_ref()
            .map(|e| e.allow.clone())
            .unwrap_or_default()
    }

    /// `Some(keep)` when exclusive mode would refuse `service` right now.
    ///
    /// 🔴 An allow-listed kind is let through ONLY when the placer will actually
    /// budget it. With the gate globally disabled, or no engine config for the
    /// kind, an acquire is a no-op guard that never consults the reservation —
    /// so it is refused like any other peer rather than admitted unchecked.
    fn box_locked_against(&self, service: ServiceKind) -> Option<ServiceKind> {
        let ex = self.exclusive.lock().unwrap().clone()?;
        if ex.keep == service {
            return None;
        }
        let budgeted = self.enabled.load(Ordering::Relaxed) && self.placer.has_engine(service);
        if budgeted && ex.allow.contains(&service) {
            return None;
        }
        Some(ex.keep)
    }

    /// Free every card for `keep`: drain + force-unload every OTHER service that
    /// holds a reservation, CONCURRENTLY. Returns the kinds it could not prove
    /// gone (empty = the box is clean).
    ///
    /// 🔴 Concurrent, not serial. `disable_all` shows what serial costs: 18 kinds
    /// × `DISABLE_EVICT_TIMEOUT` = up to 27 minutes to answer one button. The
    /// engines do not contend — each is a different container answering its own
    /// HTTP — so the whole sweep is bounded by the slowest single engine.
    ///
    /// 🔴 Call [`Self::set_exclusive`] FIRST. This evicts; it does not hold the
    /// door, and without the flag a queued job admits onto the card the eviction
    /// just freed.
    ///
    /// Fails closed exactly like the eviction path it reuses: a kind whose unload
    /// cannot be confirmed KEEPS its reservation and is reported, never silently
    /// dropped. Note it deliberately does not touch `keep`, so a caller that also
    /// needs the keeper's own worker unloaded must do that itself.
    pub async fn evict_all_except(&self, keep: ServiceKind) -> Vec<ServiceKind> {
        self.evict_all_except_sparing(keep, &[]).await
    }

    /// [`Self::evict_all_except`], leaving each kind in `spare` resident when its
    /// card's budget still balances under the current hold (`used ≤ total`).
    ///
    /// For a standing co-tenant (the text embedder) that the keeper's reservation
    /// was sized to leave room for: unloading it only to have it reload beside the
    /// keeper moments later buys nothing, and reloading it AFTER the keeper has
    /// claimed its memory is the order that can OOM. A spared kind whose card is
    /// over budget is evicted like everything else — call [`Self::set_exclusive_with`]
    /// first, or there is no hold to measure against.
    pub async fn evict_all_except_sparing(
        &self,
        keep: ServiceKind,
        spare: &[ServiceKind],
    ) -> Vec<ServiceKind> {
        let resident: Vec<ServiceKind> = self
            .occupancy()
            .into_iter()
            .flat_map(|c| {
                let fits = c.used_mb <= c.total_mb;
                c.residents
                    .into_iter()
                    .map(|(k, _peak)| k)
                    .filter(move |k| !(fits && spare.contains(k)))
            })
            .filter(|k| *k != keep)
            .collect();
        if resident.is_empty() {
            return Vec::new();
        }
        // Mark in-progress BEFORE any drain, so the 5 s reconcile loop cannot
        // re-`observe` a still-draining worker back onto the card underneath us.
        {
            let mut cooldown = self.evict_cooldown.lock().unwrap();
            for kind in &resident {
                cooldown.insert(*kind, EvictPhase::InProgress);
            }
        }
        let mut set = tokio::task::JoinSet::new();
        for kind in resident {
            let this = self.clone();
            set.spawn(async move { (kind, this.evict_for_placement(kind).await) });
        }
        let mut failed = Vec::new();
        while let Some(joined) = set.join_next().await {
            let Ok((kind, ok)) = joined else { continue };
            if ok {
                self.placer.force_remove(kind);
                self.evict_cooldown
                    .lock()
                    .unwrap()
                    .insert(kind, EvictPhase::DoneAt(Instant::now()));
            } else {
                warn!(
                    service = ?kind,
                    "GPU: exclusive sweep could not confirm an unload; retaining its VRAM reservation"
                );
                self.evict_cooldown.lock().unwrap().remove(&kind);
                failed.push(kind);
            }
        }
        self.vacancy.notify_waiters();
        failed
    }

    /// One service's placement descriptor — the queue's planner needs the peak and
    /// candidate list to decide what to keep warm.
    pub fn engine_config(&self, kind: ServiceKind) -> Option<EngineDesc> {
        self.placer.engine(kind)
    }

    /// Live guard count for a service (0 = warm but idle, or absent).
    pub fn holders(&self, kind: ServiceKind) -> u32 {
        self.placer.holders(kind)
    }

    /// Queue-driven early unload: drain + force-unload an **idle** resident whose
    /// VRAM nothing in the queue wants, so the next job's admission does not pay
    /// the drain.
    ///
    /// Refuses while a guard is held — an idle resident is a warm worker, a held
    /// one is a running render — or while the service reports itself busy
    /// ([`Self::set_reported_busy`]). Reuses the eviction path verbatim, which matters:
    /// on both engine families `drain` closes admission and **only `unload`
    /// reopens it**, and koblem calls `/v1/admin/load` nowhere, so a drain that is
    /// not followed through leaves the engine answering 503 to every render until
    /// the container restarts. That path always completes the unload, and always
    /// sends `force: true` — required, because an unforced unload on the sd.cpp
    /// fleet answers **200** with a body meaning "busy" and the gate reads only the
    /// status code.
    ///
    /// Returns `true` when the unload was confirmed and the reservation dropped.
    pub async fn unload_if_idle(&self, kind: ServiceKind) -> bool {
        if self.placer.holders(kind) > 0
            || self.is_reported_busy(kind)
            || self.recently_evicted(kind)
        {
            return false;
        }
        let checkpoint = self.placer.checkpoint();
        self.evict_cooldown
            .lock()
            .unwrap()
            .insert(kind, EvictPhase::InProgress);
        if !self.evict_for_placement(kind).await {
            self.placer.restore_checkpoint(checkpoint);
            self.evict_cooldown.lock().unwrap().remove(&kind);
            return false;
        }
        self.placer.force_remove(kind);
        self.evict_cooldown
            .lock()
            .unwrap()
            .insert(kind, EvictPhase::DoneAt(Instant::now()));
        self.vacancy.notify_waiters();
        true
    }

    /// Fire the unload / drain side-effects a placement (or card-disable) decision
    /// requires: SIGKILL evicted non-relocatable victims, drain+unload relocatable
    /// victims so they respawn on the relocation target's card on their next
    /// request (the C++ server self-relocates when the `gpu` field changes).
    async fn apply_decision_side_effects(&self, decision: &PlaceDecision) -> bool {
        let victims: Vec<ServiceKind> = decision
            .evict
            .iter()
            .chain(decision.relocate.iter().map(|(k, _)| k))
            .copied()
            .collect();
        if victims.is_empty() {
            return true;
        }
        // Mark victims in-progress BEFORE the drain so the reconcile loop can't
        // re-observe a still-draining worker (a long LLM review keeps reporting
        // `loaded` for minutes) back onto the card its evictor just took.
        {
            let mut cooldown = self.evict_cooldown.lock().unwrap();
            for kind in &victims {
                cooldown.insert(*kind, EvictPhase::InProgress);
            }
        }
        for kind in &decision.evict {
            if !self.evict_for_placement(*kind).await {
                let mut cooldown = self.evict_cooldown.lock().unwrap();
                for victim in &victims {
                    cooldown.remove(victim);
                }
                return false;
            }
        }
        for (kind, _to) in &decision.relocate {
            if !self.evict_for_placement(*kind).await {
                let mut cooldown = self.evict_cooldown.lock().unwrap();
                for victim in &victims {
                    cooldown.remove(victim);
                }
                return false;
            }
        }
        // Drain+unload done → start the post-completion grace window.
        {
            let now = Instant::now();
            let mut cooldown = self.evict_cooldown.lock().unwrap();
            for kind in &victims {
                cooldown.insert(*kind, EvictPhase::DoneAt(now));
            }
        }
        // We just freed VRAM elsewhere — wake any other blocked acquirer.
        self.vacancy.notify_waiters();
        true
    }

    /// True while the gate is evicting `kind`, or within [`EVICT_COOLDOWN`] of the
    /// eviction completing. The reconcile loop uses this to avoid re-`observe`-ing a
    /// service whose worker is still shutting down (its announce lags the SIGKILL).
    pub fn recently_evicted(&self, kind: ServiceKind) -> bool {
        match self.evict_cooldown.lock().unwrap().get(&kind) {
            Some(EvictPhase::InProgress) => true,
            Some(EvictPhase::DoneAt(t)) => t.elapsed() < EVICT_COOLDOWN,
            None => false,
        }
    }

    /// Evict a service: drain (if it has a drain endpoint) then SIGKILL its
    /// worker so its VRAM returns to floor. Returns false unless the unload
    /// endpoint acknowledges the transition; admission then restores its ledger.
    async fn evict_for_placement(&self, kind: ServiceKind) -> bool {
        if let Some(hk) = kind.heavy_kind()
            && let Some(svc) = self.heavy_service(hk)
        {
            // Drain first (no-op for services without a meaningful drain) via the
            // long-timeout client so an in-flight multi-minute review finishes
            // before we unload, then SIGKILL with force to reclaim the VRAM.
            if !svc.drain_path.is_empty() {
                // Drain makes normal handoff graceful, but a force unload is
                // still authoritative if drain times out or the worker is already
                // unhealthy — so this leg is bounded by what waiting could ever
                // buy on THIS service. See [`drain_budget`].
                let _ = tokio::time::timeout(
                    drain_budget(kind),
                    self.post_with(&self.drain_http, &svc.url, &svc.drain_path, kind, false),
                )
                .await;
            }
            // On the force client, not the 10s one: force does not mean instant.
            self.post_with(&self.force_http, &svc.url, &svc.unload_path, kind, true)
                .await
        } else if kind.heavy_kind().is_none() {
            // Light services: SIGKILL their forked worker via their unload URL.
            self.unload_service(kind).await
        } else {
            // No URL means this service has no worker the gate can prove gone.
            false
        }
    }

    // ── Per-service kill-switch (operator/benchmark) ─────────────────────────

    /// Set a service's enabled flag. Disabling fires its unload URL so its VRAM
    /// clears within seconds AND drops its standing budget placement; subsequent
    /// acquires of that kind return `None` until re-enabled.
    ///
    /// The flag flips immediately; the unload is awaited only up to
    /// [`DISABLE_EVICT_TIMEOUT`] and then finishes detached, so callers in an
    /// HTTP handler always get an answer. Returning does NOT guarantee the
    /// reservation is gone — read it back from the occupancy endpoint.
    pub async fn set_service_enabled(&self, kind: ServiceKind, enabled: bool) {
        let idx = kind as usize;
        let prev = self.service_enabled[idx].swap(enabled, Ordering::Relaxed);
        if prev && !enabled {
            // Keep its reservation if its worker cannot be confirmed gone. The
            // operator-disabled service will reject new work, but no peer may
            // spend the potentially still-resident VRAM.
            //
            // Run the eviction as a task and wait on it only briefly: this is an
            // HTTP handler and the drain leg is on the 900s client. Spawning
            // rather than `timeout`-ing the future directly matters — on timeout
            // we DETACH (dropping a JoinHandle leaves the task running) instead
            // of cancelling mid-drain, so the unload still lands and the ledger
            // still gets cleaned up, just after we have answered.
            let this = self.clone();
            let evict = tokio::spawn(async move {
                if this.evict_for_placement(kind).await {
                    this.placer.force_remove(kind);
                    this.vacancy.notify_waiters();
                } else {
                    warn!(service = ?kind, "GPU: disabled service could not be unloaded; retaining VRAM reservation");
                }
            });
            if tokio::time::timeout(DISABLE_EVICT_TIMEOUT, evict)
                .await
                .is_err()
            {
                warn!(service = ?kind, timeout_s = DISABLE_EVICT_TIMEOUT.as_secs(),
                    "GPU: disable eviction still running; answering now and finishing it in the background");
            }
        } else if !prev && enabled {
            // Re-establish the always-resident LLM's standing reservation — a
            // disable dropped it, and re-enable must put its peak back on the
            // budget or a spilling heavy would admit on top of the warm worker.
            if kind.is_standing() {
                self.placer.seed_resident(kind);
            }
            // Re-enabling can unblock acquirers waiting on this service.
            self.vacancy.notify_waiters();
        }
    }

    pub fn is_service_enabled(&self, kind: ServiceKind) -> bool {
        self.service_enabled[kind as usize].load(Ordering::Relaxed)
    }

    /// Snapshot every service's enabled flag.
    pub fn service_states(&self) -> Vec<(ServiceKind, bool)> {
        ServiceKind::ALL
            .iter()
            .map(|k| (*k, self.is_service_enabled(*k)))
            .collect()
    }

    // ── Unload helpers (kept; used by eviction) ──────────────────────────────

    /// SIGKILL a light service's worker via its unload URL.
    async fn unload_service(&self, kind: ServiceKind) -> bool {
        match kind {
            ServiceKind::Stt
            | ServiceKind::Vision
            | ServiceKind::Matting
            | ServiceKind::TextEmbed => {
                let url = {
                    let urls = self.service_urls.lock().unwrap();
                    match kind {
                        ServiceKind::Stt => urls.stt.clone(),
                        ServiceKind::Vision => urls.vision.clone(),
                        ServiceKind::Matting => urls.matting.clone(),
                        ServiceKind::TextEmbed => urls.text_embed.clone(),
                        _ => unreachable!(),
                    }
                };
                let Some(base_url) = url.filter(|u| !u.is_empty()) else {
                    return false;
                };
                self.post(&base_url, "/unload", kind, false).await
            }
            ServiceKind::Tts => {
                let tags = kind.light_tags();
                let entries: Vec<TtsService> = self
                    .service_urls
                    .lock()
                    .unwrap()
                    .tts
                    .iter()
                    .filter(|t| tags.contains(&t.tag.as_str()))
                    .cloned()
                    .collect();
                if entries.is_empty() {
                    return false;
                }
                let mut success = true;
                for entry in entries {
                    success &= self.post(&entry.url, &entry.unload_path, kind, false).await;
                }
                success
            }
            ServiceKind::Music | ServiceKind::Sa3 => {
                let tags = kind.light_tags();
                let entries: Vec<MusicService> = self
                    .service_urls
                    .lock()
                    .unwrap()
                    .music
                    .iter()
                    .filter(|m| tags.contains(&m.tag.as_str()))
                    .cloned()
                    .collect();
                if entries.is_empty() {
                    return false;
                }
                let mut success = true;
                for e in entries {
                    success &= self.post(&e.url, &e.unload_path, kind, false).await;
                }
                success
            }
            ServiceKind::Llm
            | ServiceKind::Avatar
            | ServiceKind::Flux
            | ServiceKind::Songgen
            | ServiceKind::Ltx
            | ServiceKind::WanVace
            | ServiceKind::Ltx2b
            | ServiceKind::Krea2
            | ServiceKind::MinimaxH3 => {
                // Drain/unload services go via `evict_for_placement` → `post`.
                false
            }
            ServiceKind::Spar3d | ServiceKind::Pixal3d => {
                // Acquire-only budget lines: nothing registers a URL for either with
                // the gate, so there is no worker endpoint to SIGKILL.
                //
                // 🔴 pixal3d could in principle expose one — its `/v1/admin/unload`
                // SIGKILLs the CUDA child and really does return the card. It is left
                // out deliberately: the parent idle-unloads on its own after 300 s,
                // and a gate-driven unload racing a queued job it cannot see would
                // kill a render the service had already admitted.
                false
            }
        }
    }

    /// POST `{url}{path}` via the short-timeout unload client. Convenience wrapper
    /// over [`Self::post_with`].
    async fn post(&self, base_url: &str, path: &str, kind: ServiceKind, force: bool) -> bool {
        self.post_with(&self.http, base_url, path, kind, force)
            .await
    }

    /// POST `{url}{path}` on `client` with a `{"force": …}` body — `true` is the
    /// render-cancelling SIGKILL. Logs success/failure.
    ///
    /// The body is sent either way, and not only because it is the honest
    /// spelling of the default. A bodyless reqwest POST carries no
    /// `Content-Length` and no chunked encoding, and cpp-httplib answers one
    /// with **400 Bad Request** — so every non-forced unload of a C++ engine
    /// silently failed. Observed as `status=400` from `tts-breeze` every 8 s for
    /// minutes: the gate believed it had evicted the worker, and the VRAM stayed
    /// where it was until the engine's own idle timer got to it.
    async fn post_with(
        &self,
        client: &reqwest::Client,
        base_url: &str,
        path: &str,
        kind: ServiceKind,
        force: bool,
    ) -> bool {
        if base_url.is_empty() {
            return false;
        }
        let url = format!("{}{}", base_url.trim_end_matches('/'), path);
        let req = client
            .post(&url)
            .json(&serde_json::json!({ "force": force }));
        match req.send().await {
            Ok(resp) if resp.status().is_success() => {
                info!(service = ?kind, url = %url, "GPU: unloaded worker");
                true
            }
            Ok(resp) => {
                warn!(
                    service = ?kind,
                    url = %url,
                    status = resp.status().as_u16(),
                    "GPU: unload returned non-success status"
                );
                false
            }
            Err(e) => {
                warn!(service = ?kind, url = %url, error = %e, "GPU: failed to unload worker");
                false
            }
        }
    }

    // ── Placement config (loaded from koblem's DB) ───────────────────────────

    /// Replace the card list (koblem loads `gpu_cards` from its DB at startup +
    /// on edit).
    pub fn set_cards(&self, cards: Vec<Card>) {
        self.placer.set_cards(cards);
        self.vacancy.notify_waiters();
    }

    /// Replace one service's placement descriptor (live-editable from the UI).
    pub fn set_engine_config(&self, kind: ServiceKind, desc: EngineDesc) {
        self.placer.set_engine(kind, desc);
        self.vacancy.notify_waiters();
    }

    /// Establish a standing (always-resident) placement for a service on its home
    /// card (the LLM) so its peak counts against the budget even with no guard
    /// held. Returns the chosen card UUID.
    pub fn seed_resident(&self, kind: ServiceKind) -> Option<String> {
        self.placer.seed_resident(kind)
    }

    /// Reclaim each standing service's **home** card ([`ServiceKind::is_standing`]:
    /// the LLM and the text embedder) once the contention that evicted it clears.
    /// Called every reconcile tick: if one was dropped and its home card is free
    /// again, re-seed its standing reservation on home so it's ready without
    /// waiting for the next request. Skipped when the gate is globally disabled or
    /// the service is administratively disabled. Home-only by design (the "drop it
    /// until home is clean, then bring it back" behaviour) — it never roams to a
    /// spill card on its own.
    pub fn rehome_standing(&self) {
        if !self.enabled.load(Ordering::Relaxed) {
            return;
        }
        let pending = self.pending_demand.lock().unwrap().clone();
        for kind in ServiceKind::ALL.into_iter().filter(|k| k.is_standing()) {
            // Beefy evicted it and does not admit it: it stays off until exit.
            if !self.is_service_enabled(kind) || self.box_locked_against(kind).is_some() {
                continue;
            }
            // Don't re-home into the gap between two queued jobs. Re-seeding here
            // puts the service back on the home card that the *next* queued job is
            // about to evict it from, so that job pays a drain (900 s budget for
            // the LLM) the job before it had already paid — every time, all night.
            // Only pending work that would actually evict it suppresses this.
            if !pending.is_empty() && self.placer.evictable_by_any(kind, &pending) {
                continue;
            }
            if let Some(card) = self.placer.rehome_standing(kind) {
                info!(service = ?kind, card = %card, "GPU: reclaimed home for standing service");
                self.vacancy.notify_waiters();
            }
        }
    }

    /// Out-of-band reconcile: a worker observed via a server's announce endpoint
    /// occupies budget at its peak even though koblem didn't place it. Ignored for
    /// a kind still within its eviction cooldown (the gate just booted it off a
    /// card and its worker is mid-shutdown) — else we'd resurrect the stale peak.
    pub fn observe(&self, kind: ServiceKind, card_uuid: &str) {
        if self.recently_evicted(kind) {
            return;
        }
        self.placer.observe(kind, card_uuid);
    }

    /// Out-of-band reconcile: a service's announce endpoint reports it is no
    /// longer loaded → drop ONLY an `Observed` placement (never a live guard-held
    /// or standing reservation) and wake waiters.
    pub fn unobserve(&self, kind: ServiceKind) {
        self.set_reported_busy(kind, false);
        if self.placer.unobserve_observed(kind) {
            self.vacancy.notify_waiters();
        }
    }

    /// Out-of-band reconcile: the service's announce endpoint says whether a caller
    /// koblem cannot see is using it right now (`busy`). Servers that don't report
    /// it are never busy.
    pub fn set_reported_busy(&self, kind: ServiceKind, busy: bool) {
        let mut set = self.busy.lock().unwrap();
        if busy {
            set.insert(kind);
        } else {
            set.remove(&kind);
        }
    }

    /// See [`Self::set_reported_busy`].
    pub fn is_reported_busy(&self, kind: ServiceKind) -> bool {
        self.busy.lock().unwrap().contains(&kind)
    }

    /// Push a card's measured real free VRAM (MiB) into the placer so admission
    /// clamps below it (guards against under-set peaks / non-gate VRAM users).
    pub fn set_measured_free(&self, card_uuid: &str, free_mb: u32) {
        self.placer.set_measured_free(card_uuid, free_mb);
        // A fresh (possibly larger) measurement can unblock a parked acquirer.
        self.vacancy.notify_waiters();
    }

    /// Snapshot the configured cards (drives the settings UI).
    pub fn cards(&self) -> Vec<Card> {
        self.placer.cards()
    }

    /// Snapshot every configured service descriptor (drives the settings UI).
    pub fn engine_configs(&self) -> Vec<(ServiceKind, EngineDesc)> {
        self.placer.engines()
    }

    /// Live per-card occupancy (totals, free budget, resident services).
    pub fn occupancy(&self) -> Vec<CardOccupancy> {
        self.placer.occupancy()
    }

    /// Service kinds actively serving a request (a placement with `holders > 0`).
    /// Drives the "busy" UI signal — an idle-but-loaded worker or the warm
    /// standing LLM seed reserves budget but is NOT busy, so it's excluded. A
    /// service reporting itself busy out-of-band (a live STT stream) is included.
    pub fn active_services(&self) -> Vec<ServiceKind> {
        let mut out = self.placer.active_kinds();
        for kind in self.busy.lock().unwrap().iter() {
            if !out.contains(kind) {
                out.push(*kind);
            }
        }
        out
    }

    /// The home/default card UUID for a service. `None` when unconfigured.
    pub fn default_gpu(&self, kind: ServiceKind) -> Option<String> {
        self.placer.default_gpu(kind)
    }

    /// Enable/disable a physical card at runtime. Disabling drains the card
    /// (relocating relocatable residents to another enabled card if they fit,
    /// else SIGKILLing them) and removes it from placement. Refuses to disable
    /// the last enabled card. Returns `Err` on an unknown UUID or last-card
    /// refusal.
    pub async fn set_card_enabled(&self, uuid: &str, enabled: bool) -> Result<(), String> {
        let result = {
            let _guard = self.decision_lock.lock().await;
            let was_disabled = self.placer.is_card_disabled(uuid);
            let checkpoint = self.placer.checkpoint();
            let decision = self.placer.set_card_enabled(uuid, enabled)?;
            if !self.apply_decision_side_effects(&decision).await {
                self.placer.restore_checkpoint(checkpoint);
                if !enabled && !was_disabled {
                    // Restore the card's admission state too: an unload failure
                    // must not make its still-resident worker look evictable.
                    self.placer.set_card_enabled(uuid, true)?;
                }
                Err("could not prove every worker left the disabled GPU".to_string())
            } else {
                Ok(())
            }
        };
        result?;
        // Re-enabling (or draining) can unblock parked acquirers.
        self.vacancy.notify_waiters();
        Ok(())
    }

    /// True only when `uuid` is a known, administratively disabled card.
    pub fn is_card_disabled(&self, uuid: &str) -> bool {
        self.placer.is_card_disabled(uuid)
    }

    /// Reconcile backstop: a worker for `kind` was observed resident on a card
    /// that is administratively disabled — it got there by bypassing admission
    /// (a request path that omitted the `gpu` field → the worker's
    /// `WORKER_DEFAULT_GPU`). Drain + SIGKILL it so it respawns on an enabled
    /// card on its next request, and drop any ledger reservation pinning it to
    /// the disabled card. No-op if the card is enabled/unknown, or if `kind` is
    /// already mid-eviction (avoids re-firing every reconcile tick while the
    /// worker shuts down). Returns `true` if an eviction was fired.
    pub async fn evict_off_disabled_card(&self, kind: ServiceKind, uuid: &str) -> bool {
        if !self.placer.is_card_disabled(uuid) || self.recently_evicted(kind) {
            return false;
        }
        let checkpoint = self.placer.checkpoint();
        self.placer.force_remove(kind);
        // Mirror `apply_decision_side_effects`: hold the evict cooldown across the
        // drain so the reconcile loop's `recently_evicted` guard suppresses a
        // re-fire on the next tick while the SIGKILLed worker's announce still
        // lags reporting `loaded` (drains on heavies use the long-timeout client —
        // re-draining every 5s would pile up).
        self.evict_cooldown
            .lock()
            .unwrap()
            .insert(kind, EvictPhase::InProgress);
        if !self.evict_for_placement(kind).await {
            self.placer.restore_checkpoint(checkpoint);
            self.evict_cooldown.lock().unwrap().remove(&kind);
            return false;
        }
        self.evict_cooldown
            .lock()
            .unwrap()
            .insert(kind, EvictPhase::DoneAt(Instant::now()));
        self.vacancy.notify_waiters();
        true
    }

    // ── Legacy compatibility wrappers ────────────────────────────────────────
    // These keep existing koblem call sites compiling. They carry NO lane
    // semantics — each is just `acquire(<kind>)`.

    pub async fn acquire_for_tts(&self) -> Option<GpuTtsGuard> {
        self.acquire(ServiceKind::Tts).await
    }
    pub async fn acquire_for_sa3(&self) -> Option<GpuMusicGuard> {
        self.acquire(ServiceKind::Sa3).await
    }
    pub async fn acquire_for_stt(&self) -> Option<GpuSttGuard> {
        self.acquire(ServiceKind::Stt).await
    }
    pub async fn acquire_for_vision(&self) -> Option<GpuVisionGuard> {
        self.acquire(ServiceKind::Vision).await
    }
    pub async fn acquire_for_music(&self) -> Option<GpuMusicGuard> {
        self.acquire(ServiceKind::Music).await
    }
    pub async fn acquire_for_matting(&self) -> Option<Acquisition> {
        self.acquire(ServiceKind::Matting).await
    }
    pub async fn acquire_for_avatar(&self) -> Option<GpuAvatarGuard> {
        self.acquire(ServiceKind::Avatar).await
    }
    pub async fn acquire_for_flux(&self) -> Option<GpuFluxGuard> {
        self.acquire(ServiceKind::Flux).await
    }
    pub async fn acquire_for_songgen(&self) -> Option<GpuSonggenGuard> {
        self.acquire(ServiceKind::Songgen).await
    }
    pub async fn acquire_for_ltx(&self) -> Option<GpuLtxGuard> {
        self.acquire(ServiceKind::Ltx).await
    }
    pub async fn acquire_for_wan_vace(&self) -> Option<GpuWanVaceGuard> {
        self.acquire(ServiceKind::WanVace).await
    }
    pub async fn acquire_for_ltx2b(&self) -> Option<GpuLtx2bGuard> {
        self.acquire(ServiceKind::Ltx2b).await
    }
    pub async fn acquire_for_krea2(&self) -> Option<GpuKrea2Guard> {
        self.acquire(ServiceKind::Krea2).await
    }
    pub async fn acquire_for_minimax_h3(&self) -> Option<GpuMinimaxH3Guard> {
        self.acquire(ServiceKind::MinimaxH3).await
    }
    pub async fn acquire_for_llm(&self) -> Option<GpuLlmGuard> {
        self.acquire(ServiceKind::Llm).await
    }

    // ── Deprecated lane predicates (now stable stubs) ────────────────────────
    // The TTS-preempt cancel token + heavy-busy latch are gone. Render loops that
    // poll these simply never self-cancel (co-residency is budget-managed). The
    // LLM is a normal service now, so it never "bounces" on a heavy render.

    /// Always false — there is no heavy slot any more.
    pub fn is_heavy_busy(&self) -> bool {
        false
    }
    /// Back-compat alias of [`Self::is_heavy_busy`].
    pub fn is_avatar_busy(&self) -> bool {
        false
    }
    /// Always false — TTS no longer preempts via a cancel token.
    pub fn is_tts_requested(&self) -> bool {
        false
    }
    /// Always false — vision no longer preempts STT via a pending flag.
    pub fn is_vision_requested(&self) -> bool {
        false
    }
    /// No-op — there is no TTS-yield cooldown dance any more.
    pub async fn yield_to_tts(&self) {}
    /// No-op (kept for the settings page) — the TTS hold window is gone.
    pub fn set_tts_hold_seconds(&self, _seconds: u64) {}
    /// No-op (kept for the settings page) — the LLM drain ceiling is gone; the
    /// drain happens inline during eviction.
    pub fn set_llm_drain_seconds(&self, _seconds: u64) {}
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex as StdMutex;
    use std::time::Duration;

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

    /// Build a descriptor. `evicts` = the kinds allowed to evict THIS service.
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

    /// A base URL whose drain/unload endpoints all 200.
    ///
    /// Eviction is fail-closed: an unload the gate cannot confirm leaves the victim's
    /// reservation in place (see [`GpuLock::evict_for_placement`]). A test that wants
    /// the eviction to SUCCEED therefore has to give the victim a reachable endpoint,
    /// exactly as production does — without one it is asserting the failure path.
    async fn stub_unload_endpoint() -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move {
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            while let Ok((mut sock, _)) = listener.accept().await {
                tokio::spawn(async move {
                    // Drain whatever the client sends until it stops; `Connection:
                    // close` means one request per socket, so a short read is the
                    // end of it. Reading it out matters: replying and closing while
                    // the body is still in flight gives reqwest a broken pipe rather
                    // than the 200.
                    let mut buf = [0u8; 2048];
                    let _ = sock.read(&mut buf).await;
                    let _ = sock
                        .write_all(
                            b"HTTP/1.1 200 OK\r\ncontent-length: 2\r\nconnection: close\r\n\r\nok",
                        )
                        .await;
                    let _ = sock.shutdown().await;
                });
            }
        });
        url
    }

    /// Put the LLM behind a [`stub_unload_endpoint`] so evicting it can be confirmed.
    async fn stub_llm(g: &GpuLock) {
        g.set_llm_service(HeavyService {
            kind: HeavyKind::Llm,
            url: stub_unload_endpoint().await,
            unload_path: "/unload".into(),
            drain_path: "/drain".into(),
        });
    }

    /// A configured gate places a service on its home card and reports the UUID.
    #[tokio::test]
    async fn acquire_places_on_home() {
        let g = GpuLock::new();
        g.set_cards(cards());
        g.set_engine_config(ServiceKind::Flux, desc(7500, B, &[B, A], &[], false));
        let a = g.acquire(ServiceKind::Flux).await.expect("flux acquires");
        assert_eq!(a.chosen_gpu(), Some(B));
    }

    /// Unconfigured service → no-op guard (no card target), fully back-compatible.
    #[tokio::test]
    async fn unconfigured_is_noop() {
        let g = GpuLock::new();
        let a = g.acquire(ServiceKind::Flux).await.expect("noop guard");
        assert_eq!(a.chosen_gpu(), None);
    }

    /// Two services on different cards run concurrently (both acquire, no block).
    #[tokio::test]
    async fn two_cards_run_concurrently() {
        let g = GpuLock::new();
        g.set_cards(cards());
        g.set_engine_config(ServiceKind::Avatar, desc(7500, B, &[B], &[], false));
        g.set_engine_config(ServiceKind::Llm, desc(10000, A, &[A], &[], false));
        let av = g.acquire(ServiceKind::Avatar).await.unwrap();
        let llm = g.acquire(ServiceKind::Llm).await.unwrap();
        assert_eq!(av.chosen_gpu(), Some(B));
        assert_eq!(llm.chosen_gpu(), Some(A));
    }

    /// Same-card contention with no evictable victim BLOCKS until budget frees.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn full_card_blocks_then_unblocks() {
        let g = GpuLock::new();
        g.set_cards(vec![Card {
            uuid: A.into(),
            total_mb: 12000,
            fast: false,
            enabled: true,
        }]);
        // Two mutually-un-evictable 7.5GB services can't co-reside on a 12GB card.
        g.set_engine_config(ServiceKind::Avatar, desc(7500, A, &[A], &[], false));
        g.set_engine_config(ServiceKind::Ltx, desc(7500, A, &[A], &[], false));

        let first = g.acquire(ServiceKind::Avatar).await.unwrap();
        assert_eq!(first.chosen_gpu(), Some(A));

        let order = Arc::new(StdMutex::new(Vec::<&'static str>::new()));
        let g2 = g.clone();
        let order2 = order.clone();
        let task = tokio::spawn(async move {
            let _ltx = g2.acquire(ServiceKind::Ltx).await.unwrap();
            order2.lock().unwrap().push("ltx");
        });

        tokio::time::sleep(Duration::from_millis(50)).await;
        order.lock().unwrap().push("first-pre-drop");
        drop(first); // frees A's budget → wakes the blocked Ltx
        task.await.unwrap();

        assert_eq!(*order.lock().unwrap(), vec!["first-pre-drop", "ltx"]);
    }

    /// A named evictor evicts an *idle-but-resident* worker that lists it when the
    /// card is full. Vision is idle-but-loaded (an `Observed` placement, no live
    /// guard) — the never-SIGKILL-in-flight rule only spares residents that are
    /// mid-request (`holders > 0`), so this idle warm worker is fair game.
    #[tokio::test]
    async fn named_evictor_evicts_resident() {
        let g = GpuLock::new();
        g.set_cards(vec![Card {
            uuid: A.into(),
            total_mb: 12000,
            fast: false,
            enabled: true,
        }]);
        // Vision declares it can be evicted by Flux (a renderer).
        g.set_engine_config(
            ServiceKind::Vision,
            desc(8000, A, &[A], &[ServiceKind::Flux], false),
        );
        g.set_engine_config(ServiceKind::Flux, desc(8000, A, &[A], &[], false));
        g.set_service_urls(None, None, Some(stub_unload_endpoint().await));
        // An idle Vision worker is resident (reconcile observed it), holding no guard.
        g.observe(ServiceKind::Vision, A);
        // Flux ∈ Vision.evictable_by and Vision is idle → evicts it to fit on the card.
        let fx = g.acquire(ServiceKind::Flux).await.unwrap();
        assert_eq!(fx.chosen_gpu(), Some(A));
        // Vision's placement was evicted from the ledger.
        let occ = g.occupancy();
        let residents = &occ[0].residents;
        assert!(residents.iter().any(|(k, _)| *k == ServiceKind::Flux));
        assert!(!residents.iter().any(|(k, _)| *k == ServiceKind::Vision));
    }

    /// Disabled service → acquire returns None (caller 503s).
    #[tokio::test]
    async fn disabled_service_returns_none() {
        let g = GpuLock::new();
        g.set_cards(cards());
        g.set_engine_config(ServiceKind::Tts, desc(4500, B, &[B], &[], false));
        g.set_service_enabled(ServiceKind::Tts, false).await;
        assert!(g.acquire(ServiceKind::Tts).await.is_none());
        g.set_service_enabled(ServiceKind::Tts, true).await;
        assert!(g.acquire(ServiceKind::Tts).await.is_some());
    }

    /// Drop frees the budget so the next acquirer fits.
    #[tokio::test]
    async fn drop_frees_budget() {
        let g = GpuLock::new();
        g.set_cards(vec![Card {
            uuid: A.into(),
            total_mb: 8000,
            fast: false,
            enabled: true,
        }]);
        g.set_engine_config(ServiceKind::Flux, desc(7500, A, &[A], &[], false));
        {
            let _f = g.acquire(ServiceKind::Flux).await.unwrap();
            assert_eq!(g.occupancy()[0].used_mb, 7500);
        }
        assert_eq!(g.occupancy()[0].used_mb, 0);
    }

    /// service_states lists every kind and reflects toggles.
    #[tokio::test]
    async fn service_states_lists_every_kind() {
        let g = GpuLock::new();
        let states = g.service_states();
        assert_eq!(states.len(), ServiceKind::ALL.len());
        g.set_service_enabled(ServiceKind::Tts, false).await;
        let tts = g
            .service_states()
            .into_iter()
            .find(|(k, _)| *k == ServiceKind::Tts)
            .map(|(_, e)| e);
        assert_eq!(tts, Some(false));
    }

    /// The LLM's standing reservation is dropped on disable and restored on
    /// re-enable (else a spilling heavy would admit on top of the warm worker).
    #[tokio::test]
    async fn llm_seed_survives_disable_enable() {
        let g = GpuLock::new();
        g.set_cards(vec![Card {
            uuid: A.into(),
            total_mb: 12000,
            fast: false,
            enabled: true,
        }]);
        g.set_engine_config(ServiceKind::Llm, desc(10000, A, &[A], &[], true));
        stub_llm(&g).await;
        g.seed_resident(ServiceKind::Llm);
        assert_eq!(g.occupancy()[0].used_mb, 10000);
        g.set_service_enabled(ServiceKind::Llm, false).await;
        assert_eq!(g.occupancy()[0].used_mb, 0, "disable drops the seed");
        g.set_service_enabled(ServiceKind::Llm, true).await;
        assert_eq!(
            g.occupancy()[0].used_mb,
            10000,
            "re-enable restores the seed"
        );
    }

    /// The LLM is dropped while a renderer holds its home card, then reclaims the
    /// home once the renderer finishes and the card is clean again.
    #[tokio::test]
    async fn rehome_reclaims_home_when_clean() {
        let g = GpuLock::new();
        g.set_cards(vec![Card {
            uuid: A.into(),
            total_mb: 12000,
            fast: false,
            enabled: true,
        }]);
        g.set_engine_config(
            ServiceKind::Llm,
            desc(10000, A, &[A], &[ServiceKind::Flux], true),
        );
        g.set_engine_config(ServiceKind::Flux, desc(7500, A, &[A], &[], false));
        stub_llm(&g).await;
        g.seed_resident(ServiceKind::Llm);
        // Flux evicts the LLM (single card → nowhere to relocate → dropped).
        let flux = g.acquire(ServiceKind::Flux).await.unwrap();
        g.rehome_standing();
        assert!(
            !g.occupancy()[0]
                .residents
                .iter()
                .any(|(k, _)| *k == ServiceKind::Llm),
            "LLM stays dropped while flux holds its home"
        );
        // Flux finishes → home clean → the LLM reclaims it on the next tick.
        drop(flux);
        g.rehome_standing();
        assert!(
            g.occupancy()[0]
                .residents
                .iter()
                .any(|(k, _)| *k == ServiceKind::Llm),
            "LLM reclaims home once the card is clean"
        );
    }

    /// `active_services` reflects live guards, not mere residency: a held guard
    /// is busy; after it drops (worker still warm/observed) the service is not.
    #[tokio::test]
    async fn active_services_reflects_live_guard_only() {
        let g = GpuLock::new();
        g.set_cards(cards());
        g.set_engine_config(ServiceKind::Flux, desc(7500, B, &[B, A], &[], false));
        {
            let _f = g.acquire(ServiceKind::Flux).await.unwrap();
            assert!(
                g.active_services().contains(&ServiceKind::Flux),
                "held → busy"
            );
        }
        // Guard dropped; a reconcile tick observes the still-loaded worker.
        g.observe(ServiceKind::Flux, B);
        assert!(
            g.occupancy()
                .iter()
                .any(|c| c.residents.iter().any(|(k, _)| *k == ServiceKind::Flux)),
            "still a resident"
        );
        assert!(
            !g.active_services().contains(&ServiceKind::Flux),
            "idle observed worker is not busy"
        );
    }

    /// After the gate evicts a service, a racing reconcile `observe` for it is
    /// ignored until the eviction cooldown lapses (so the still-dying worker
    /// isn't resurrected onto the card its evictor just took).
    #[tokio::test]
    async fn observe_ignored_during_evict_cooldown() {
        let g = GpuLock::new();
        g.set_cards(vec![Card {
            uuid: A.into(),
            total_mb: 12000,
            fast: false,
            enabled: true,
        }]);
        g.set_engine_config(
            ServiceKind::Llm,
            desc(10000, A, &[A], &[ServiceKind::Flux], true),
        );
        g.set_engine_config(ServiceKind::Flux, desc(7500, A, &[A], &[], false));
        stub_llm(&g).await;
        g.seed_resident(ServiceKind::Llm);
        // Flux evicts the LLM (relocation impossible → cancel) and sets its cooldown.
        let _flux = g.acquire(ServiceKind::Flux).await.unwrap();
        assert!(g.recently_evicted(ServiceKind::Llm));
        // A reconcile tick tries to re-observe the LLM — must be ignored.
        g.observe(ServiceKind::Llm, A);
        let occ = g.occupancy();
        assert!(
            !occ[0].residents.iter().any(|(k, _)| *k == ServiceKind::Llm),
            "LLM not resurrected during cooldown"
        );
        assert!(
            occ[0]
                .residents
                .iter()
                .any(|(k, _)| *k == ServiceKind::Flux)
        );
    }

    /// Every ServiceKind round-trips through as_str/parse — guards the split-out
    /// `Sa3` wire string against a missing match arm.
    #[test]
    fn service_kind_str_roundtrips() {
        for k in ServiceKind::ALL {
            assert_eq!(ServiceKind::parse(k.as_str()), Some(k));
        }
        assert_eq!(ServiceKind::parse("sa3"), Some(ServiceKind::Sa3));
    }

    /// `reconcile_targets` resolves each kind to its OWN engine URL by tag, even
    /// though acestep+sa3 share one `music` Vec — guards the `light_tags`
    /// routing against a wrong tag. A tag missing from `light_tags` resolves to
    /// no URL, which makes eviction a silent no-op.
    #[test]
    fn reconcile_targets_resolve_split_kinds_by_tag() {
        let g = GpuLock::new();
        g.set_cards(cards());
        for k in [ServiceKind::Tts, ServiceKind::Music, ServiceKind::Sa3] {
            g.set_engine_config(k, desc(3000, B, &[B], &[], false));
        }
        g.set_tts_services(vec![TtsService {
            tag: "breeze".into(),
            url: "http://breeze".into(),
            unload_path: "/u".into(),
        }]);
        g.set_music_service("acestep", Some("http://acestep".into()), "/u");
        g.set_music_service("sa3", Some("http://sa3".into()), "/u");
        let targets = g.reconcile_targets();
        let find = |k: ServiceKind| {
            targets
                .iter()
                .find(|(kk, _)| *kk == k)
                .map(|(_, u)| u.as_str())
        };
        assert_eq!(find(ServiceKind::Tts), Some("http://breeze"));
        assert_eq!(find(ServiceKind::Music), Some("http://acestep"));
        assert_eq!(find(ServiceKind::Sa3), Some("http://sa3"));
    }

    /// kobbler registers its directly-driven TTS service under the legacy
    /// `chatterbox` tag via `set_service_urls`; that tag must still resolve or
    /// the gate can never unload it.
    #[test]
    fn reconcile_targets_resolve_legacy_chatterbox_tag() {
        let g = GpuLock::new();
        g.set_cards(cards());
        g.set_engine_config(ServiceKind::Tts, desc(3000, B, &[B], &[], false));
        g.set_service_urls(None, Some("http://chatterbox".into()), None);
        let targets = g.reconcile_targets();
        assert_eq!(
            targets
                .iter()
                .find(|(k, _)| *k == ServiceKind::Tts)
                .map(|(_, u)| u.as_str()),
            Some("http://chatterbox")
        );
    }

    // ── The queue's non-blocking surface ─────────────────────────────────────

    /// A single 12 GB card with one 7.5 GB service on it: the first acquire wins,
    /// the second is REFUSED rather than parked, and names what it is waiting for.
    /// This is the whole difference from `acquire` — the dispatcher has to be able
    /// to move on to the next job.
    #[tokio::test]
    async fn try_acquire_refuses_instead_of_parking() {
        let g = GpuLock::new();
        g.set_cards(vec![Card {
            uuid: A.into(),
            total_mb: 12000,
            fast: false,
            enabled: true,
        }]);
        g.set_engine_config(ServiceKind::Avatar, desc(7500, A, &[A], &[], false));
        g.set_engine_config(ServiceKind::Ltx, desc(7500, A, &[A], &[], false));

        let held = g
            .try_acquire(ServiceKind::Avatar)
            .await
            .expect("first fits");
        assert_eq!(held.chosen_gpu(), Some(A));

        let blocked = g.try_acquire(ServiceKind::Ltx).await.err();
        let Some(AdmitFailure::Place(PlaceBlock::NoCardFits { blockers, .. })) = blocked else {
            panic!("expected a reasoned refusal, got {blocked:?}");
        };
        assert!(
            blockers.contains(&ServiceKind::Avatar),
            "names the resident"
        );

        drop(held);
        assert!(g.try_acquire(ServiceKind::Ltx).await.is_ok());
    }

    /// `can_place` answers the same question as `try_acquire` and reserves nothing —
    /// the dispatcher calls it for every queued row on every tick, so a query that
    /// took budget would deadlock the queue against itself.
    #[tokio::test]
    async fn can_place_reserves_nothing() {
        let g = GpuLock::new();
        g.set_cards(vec![Card {
            uuid: A.into(),
            total_mb: 12000,
            fast: false,
            enabled: true,
        }]);
        g.set_engine_config(ServiceKind::Flux, desc(7500, A, &[A], &[], false));

        assert!(g.can_place(ServiceKind::Flux).is_none(), "would place");
        for _ in 0..5 {
            let _ = g.can_place(ServiceKind::Flux);
        }
        assert_eq!(g.occupancy()[0].used_mb, 0, "queries never reserve");

        let _held = g.try_acquire(ServiceKind::Flux).await.unwrap();
        // Same kind again is a REUSE, not a block — so ask about a different one.
        g.set_engine_config(ServiceKind::Krea2, desc(7500, A, &[A], &[], false));
        assert!(matches!(
            g.can_place(ServiceKind::Krea2),
            Some(AdmitFailure::Place(PlaceBlock::NoCardFits { .. }))
        ));
        assert_eq!(g.occupancy()[0].used_mb, 7500, "still just the one holder");
    }

    /// A disabled service is a blocked REASON, not a failure — the queue stamps it,
    /// skips, and starts the job when the operator re-enables. Disabling flux at
    /// 22:00 must not burn the overnight batch.
    #[tokio::test]
    async fn disabled_service_is_a_reason_not_a_failure() {
        let g = GpuLock::new();
        g.set_cards(cards());
        g.set_engine_config(ServiceKind::Flux, desc(7500, B, &[B], &[], false));
        g.set_service_enabled(ServiceKind::Flux, false).await;

        assert_eq!(
            g.can_place(ServiceKind::Flux),
            Some(AdmitFailure::ServiceDisabled)
        );
        assert_eq!(
            g.try_acquire(ServiceKind::Flux).await.err(),
            Some(AdmitFailure::ServiceDisabled)
        );

        g.set_service_enabled(ServiceKind::Flux, true).await;
        assert!(g.can_place(ServiceKind::Flux).is_none());
    }

    /// An operator pin lands on the named card, and a pin at a DISABLED card is
    /// refused — a disabled card is off limits to every path, override or not.
    #[tokio::test]
    async fn pin_targets_a_card_and_never_a_disabled_one() {
        let g = GpuLock::new();
        g.set_cards(cards());
        // Home is B, but the operator wants it on A.
        g.set_engine_config(ServiceKind::Flux, desc(7500, B, &[B, A], &[], false));
        let a = g
            .try_acquire_on(ServiceKind::Flux, A, false)
            .await
            .expect("pins to A");
        assert_eq!(a.chosen_gpu(), Some(A));
        drop(a);

        g.set_card_enabled(A, false).await.unwrap();
        assert_eq!(
            g.try_acquire_on(ServiceKind::Flux, A, false).await.err(),
            Some(AdmitFailure::Place(PlaceBlock::PinUnavailable {
                card: A.to_string()
            }))
        );
    }

    /// `force` overrides the victim's `evictable_by` rule and nothing else. Krea2
    /// is not named as an evictor of the LLM, so without force it cannot place;
    /// with force it evicts and takes the card.
    #[tokio::test]
    async fn force_overrides_the_eviction_rule() {
        let g = GpuLock::new();
        g.set_cards(vec![Card {
            uuid: A.into(),
            total_mb: 12000,
            fast: false,
            enabled: true,
        }]);
        // The LLM names ONLY Flux as an evictor — krea2 is not allowed to bump it.
        g.set_engine_config(
            ServiceKind::Llm,
            desc(10000, A, &[A], &[ServiceKind::Flux], true),
        );
        g.set_engine_config(ServiceKind::Krea2, desc(7500, A, &[A], &[], false));
        stub_llm(&g).await;
        g.seed_resident(ServiceKind::Llm);

        assert!(
            matches!(
                g.try_acquire_on(ServiceKind::Krea2, A, false).await,
                Err(AdmitFailure::Place(PlaceBlock::NoCardFits { .. }))
            ),
            "the rule holds without force"
        );
        let forced = g
            .try_acquire_on(ServiceKind::Krea2, A, true)
            .await
            .expect("force evicts the LLM");
        assert_eq!(forced.chosen_gpu(), Some(A));
    }

    /// Pending queue demand suppresses the LLM re-home. Without this the LLM is
    /// re-seeded in the gap between two queued jobs, so the second pays an eviction
    /// the first had already paid — every gap, all night.
    #[tokio::test]
    async fn pending_demand_suppresses_rehome() {
        let g = GpuLock::new();
        g.set_cards(vec![Card {
            uuid: A.into(),
            total_mb: 12000,
            fast: false,
            enabled: true,
        }]);
        g.set_engine_config(
            ServiceKind::Llm,
            desc(10000, A, &[A], &[ServiceKind::Flux], true),
        );
        g.set_engine_config(ServiceKind::Flux, desc(7500, A, &[A], &[], false));
        stub_llm(&g).await;
        g.seed_resident(ServiceKind::Llm);

        let flux = g.try_acquire(ServiceKind::Flux).await.unwrap();
        drop(flux); // job N finished; job N+1 is still queued

        // More flux work is coming, and flux is named in the LLM's evictable_by.
        g.set_pending_demand(&[ServiceKind::Flux]);
        g.rehome_standing();
        assert!(
            !g.occupancy()[0]
                .residents
                .iter()
                .any(|(k, _)| *k == ServiceKind::Llm),
            "no re-home into the gap — job N+1 would only evict it again"
        );

        // Queue drained → the LLM reclaims its home exactly as before.
        g.set_pending_demand(&[]);
        g.rehome_standing();
        assert!(
            g.occupancy()[0]
                .residents
                .iter()
                .any(|(k, _)| *k == ServiceKind::Llm),
            "an empty queue restores the pre-queue behaviour"
        );
    }

    /// Demand that could never evict the standing service does NOT suppress the
    /// re-home — the suppression is targeted, not a blanket "queue is busy" latch.
    #[tokio::test]
    async fn unrelated_pending_demand_still_rehomes() {
        let g = GpuLock::new();
        g.set_cards(vec![Card {
            uuid: A.into(),
            total_mb: 12000,
            fast: false,
            enabled: true,
        }]);
        g.set_engine_config(
            ServiceKind::Llm,
            desc(10000, A, &[A], &[ServiceKind::Flux], true),
        );
        stub_llm(&g).await;
        g.set_pending_demand(&[ServiceKind::Krea2]); // not in the LLM's evictable_by
        g.rehome_standing();
        assert!(
            g.occupancy()[0]
                .residents
                .iter()
                .any(|(k, _)| *k == ServiceKind::Llm)
        );
    }

    /// A resident koblem never placed (observed, no guard) whose server reports a
    /// direct caller using it — a live STT stream — is not idle either.
    #[tokio::test]
    async fn unload_if_idle_refuses_a_reported_busy_resident() {
        let g = GpuLock::new();
        g.set_cards(cards());
        g.set_engine_config(ServiceKind::Flux, desc(7500, B, &[B], &[], false));
        g.set_flux_service(HeavyService {
            kind: HeavyKind::Flux,
            url: stub_unload_endpoint().await,
            unload_path: "/v1/admin/unload".into(),
            drain_path: "/v1/admin/drain".into(),
        });

        // Observed out-of-band (no guard) but the server says a direct caller is using it.
        g.observe(ServiceKind::Flux, B);
        g.set_reported_busy(ServiceKind::Flux, true);
        assert!(
            !g.unload_if_idle(ServiceKind::Flux).await,
            "never reclaim a busy resident"
        );
        assert!(g.active_services().contains(&ServiceKind::Flux));

        g.set_reported_busy(ServiceKind::Flux, false);
        assert!(g.unload_if_idle(ServiceKind::Flux).await);

        // unobserve (worker reports unloaded) clears a stale busy flag.
        g.set_reported_busy(ServiceKind::Flux, true);
        g.unobserve(ServiceKind::Flux);
        assert!(!g.is_reported_busy(ServiceKind::Flux));
    }

    /// The early-unload planner refuses while a guard is held: an idle resident is
    /// a warm worker, a held one is a running render.
    #[tokio::test]
    async fn unload_if_idle_refuses_a_held_placement() {
        let g = GpuLock::new();
        g.set_cards(cards());
        g.set_engine_config(ServiceKind::Flux, desc(7500, B, &[B], &[], false));
        g.set_flux_service(HeavyService {
            kind: HeavyKind::Flux,
            url: stub_unload_endpoint().await,
            unload_path: "/v1/admin/unload".into(),
            drain_path: "/v1/admin/drain".into(),
        });

        let held = g.try_acquire(ServiceKind::Flux).await.unwrap();
        assert!(
            !g.unload_if_idle(ServiceKind::Flux).await,
            "never unload under a live render"
        );
        drop(held);

        // Warm but idle (reconcile observed it) → the planner may reclaim it.
        g.observe(ServiceKind::Flux, B);
        assert!(g.unload_if_idle(ServiceKind::Flux).await);
        assert!(
            !g.occupancy()
                .iter()
                .any(|c| c.residents.iter().any(|(k, _)| *k == ServiceKind::Flux))
        );
    }
    // ── exclusive ("Beefy") mode ─────────────────────────────────────────────

    #[tokio::test]
    async fn exclusive_refuses_every_peer_and_admits_the_keeper() {
        let g = GpuLock::new();
        g.set_cards(cards());
        g.set_engine_config(ServiceKind::Flux, desc(7500, B, &[B], &[], false));
        g.set_engine_config(ServiceKind::Llm, desc(10000, A, &[A], &[], true));

        g.set_exclusive(Some(ServiceKind::Llm));
        assert_eq!(g.exclusive(), Some(ServiceKind::Llm));

        // Every OTHER kind is refused, through all three surfaces.
        assert!(g.acquire(ServiceKind::Flux).await.is_none());
        assert_eq!(
            g.try_acquire(ServiceKind::Flux).await.err(),
            Some(AdmitFailure::BoxLocked {
                keep: ServiceKind::Llm
            })
        );
        assert_eq!(
            g.can_place(ServiceKind::Flux),
            Some(AdmitFailure::BoxLocked {
                keep: ServiceKind::Llm
            })
        );
        // ...and the keeper itself is unaffected.
        assert!(g.can_place(ServiceKind::Llm).is_none());
        assert!(g.acquire(ServiceKind::Llm).await.is_some());
    }

    /// 🔴 Ordering regression guard. `set_enabled(false)` makes `can_place`
    /// answer `None` = "it would place"; if the box-lock check sits after that
    /// early return, the dispatcher starts jobs straight into a locked box.
    #[tokio::test]
    async fn exclusive_survives_a_globally_disabled_gate() {
        let g = GpuLock::new();
        g.set_cards(cards());
        g.set_engine_config(ServiceKind::Flux, desc(7500, B, &[B], &[], false));
        g.set_enabled(false);
        g.set_exclusive(Some(ServiceKind::Llm));

        assert_eq!(
            g.can_place(ServiceKind::Flux),
            Some(AdmitFailure::BoxLocked {
                keep: ServiceKind::Llm
            })
        );
    }

    /// 🔴 `force` overrides the residents' `evictable_by` policy. It must NOT
    /// override the box lock — an operator pin is not a licence to land on top
    /// of a 25 GiB model.
    #[tokio::test]
    async fn exclusive_is_not_overridden_by_a_forced_pin() {
        let g = GpuLock::new();
        g.set_cards(cards());
        g.set_engine_config(ServiceKind::Flux, desc(7500, B, &[B], &[], false));
        g.set_exclusive(Some(ServiceKind::Llm));

        assert_eq!(
            g.try_acquire_on(ServiceKind::Flux, B, true).await.err(),
            Some(AdmitFailure::BoxLocked {
                keep: ServiceKind::Llm
            })
        );
        assert_eq!(
            g.can_place_on(ServiceKind::Flux, B, true),
            Some(AdmitFailure::BoxLocked {
                keep: ServiceKind::Llm
            })
        );
    }

    #[tokio::test]
    async fn clearing_exclusive_lets_peers_back_in() {
        let g = GpuLock::new();
        g.set_cards(cards());
        g.set_engine_config(ServiceKind::Flux, desc(7500, B, &[B], &[], false));

        g.set_exclusive(Some(ServiceKind::Llm));
        assert!(g.try_acquire(ServiceKind::Flux).await.is_err());
        g.set_exclusive(None);
        assert!(g.exclusive().is_none());
        assert!(g.try_acquire(ServiceKind::Flux).await.is_ok());
    }

    /// Entering the mode must release an acquirer already parked on the vacancy
    /// notify — otherwise it hangs for the whole Beefy session (minutes to hours)
    /// instead of getting a prompt refusal.
    #[tokio::test]
    async fn exclusive_releases_an_already_parked_acquirer() {
        let g = GpuLock::new();
        g.set_cards(cards());
        // 🔴 Two of these must NOT both fit on B (16000 MB) or the second one
        // never parks and the test passes without ever exercising the wait path.
        g.set_engine_config(ServiceKind::Flux, desc(9000, B, &[B], &[], false));
        g.set_engine_config(ServiceKind::Krea2, desc(9000, B, &[B], &[], false));
        let _held = g.acquire(ServiceKind::Flux).await.expect("first fits");

        let g2 = g.clone();
        let parked = tokio::spawn(async move { g2.acquire(ServiceKind::Krea2).await });
        // Let it reach the wait.
        tokio::time::sleep(Duration::from_millis(200)).await;
        g.set_exclusive(Some(ServiceKind::Llm));

        let out = tokio::time::timeout(Duration::from_secs(5), parked)
            .await
            .expect("parked acquirer must not hang past the 2 s backstop")
            .expect("task panicked");
        assert!(out.is_none(), "a locked box must refuse, not admit");
    }

    /// 🔴 The eviction race: Beefy is `set_exclusive` THEN `evict_all_except`, and
    /// the sweep frees VRAM on purpose. A waiter woken by the first call must not
    /// get its placement in on the card the second one just cleared.
    #[tokio::test]
    async fn a_parked_acquirer_refuses_even_when_the_sweep_frees_its_card() {
        let g = GpuLock::new();
        g.set_cards(cards());
        g.set_engine_config(ServiceKind::Flux, desc(9000, B, &[B], &[], false));
        g.set_engine_config(ServiceKind::Krea2, desc(9000, B, &[B], &[], false));
        let held = g.acquire(ServiceKind::Flux).await.expect("first fits");

        let g2 = g.clone();
        let parked = tokio::spawn(async move { g2.acquire(ServiceKind::Krea2).await });
        tokio::time::sleep(Duration::from_millis(200)).await;

        g.set_exclusive(Some(ServiceKind::Llm));
        // The sweep's effect on the budget, without the HTTP: card B is now free
        // and the waiter is awake. It must still refuse.
        drop(held);

        let out = tokio::time::timeout(Duration::from_secs(5), parked)
            .await
            .expect("must not hang")
            .expect("task panicked");
        assert!(
            out.is_none(),
            "admitted onto the card the exclusive sweep just freed"
        );
    }

    #[tokio::test]
    async fn evict_all_except_clears_peers_and_spares_the_keeper() {
        let g = GpuLock::new();
        g.set_cards(cards());
        let url = stub_unload_endpoint().await;
        g.set_flux_service(HeavyService {
            kind: HeavyKind::Flux,
            url,
            unload_path: "/unload".into(),
            drain_path: "/drain".into(),
        });
        g.set_engine_config(ServiceKind::Flux, desc(7500, B, &[B], &[], false));
        g.set_engine_config(ServiceKind::Llm, desc(10000, A, &[A], &[], true));
        stub_llm(&g).await;
        // The keeper has to actually BE on a card for "spared" to mean anything —
        // without its standing reservation the final assert only proves it was
        // never there.
        g.seed_resident(ServiceKind::Llm);
        // 🔴 HOLD the guard. `placer.release` REMOVES a Guard-sourced placement
        // once holders hit zero, so dropping it would leave nothing resident to
        // evict and the test would pass vacuously. `evict_for_placement` (unlike
        // `unload_if_idle`) does not refuse a held placement — the operator asked
        // for the whole box.
        let _flux = g.acquire(ServiceKind::Flux).await.expect("flux fits");

        g.set_exclusive(Some(ServiceKind::Llm));
        let failed = g.evict_all_except(ServiceKind::Llm).await;
        assert!(failed.is_empty(), "confirmed unloads must not be reported");

        let residents: Vec<ServiceKind> = g
            .occupancy()
            .into_iter()
            .flat_map(|c| c.residents.into_iter().map(|(k, _)| k))
            .collect();
        assert!(
            !residents.contains(&ServiceKind::Flux),
            "flux should be gone"
        );
        assert!(
            residents.contains(&ServiceKind::Llm),
            "the keeper must be spared — beefy unloads its own worker itself"
        );
    }

    /// 🔴 Fails CLOSED: a service whose unload cannot be confirmed KEEPS its
    /// reservation and is reported, never silently dropped — admitting on top of
    /// VRAM that may still be resident is the failure this path exists to avoid.
    #[tokio::test]
    async fn evict_all_except_retains_an_unconfirmed_victim() {
        let g = GpuLock::new();
        g.set_cards(cards());
        // No heavy service registered for Flux → its unload cannot be proven.
        g.set_engine_config(ServiceKind::Flux, desc(7500, B, &[B], &[], false));
        g.set_engine_config(ServiceKind::Llm, desc(10000, A, &[A], &[], true));
        stub_llm(&g).await;
        // Held, for the same reason as the test above.
        let _held = g.acquire(ServiceKind::Flux).await.expect("flux fits");

        g.set_exclusive(Some(ServiceKind::Llm));
        let failed = g.evict_all_except(ServiceKind::Llm).await;
        assert_eq!(failed, vec![ServiceKind::Flux]);

        let residents: Vec<ServiceKind> = g
            .occupancy()
            .into_iter()
            .flat_map(|c| c.residents.into_iter().map(|(k, _)| k))
            .collect();
        assert!(
            residents.contains(&ServiceKind::Flux),
            "an unconfirmed victim must KEEP its reservation"
        );
    }

    fn resident(g: &GpuLock, kind: ServiceKind) -> bool {
        g.occupancy()
            .iter()
            .any(|c| c.residents.iter().any(|(k, _)| *k == kind))
    }

    /// The text embedder rides out Beefy when the reservation leaves it room, and
    /// is unloaded like any peer when it does not.
    #[tokio::test]
    async fn exclusive_sweep_spares_a_standing_co_tenant_only_when_it_fits() {
        for (reserve_b, spared) in [(14000, true), (15000, false)] {
            let g = GpuLock::new();
            g.set_cards(cards());
            g.set_text_embed_url(Some(stub_unload_endpoint().await));
            g.set_engine_config(
                ServiceKind::TextEmbed,
                desc(1600, B, &[B], &[ServiceKind::Flux], false),
            );
            g.seed_resident(ServiceKind::TextEmbed);
            g.set_exclusive_with(
                ServiceKind::Llm,
                &[ServiceKind::TextEmbed],
                HashMap::from([(A.to_string(), 12000), (B.to_string(), reserve_b)]),
            );
            let failed = g
                .evict_all_except_sparing(ServiceKind::Llm, &[ServiceKind::TextEmbed])
                .await;
            assert!(failed.is_empty());
            assert_eq!(
                resident(&g, ServiceKind::TextEmbed),
                spared,
                "reserve {reserve_b} on a 16000 card with a 1600 embedder"
            );
        }
    }

    /// A standing kind is re-seeded on its home card once room comes back, the
    /// same way the LLM is.
    #[tokio::test]
    async fn rehome_reseeds_the_text_embedder() {
        let g = GpuLock::new();
        g.set_cards(cards());
        g.set_engine_config(
            ServiceKind::TextEmbed,
            desc(1600, B, &[B], &[ServiceKind::Flux], false),
        );
        assert!(!resident(&g, ServiceKind::TextEmbed));
        g.rehome_standing();
        assert!(resident(&g, ServiceKind::TextEmbed));
        assert_eq!(
            ServiceKind::parse("text_embed"),
            Some(ServiceKind::TextEmbed)
        );
        assert_eq!(ServiceKind::TextEmbed.as_str(), "text_embed");
    }

    // ── exclusive allow-list ─────────────────────────────────────────────────

    /// The shape Beefy ships: the LLM standing on A with stt/tts/flux in its
    /// `evictable_by`, A reserved to 8700, B given no entry (reserved whole).
    async fn beefy_with_allow(g: &GpuLock, allow: &[ServiceKind]) {
        g.set_cards(cards());
        g.set_engine_config(
            ServiceKind::Llm,
            desc(
                9000,
                A,
                &[A],
                &[ServiceKind::Stt, ServiceKind::Tts, ServiceKind::Flux],
                true,
            ),
        );
        // Reachable on purpose: a wrongful eviction of the keeper would then
        // SUCCEED and show up as a missing resident, instead of failing quietly.
        stub_llm(g).await;
        g.seed_resident(ServiceKind::Llm);
        g.set_engine_config(ServiceKind::Stt, desc(1500, A, &[A, B], &[], false));
        g.set_engine_config(ServiceKind::Tts, desc(3000, B, &[B, A], &[], false));
        g.set_engine_config(ServiceKind::Flux, desc(7500, B, &[B, A], &[], false));
        g.set_exclusive_with(
            ServiceKind::Llm,
            allow,
            HashMap::from([(A.to_string(), 8700)]),
        );
    }

    fn llm_resident(g: &GpuLock) -> bool {
        g.occupancy()
            .iter()
            .any(|c| c.residents.iter().any(|(k, _)| *k == ServiceKind::Llm))
    }

    #[tokio::test]
    async fn exclusive_allow_list_admits_beside_the_reservation() {
        let g = GpuLock::new();
        beefy_with_allow(&g, &[ServiceKind::Stt, ServiceKind::Tts]).await;
        assert_eq!(
            g.exclusive_allow(),
            vec![ServiceKind::Stt, ServiceKind::Tts]
        );

        let tts = g
            .try_acquire(ServiceKind::Tts)
            .await
            .expect("tts fits beside the reservation");
        assert_eq!(tts.chosen_gpu(), Some(A), "its home is reserved whole");

        // 300 MiB left on A: an allowed kind that does not fit is a RESOURCE wait,
        // not a box lock — and the keeper was not sacrificed to make room.
        let stt = g.try_acquire(ServiceKind::Stt).await;
        assert!(
            matches!(stt, Err(AdmitFailure::Place(PlaceBlock::NoCardFits { .. }))),
            "{:?}",
            stt.as_ref().err()
        );
        assert!(
            llm_resident(&g),
            "the keeper must never be evicted for a co-tenant"
        );

        // Off the list → still locked; the keeper still admits.
        assert_eq!(
            g.can_place(ServiceKind::Flux),
            Some(AdmitFailure::BoxLocked {
                keep: ServiceKind::Llm
            })
        );
        assert!(g.acquire(ServiceKind::Llm).await.is_some());

        drop(tts);
        assert!(
            g.try_acquire(ServiceKind::Stt).await.is_ok(),
            "room came back"
        );
    }

    /// 🔴 An allowed kind the placer will not budget is refused, not waved through
    /// as a no-op guard that never looks at the reservation.
    #[tokio::test]
    async fn exclusive_allow_list_needs_a_live_budget() {
        let g = GpuLock::new();
        beefy_with_allow(&g, &[ServiceKind::Stt, ServiceKind::Vision]).await;
        // Allowed, but no engine config for it.
        assert_eq!(
            g.can_place(ServiceKind::Vision),
            Some(AdmitFailure::BoxLocked {
                keep: ServiceKind::Llm
            })
        );
        // Allowed and configured, but the gate is globally off.
        g.set_enabled(false);
        assert_eq!(
            g.can_place(ServiceKind::Stt),
            Some(AdmitFailure::BoxLocked {
                keep: ServiceKind::Llm
            })
        );
        assert!(g.acquire(ServiceKind::Stt).await.is_none());
    }

    /// An empty reservation map is the whole-box lock, allow-list or not.
    #[tokio::test]
    async fn exclusive_allow_list_without_a_reservation_admits_nothing() {
        let g = GpuLock::new();
        g.set_cards(cards());
        g.set_engine_config(ServiceKind::Stt, desc(1500, A, &[A, B], &[], false));
        g.set_exclusive_with(ServiceKind::Llm, &[ServiceKind::Stt], HashMap::new());
        assert!(matches!(
            g.try_acquire(ServiceKind::Stt).await,
            Err(AdmitFailure::Place(_))
        ));
    }

    #[tokio::test]
    async fn leaving_exclusive_drops_the_reservation() {
        let g = GpuLock::new();
        beefy_with_allow(&g, &[ServiceKind::Stt]).await;
        g.set_exclusive(None);
        assert!(g.exclusive_allow().is_empty());
        assert!(g.occupancy().iter().all(|c| c.reserved_mb.is_none()));
        assert!(g.try_acquire(ServiceKind::Flux).await.is_ok());
    }
}
