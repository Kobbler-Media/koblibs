//! Shared wire types for the koblem API.
//!
//! These types define the contract between koblem and its callers.
//! Both the server and client deserialize/serialize these — changing
//! a field here updates both sides at compile time.

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::Value;
use uuid::Uuid;

// ── Image → rig (SPAR3D + RigAnything) ────────────────────────────────

/// Texture policy for a generated game-ready GLB. `ProjectFront` preserves
/// source-detail on the visible side; `TwoSidedMirror` is ONLY for explicitly
/// symmetric side-on props (text and characters must not use it).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RigTextureMode {
    Generated,
    #[default]
    ProjectFront,
    TwoSidedMirror,
}

/// Semantic intent emitted by the character-writing model.  The image-to-rig
/// worker asks SAM3 for each named part and only labels a RigAnything joint
/// when its visible skin weights overlap that mask confidently.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RigSemanticPart {
    pub name: String,
    pub motion: String,
    pub count: u32,
}

/// JSON request accepted by Koblem's image-to-rig endpoint. The image itself
/// is supplied as a multipart `image` part in browser/CLI calls; this type
/// intentionally carries only durable processing options.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageToRigRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seed: Option<u64>,
    #[serde(default)]
    pub texture_mode: RigTextureMode,
    /// Native source texture resolution for projective baking. The service
    /// currently supports 512 and 1024; absent means its production default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_texture_resolution: Option<u32>,
    /// Request ordinary glTF skin data. It stays opt-in for generic API users;
    /// Paperworld turns it on for a character and still treats an unmapped rig
    /// as static/root-bob rather than a generic humanoid animation target.
    #[serde(default)]
    pub rig: bool,
    /// Run RMBG-2.0 first. This is default-on for UI/API consumers because
    /// SPAR3D conditions best on a transparent cutout.
    #[serde(default = "default_remove_background")]
    pub remove_background: bool,
    /// Emit plain, independently loadable LOD1 and LOD2 GLBs alongside the
    /// full-detail model.  They are intended for a game-asset bundle, where
    /// selecting whole GLBs by distance works in ordinary glTF runtimes.
    #[serde(default = "default_generate_lods")]
    pub generate_lods: bool,
    /// Optional character intent, for example arm/swing ×2 and head/bob ×1.
    /// Absent preserves the generic RigAnything naming contract.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub semantic_parts: Option<Vec<RigSemanticPart>>,
}

fn default_remove_background() -> bool {
    true
}

fn default_generate_lods() -> bool {
    true
}

impl Default for ImageToRigRequest {
    fn default() -> Self {
        Self {
            seed: None,
            texture_mode: RigTextureMode::ProjectFront,
            source_texture_resolution: Some(1024),
            rig: false,
            remove_background: true,
            generate_lods: true,
            semantic_parts: None,
        }
    }
}

/// Durable result returned by Koblem after it has saved the GLB in its
/// generation gallery. `url` is a standard authenticated generation URL.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageToRigResponse {
    pub generation_id: String,
    pub url: String,
    /// ZIP bundle: `model.glb`, optional `model.lod1.glb` / `model.lod2.glb`,
    /// original source, manifest and integration README.
    pub bundle_url: String,
    /// Persisted gallery URLs for the independently-loadable LOD sidecars,
    /// ordered LOD1 then LOD2. Empty when the caller opted out.
    #[serde(default)]
    pub lod_urls: Vec<String>,
    #[serde(default)]
    pub generate_lods: bool,
    pub texture_mode: RigTextureMode,
    pub source_texture_resolution: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub matting_elapsed_seconds: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worker_wall_seconds: Option<f64>,
    #[serde(default)]
    pub metadata: Value,
}

/// Health/capabilities used by the 3D page before enabling Submit.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ImageToRigProps {
    #[serde(default)]
    pub configured: bool,
    #[serde(default)]
    pub healthy: bool,
    #[serde(default)]
    pub supports_project_front: bool,
    #[serde(default)]
    pub supports_two_sided_mirror: bool,
    #[serde(default)]
    pub source_texture_resolutions: Vec<u32>,
}

// ── Grammar mode (GBNF selection at the sampling level) ─────────────

/// Grammar mode for grammar-constrained decoding. Wire shape is a single
/// string. Two values are reserved built-ins; everything else is treated as
/// a name to look up in the `grammars` DB table.
///
/// - `"off"`         → no grammar (model unconstrained).
/// - `"review"`      → koblem's built-in accept-list GBNF
///   ([`REVIEW_GRAMMAR`]). The historical default for the
///   review and filter pipelines.
/// - `"<other>"`     → reference to a user-defined grammar stored under that
///   name in the `grammars` table. The server resolves the
///   name to a GBNF string at request time.
///
/// Resolution requires DB access, so unlike [`SamplingValues::enable_thinking`]
/// the grammar mode is NOT applied by [`SamplingConfig::apply_to`]. The
/// koblem server runs its own resolver after sampling overlay completes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GrammarMode {
    Off,
    Review,
    Named(String),
}

/// Reserved name for the built-in review grammar.
pub const GRAMMAR_NAME_REVIEW: &str = "review";
/// Reserved name for the no-grammar mode.
pub const GRAMMAR_NAME_OFF: &str = "off";

impl GrammarMode {
    pub fn from_wire(s: &str) -> Self {
        match s {
            GRAMMAR_NAME_OFF => GrammarMode::Off,
            GRAMMAR_NAME_REVIEW => GrammarMode::Review,
            other => GrammarMode::Named(other.to_string()),
        }
    }

    pub fn as_wire(&self) -> &str {
        match self {
            GrammarMode::Off => GRAMMAR_NAME_OFF,
            GrammarMode::Review => GRAMMAR_NAME_REVIEW,
            GrammarMode::Named(s) => s.as_str(),
        }
    }

    /// True when this mode is the no-grammar selection. Useful for callers
    /// that want to short-circuit the DB resolver.
    pub fn is_off(&self) -> bool {
        matches!(self, GrammarMode::Off)
    }
}

impl Serialize for GrammarMode {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(self.as_wire())
    }
}

impl<'de> Deserialize<'de> for GrammarMode {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        Ok(GrammarMode::from_wire(&s))
    }
}

/// Built-in GBNF for the review accept-list pipeline.
///
/// **`notes` comes BEFORE `accept` by design.** This is a chain-of-thought
/// trick adapted for grammar-constrained decoding: because llama.cpp emits
/// tokens in grammar order, forcing `notes` first lets the model write a
/// brief summary before committing to the accept list. Early versions with
/// `accept` first suffered from the model anchoring on early junk candidates
/// and returning empty accept lists at scale (the Severance 0-picks failure).
///
/// **`accept` entries are objects with `idx` + `reason`** — the parser also
/// tolerates bare integers and `{idx,reason}`/`{idx}` mixes for legacy
/// runs.
pub const REVIEW_GRAMMAR: &str = r#"
root         ::= ws "{" ws "\"notes\"" ws ":" ws notes-val ws "," ws "\"accept\"" ws ":" ws accept-arr ws "}"
accept-arr   ::= "[" ws (accept-obj (ws "," ws accept-obj)*)? ws "]"
accept-obj   ::= "{" ws "\"idx\"" ws ":" ws integer ws "," ws "\"reason\"" ws ":" ws string ws "}"
notes-val    ::= string | "null"
integer      ::= [0-9]+
string       ::= "\"" ( [^"\\] | "\\" (["\\/bfnrt] | "u" [0-9a-fA-F] [0-9a-fA-F] [0-9a-fA-F] [0-9a-fA-F]) )* "\""
ws           ::= [ \t\n]{0,4}
"#;

// ── Sampling config (llama.cpp /v1/chat/completions knobs) ──────────

/// Named sampling preset. `Deterministic` matches koblem's historical
/// hard-coded structured-path behaviour (`temperature: 0.0` only, every
/// other knob unset so llama-server's defaults apply). The two Qwen
/// presets are taken from Qwen's non-thinking / instruct-mode recommended
/// sampling for Qwen3.5. Thinking-mode presets are deliberately not shipped
/// (prior testing against thinking mode was disappointing).
///
/// 🔴 There is NO `--reasoning off` — this comment used to claim there was, and
/// there is no such llama-server flag on either of koblem's servers; both run
/// `--jinja` and nothing else on this axis. Thinking is turned off per REQUEST,
/// by `SamplingConfig::enable_thinking = false`, which koblem writes into
/// `chat_template_kwargs.enable_thinking` (measured working).
///
/// 🔴 And do NOT reach for `reasoning_effort: "none"` inside
/// `chat_template_kwargs` — the only valid values there are `xhigh` (the
/// default), `medium` and `low`; `"none"` makes the template raise and the
/// request comes back HTTP 500. (llama.cpp intercepts a TOP-LEVEL OpenAI
/// `reasoning_effort: "none"` before the template sees it, so that one happens
/// to work — but koblem does not rely on it.)
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SamplingPreset {
    Deterministic,
    QwenInstructGeneral,
    QwenInstructReasoning,
}

/// How much thinking a reasoning-capable chat template should budget, written
/// to `chat_template_kwargs.reasoning_effort`.
///
/// 🔴 **There are EXACTLY three variants and none of them is "none".** Qwen3.8's
/// chat template RAISES on `reasoning_effort: "none"` and llama-server answers
/// **HTTP 500** — measured. Thinking is turned OFF the one way that is measured
/// working: `enable_thinking = false` (which is why [`SamplingConfig::apply_to`]
/// skips emitting this field entirely when thinking is off — an effort budget
/// for a trace that is never produced is noise the template can trip over).
/// A type that cannot spell `"none"` cannot ship it: do not add the variant.
///
/// `Xhigh` is the template's own default; `Medium` and `Low` shorten the trace.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ReasoningEffort {
    Xhigh,
    Medium,
    Low,
}

impl ReasoningEffort {
    /// The exact string the chat template expects.
    pub fn as_wire(self) -> &'static str {
        match self {
            ReasoningEffort::Xhigh => "xhigh",
            ReasoningEffort::Medium => "medium",
            ReasoningEffort::Low => "low",
        }
    }

    /// Parse a wire string. `None` for anything unrecognised — including
    /// `"none"`, which is deliberately unrepresentable (see the type docs).
    pub fn from_wire(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "xhigh" => Some(ReasoningEffort::Xhigh),
            "medium" => Some(ReasoningEffort::Medium),
            "low" => Some(ReasoningEffort::Low),
            _ => None,
        }
    }
}

impl SamplingPreset {
    /// Concrete values for this preset. `None` entries mean "leave the
    /// field unset so llama-server's built-in default applies".
    pub fn values(self) -> SamplingValues {
        match self {
            SamplingPreset::Deterministic => SamplingValues {
                temperature: Some(0.0),
                ..SamplingValues::default()
            },
            SamplingPreset::QwenInstructGeneral => SamplingValues {
                temperature: Some(0.7),
                top_p: Some(0.8),
                top_k: Some(20),
                min_p: Some(0.0),
                presence_penalty: Some(1.5),
                repetition_penalty: Some(1.0),
                max_tokens: None,
                enable_thinking: None,
                grammar: None,
                reasoning_effort: None,
            },
            SamplingPreset::QwenInstructReasoning => SamplingValues {
                temperature: Some(1.0),
                top_p: Some(0.95),
                top_k: Some(20),
                min_p: Some(0.0),
                // 🔴 0.0, NOT 1.5. Qwen specifies presence_penalty 1.5 for
                // NON-thinking mode and 0.0 for thinking mode. A presence penalty
                // on a reasoning trace fights the model: a chain of thought
                // revisits the same tokens by design, and penalising that pushes
                // it off them. This was 1.5 — see the note on `SamplingPreset`
                // about thinking mode testing "disappointingly", which this may
                // well explain.
                presence_penalty: Some(0.0),
                repetition_penalty: Some(1.0),
                max_tokens: None,
                enable_thinking: None,
                grammar: None,
                reasoning_effort: None,
            },
        }
    }
}

/// Resolved sampling values ready to apply to a llama-server request body.
#[derive(Debug, Clone, Default)]
pub struct SamplingValues {
    pub temperature: Option<f64>,
    pub top_p: Option<f64>,
    pub top_k: Option<i32>,
    pub min_p: Option<f64>,
    pub presence_penalty: Option<f64>,
    pub repetition_penalty: Option<f64>,
    pub max_tokens: Option<i32>,
    /// When `Some`, written to the body as
    /// `chat_template_kwargs.enable_thinking`. Controls whether the chat
    /// template auto-injects the `<think>\n` opener for reasoning-capable
    /// models (Qwen3.5, etc.). Structured-output endpoints set this to
    /// `Some(false)` in their baselines; chat / proxy callers pick.
    pub enable_thinking: Option<bool>,
    /// Selected grammar mode. `None` means "leave the body's `grammar` field
    /// alone" (typical for chat / proxy paths). DB resolution for
    /// [`GrammarMode::Named`] happens server-side outside [`SamplingConfig::apply_to`].
    pub grammar: Option<GrammarMode>,
    /// When `Some`, written to the body as
    /// `chat_template_kwargs.reasoning_effort`. Only meaningful while thinking
    /// is on — [`SamplingConfig::apply_to`] drops it when
    /// `enable_thinking == Some(false)`. See [`ReasoningEffort`] for why there
    /// is no "none".
    pub reasoning_effort: Option<ReasoningEffort>,
}

/// User-facing sampling config. A preset supplies defaults; any explicit
/// field overrides the corresponding preset value. Pure `SamplingConfig::default()`
/// resolves to "leave the body alone" — nothing is injected.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SamplingConfig {
    /// Named preset (see [`SamplingPreset`]). Serialises as the top-level
    /// key `sampling_preset` on the wire — that's what external callers
    /// (OpenAI-shaped JSON, settings-DB blobs, chat UI) send and what
    /// `llm_logs.sampling_preset` stores.
    ///
    /// 🔴 Parsed LENIENTLY: an unrecognised name resolves to `None` rather than
    /// failing the whole struct. Two real callers echo this field back from a
    /// place it may not be a preset at all — the `/v1/chat/completions` proxy
    /// (`from_value(body).unwrap_or_default()`, where a strict error would throw
    /// away the caller's temperature too) and the llm-logs Rerun form (whose
    /// `llm_logs.sampling_preset` column now also carries koblem's *surface key*
    /// for calls that never had a preset). Neither should 422 over a tag.
    #[serde(
        rename = "sampling_preset",
        default,
        deserialize_with = "de_preset",
        skip_serializing_if = "Option::is_none"
    )]
    pub preset: Option<SamplingPreset>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub top_p: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub top_k: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_p: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub presence_penalty: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repetition_penalty: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<i32>,
    /// Toggle for the chat template's `<think>\n` auto-injector. `None` =
    /// inherit from a lower-priority layer (preset / DB default / call-site
    /// baseline). Structured-output baselines set this to `Some(false)` so
    /// review/filter/validate/rank stay deterministic by default; chat and
    /// the proxy default to `None` so the caller chooses.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enable_thinking: Option<bool>,
    /// Grammar mode selector. Wire key is `grammar_mode` so it doesn't
    /// collide with llama.cpp's `grammar` field (which carries raw GBNF
    /// text on the OpenAI-compatible proxy path).
    ///
    /// `None` = inherit. Review and filter pipelines baseline this to
    /// [`GrammarMode::Review`]; chat and the proxy default to `None` (no
    /// grammar applied unless the caller asks for one). When the resolved
    /// mode is [`GrammarMode::Named`], the koblem server looks the name up
    /// in the `grammars` table — clients only ever speak names.
    #[serde(
        rename = "grammar_mode",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub grammar: Option<GrammarMode>,
    /// Thinking budget for the chat template. `None` = inherit (the template's
    /// own default, `xhigh` on Qwen3.8). Inert when thinking is off — see
    /// [`ReasoningEffort`], which by construction cannot express the `"none"`
    /// value that makes the template raise a 500.
    ///
    /// 🔴 Deserialisation is LENIENT on purpose (`de_reasoning_effort`): an
    /// OpenAI-compatible caller is entitled to send a top-level
    /// `reasoning_effort: "none"` (llama.cpp intercepts that one itself), and a
    /// hard parse error here would make `serde_json::from_value::<SamplingConfig>`
    /// in `chat_completions.rs` fail and silently drop the caller's ENTIRE
    /// sampling override. Unknown values resolve to `None` = inherit.
    #[serde(
        default,
        deserialize_with = "de_reasoning_effort",
        skip_serializing_if = "Option::is_none"
    )]
    pub reasoning_effort: Option<ReasoningEffort>,
}

/// Lenient `Option<SamplingPreset>` parser — see the field docs.
fn de_preset<'de, D: Deserializer<'de>>(d: D) -> Result<Option<SamplingPreset>, D::Error> {
    let raw = Option::<String>::deserialize(d)?;
    Ok(raw.as_deref().and_then(|s| match s {
        "deterministic" => Some(SamplingPreset::Deterministic),
        "qwen_instruct_general" => Some(SamplingPreset::QwenInstructGeneral),
        "qwen_instruct_reasoning" => Some(SamplingPreset::QwenInstructReasoning),
        _ => None,
    }))
}

/// Lenient `Option<ReasoningEffort>` parser — see the field docs.
fn de_reasoning_effort<'de, D: Deserializer<'de>>(
    d: D,
) -> Result<Option<ReasoningEffort>, D::Error> {
    let raw = Option::<String>::deserialize(d)?;
    Ok(raw.as_deref().and_then(ReasoningEffort::from_wire))
}

impl SamplingConfig {
    /// Resolve to concrete `SamplingValues`: start from preset, overlay
    /// any explicit fields.
    pub fn resolve(&self) -> SamplingValues {
        let mut values = self.preset.map(SamplingPreset::values).unwrap_or_default();
        if self.temperature.is_some() {
            values.temperature = self.temperature;
        }
        if self.top_p.is_some() {
            values.top_p = self.top_p;
        }
        if self.top_k.is_some() {
            values.top_k = self.top_k;
        }
        if self.min_p.is_some() {
            values.min_p = self.min_p;
        }
        if self.presence_penalty.is_some() {
            values.presence_penalty = self.presence_penalty;
        }
        if self.repetition_penalty.is_some() {
            values.repetition_penalty = self.repetition_penalty;
        }
        if self.max_tokens.is_some() {
            values.max_tokens = self.max_tokens;
        }
        if self.enable_thinking.is_some() {
            values.enable_thinking = self.enable_thinking;
        }
        if self.grammar.is_some() {
            values.grammar = self.grammar.clone();
        }
        if self.reasoning_effort.is_some() {
            values.reasoning_effort = self.reasoning_effort;
        }
        values
    }

    /// Merge another config on top of this one: other's explicit fields
    /// (and preset) override self's. Used to stack request-override over
    /// DB-default over call-site-baseline.
    pub fn overlay(&self, other: &SamplingConfig) -> SamplingConfig {
        SamplingConfig {
            preset: other.preset.or(self.preset),
            temperature: other.temperature.or(self.temperature),
            top_p: other.top_p.or(self.top_p),
            top_k: other.top_k.or(self.top_k),
            min_p: other.min_p.or(self.min_p),
            presence_penalty: other.presence_penalty.or(self.presence_penalty),
            repetition_penalty: other.repetition_penalty.or(self.repetition_penalty),
            max_tokens: other.max_tokens.or(self.max_tokens),
            enable_thinking: other.enable_thinking.or(self.enable_thinking),
            grammar: other.grammar.clone().or_else(|| self.grammar.clone()),
            reasoning_effort: other.reasoning_effort.or(self.reasoning_effort),
        }
    }

    /// Merge resolved sampling values into a llama-server request body.
    /// Only writes fields that resolve to `Some(_)` — absent fields are
    /// left untouched so the server's own defaults stay authoritative.
    ///
    /// **Does NOT apply [`SamplingValues::grammar`].** Resolving a named
    /// grammar requires DB access; the koblem server runs its own
    /// [`GrammarMode`]-aware step after `apply_to` returns.
    pub fn apply_to(&self, body: &mut Value) {
        let Some(obj) = body.as_object_mut() else {
            return;
        };
        let v = self.resolve();
        if let Some(t) = v.temperature {
            obj.insert("temperature".into(), Value::from(t));
        }
        if let Some(t) = v.top_p {
            obj.insert("top_p".into(), Value::from(t));
        }
        if let Some(t) = v.top_k {
            obj.insert("top_k".into(), Value::from(t));
        }
        if let Some(t) = v.min_p {
            obj.insert("min_p".into(), Value::from(t));
        }
        if let Some(t) = v.presence_penalty {
            obj.insert("presence_penalty".into(), Value::from(t));
        }
        if let Some(t) = v.repetition_penalty {
            obj.insert("repetition_penalty".into(), Value::from(t));
        }
        if let Some(t) = v.max_tokens {
            obj.insert("max_tokens".into(), Value::from(t));
        }
        if let Some(b) = v.enable_thinking {
            // Merge into existing `chat_template_kwargs` if the body already
            // has one (e.g. an OpenAI-compatible caller passing extra
            // template options) — only the `enable_thinking` key is touched.
            let kwargs = obj
                .entry("chat_template_kwargs".to_string())
                .or_insert_with(|| Value::Object(serde_json::Map::new()));
            if let Some(kw) = kwargs.as_object_mut() {
                kw.insert("enable_thinking".into(), Value::Bool(b));
            }
        }
        // Same merge, same object — but only while thinking is actually on.
        // With `enable_thinking: false` the template produces no trace at all,
        // so an effort budget is noise; and the one value that would mean
        // "no trace" (`"none"`) makes Qwen3.8's template raise a 500, which is
        // why `ReasoningEffort` cannot spell it.
        if let Some(effort) = v.reasoning_effort
            && v.enable_thinking != Some(false)
        {
            let kwargs = obj
                .entry("chat_template_kwargs".to_string())
                .or_insert_with(|| Value::Object(serde_json::Map::new()));
            if let Some(kw) = kwargs.as_object_mut() {
                kw.insert(
                    "reasoning_effort".into(),
                    Value::String(effort.as_wire().to_string()),
                );
            }
        }
    }
}

// ── Chat completions (OpenAI-compatible) ────────────────────────────

/// A chat message with role and content.
/// Supports system, user, assistant, and tool roles.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: String,
    #[serde(default)]
    pub content: Option<String>,
    /// Tool calls made by the assistant (present when role=assistant and model wants to call tools).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ToolCall>>,
    /// Tool call ID this message is responding to (present when role=tool).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    /// Function name (used by some clients when role=tool).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

/// OpenAI-compatible tool call from the assistant.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    pub r#type: String,
    pub function: FunctionCall,
}

/// Function call details within a tool call.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FunctionCall {
    pub name: String,
    pub arguments: String,
}

/// OpenAI-compatible tool definition.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Tool {
    pub r#type: String,
    pub function: FunctionDef,
}

/// Function definition within a tool.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FunctionDef {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub parameters: Option<Value>,
    #[serde(default)]
    pub strict: Option<bool>,
}

/// OpenAI-compatible chat completion request.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatCompletionRequest {
    pub messages: Vec<ChatMessage>,
    #[serde(default = "default_max_tokens")]
    pub max_tokens: u32,
    #[serde(default)]
    pub temperature: Option<f64>,
    #[serde(default)]
    pub stream: bool,
    /// Model name — accepted for OpenAI compatibility but ignored by koblem.
    #[serde(default)]
    pub model: Option<String>,
    /// Tool definitions (OpenAI format) — koblem rewrites MeiliSearch tool params for the LLM.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<Tool>>,
}

fn default_max_tokens() -> u32 {
    1024
}

/// OpenAI-compatible chat completion response (non-streaming).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatCompletionResponse {
    pub id: String,
    pub object: String,
    pub created: u64,
    pub model: String,
    pub choices: Vec<ChatChoice>,
    #[serde(default)]
    pub usage: Option<Usage>,
}

/// A single choice in the chat completion response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatChoice {
    pub index: u32,
    pub message: ChatChoiceMessage,
    pub finish_reason: Option<String>,
}

/// Message content from a chat completion choice.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatChoiceMessage {
    pub role: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ToolCall>>,
}

/// Token usage statistics.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Usage {
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    pub total_tokens: u32,
}

// ── Health ───────────────────────────────────────────────────────────

/// Health check response from koblem.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HealthResponse {
    pub status: String,
    pub llama_server: bool,
    /// Python-service reachability. Additive — absent from older koblem builds.
    #[serde(default)]
    pub vision: bool,
    #[serde(default)]
    pub tts: bool,
    #[serde(default)]
    pub stt: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub llama_model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vision_model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tts_model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stt_model: Option<String>,
    /// Does the LLM currently behind koblem accept `input_audio` content parts?
    ///
    /// 🔴 DERIVED, NEVER PROBED. `/v1/models` reports nothing about the mmproj,
    /// so there is no way to ask the server whether it has an audio encoder.
    /// koblem derives it from WHICH server it is pointed at: the resident gemma
    /// ships a unified vision+audio mmproj, Beefy mode's Qwen3.8-27B ships a
    /// VISION-ONLY one. Images work in both.
    ///
    /// Defaults to `true` so an older koblem — which had no Beefy mode and was
    /// therefore always on gemma — is read correctly rather than as "no audio".
    #[serde(default = "default_true")]
    pub llama_audio_input: bool,
}

// ── Auth / API keys ─────────────────────────────────────────────────

/// One row of `GET /api/v1/auth/api-keys`. `key` is the plaintext value, only
/// retained for keys created through this API.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiKeySummary {
    pub id: Uuid,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_used_at: Option<String>,
    pub created_at: String,
    #[serde(default)]
    pub revoked: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CreateApiKeyRequest {
    pub name: String,
}

/// The only time the plaintext key is returned on creation — store it now.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateApiKeyResponse {
    pub id: Uuid,
    pub key: String,
}

/// `GET /api/v1/auth/me`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AuthMeResponse {
    pub authenticated: bool,
}

// ── GPU scheduler status ────────────────────────────────────────────

/// One entry of `GET /api/v1/gpu/services`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GpuServiceState {
    pub name: String,
    pub enabled: bool,
    /// Actively serving a request right now — NOT merely holding budget while idle.
    #[serde(default)]
    pub busy: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GpuServicesResponse {
    #[serde(default)]
    pub services: Vec<GpuServiceState>,
}

/// One configured card from `GET /api/v1/gpu/scheduler/cards`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GpuCard {
    pub uuid: String,
    #[serde(default)]
    pub label: String,
    #[serde(default)]
    pub total_mb: i32,
    #[serde(default)]
    pub fast: bool,
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub sort_order: i32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GpuCardsResponse {
    #[serde(default)]
    pub cards: Vec<GpuCard>,
}

/// A service currently holding budget on a card.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GpuResident {
    pub service: String,
    pub peak_mb: i64,
}

/// One card's live VRAM budget from `GET /api/v1/gpu/scheduler/occupancy`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GpuCardOccupancy {
    pub uuid: String,
    #[serde(default)]
    pub total_mb: i64,
    #[serde(default)]
    pub used_mb: i64,
    #[serde(default)]
    pub free_mb: i64,
    /// nvidia-smi's view, when the reconcile loop could observe it. Clamps the
    /// budget-derived `free_mb`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub measured_free_mb: Option<i64>,
    /// Beefy's reservation on this card while it holds the box — the 27B's
    /// footprint, standing in for its single-card ledger row, so `used_mb` is
    /// NOT the sum of `residents` then. Absent when Beefy is off.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reserved_mb: Option<i64>,
    #[serde(default)]
    pub fast: bool,
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub residents: Vec<GpuResident>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GpuOccupancyResponse {
    #[serde(default)]
    pub cards: Vec<GpuCardOccupancy>,
}

/// One per-service placement config from `GET /api/v1/gpu/scheduler/engines`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GpuEngineConfig {
    pub service: String,
    #[serde(default)]
    pub peak_mb: i32,
    #[serde(default)]
    pub home_gpu: String,
    #[serde(default)]
    pub candidate_gpus: Vec<String>,
    /// Services allowed to evict THIS one. Empty = never auto-evicted.
    #[serde(default)]
    pub evictable_by: Vec<String>,
    #[serde(default)]
    pub relocatable: bool,
    /// Reserved for future conditional eviction rules — stored and round-tripped
    /// through the settings UI, not yet interpreted.
    #[serde(default)]
    pub rules_json: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GpuEnginesResponse {
    #[serde(default)]
    pub engines: Vec<GpuEngineConfig>,
}

// ── Review candidates ──────────────────────────────────────────────

/// Structured quality profile carried on the wire. Mirrors kobload's
/// `QualityProfileRow` minus DB metadata. koblem renders these fields
/// directly into the prompt instead of parsing free-form strings.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct QualityProfile {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_resolution: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_resolution: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub preferred_sources: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub preferred_codecs: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub preferred_audio: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub preferred_languages: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub rejected_sources: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub rejected_languages: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub rejected_terms: Vec<String>,
    #[serde(default)]
    pub reject_hardcoded_subs: bool,
    #[serde(default)]
    pub reject_remux: bool,
}

/// Per-variant wanted context. Replaces the previous flat `wanted_*` fields
/// so koblem can render a prompt block tailored to each media type without
/// inferring intent from which fields happen to be `Some`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[derive(Default)]
pub enum WantContext {
    TvEpisode {
        season: String,
        episode: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        episode_title: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        air_date: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        year: Option<i32>,
    },
    TvSeason {
        season: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        year: Option<i32>,
        /// When set, only releases for these specific episodes (or a season
        /// pack covering them) are wanted. When `None`, any episode of the
        /// season is wanted.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        episodes_needed: Option<Vec<nats_jobs::EpisodeRef>>,
    },
    Movie {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        year: Option<i32>,
    },
    SportEvent {
        league_name: String,
        season: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        round: Option<u32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        location: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        session_type: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        event_title: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        event_date: Option<String>,
    },
    /// Catch-all for media types that don't have a tailored prompt path
    /// (book, music, game, …). koblem falls back to a generic render.
    #[default]
    Other,
}

/// A candidate release for review, with kobload's scoring signals.
///
/// Quality attributes ride along inside `parsed_quality` (the canonical
/// `nats_jobs::ParsedQuality`) so every bit kobload's filename parser
/// extracted is available to koblem without re-parsing.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ReviewCandidate {
    /// Raw release filename.
    pub name: String,
    /// Whether kobload's scoring pipeline accepted this candidate.
    pub kobload_accepted: bool,
    /// Kobload's total score for this candidate.
    pub kobload_score: f64,
    /// Full parsed-quality block from kobload's filename parser. Embedded
    /// structurally so koblem can surface audio / language / HDR / 3D / sub
    /// flags etc. without inferring from the raw name.
    #[serde(default)]
    pub parsed_quality: nats_jobs::ParsedQuality,
    /// Structural fields parsed from the filename via `release.media`.
    /// Independent from `parsed_quality` — these are show / season /
    /// episode / year extraction, not quality attributes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parsed_season: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parsed_episode: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parsed_title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parsed_year: Option<i32>,
    /// All rejection reasons from kobload's scoring pipeline.
    #[serde(default)]
    pub reject_reasons: Vec<nats_jobs::RejectReason>,
    #[serde(default)]
    pub quality_rejected: bool,
    #[serde(default)]
    pub quality_reject_reasons: Vec<nats_jobs::QualityRejectReason>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub structural_reject_reason: Option<nats_jobs::StructuralRejectReason>,
    /// Release-group tier bonus from TRaSH guides.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tier_bonus: Option<f64>,
    /// When this release was published (ISO date string).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub publish_date: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seeders: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub votes: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub grabs: Option<i32>,
}

/// Request to review a batch of release candidates.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ReviewRequest {
    /// Kobbler entity UUID for disambiguation lookups.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_id: Option<String>,
    /// Loose pre-filter title used for the prompt's WANTED line and for
    /// the candidate title-overlap pre-filter (e.g. `"MotoGP 2026"`).
    /// Specific event/episode details live in `want`.
    pub wanted_title: String,
    /// Structured wanted context. koblem branches on the variant to render
    /// a per-variant prompt block.
    #[serde(default)]
    pub want: WantContext,
    /// Structured quality preferences. Replaces the historical
    /// `quality_wants`/`quality_rejects` semicolon-mashed strings.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quality_profile: Option<QualityProfile>,
    /// Candidate releases to review.
    pub candidates: Vec<ReviewCandidate>,
    /// Per-request sampling override. Merged on top of the koblem-side
    /// DB default (`sampling_review_candidates`). Absent fields fall
    /// through to the DB default, then to the hardcoded `Deterministic`
    /// baseline. Kobbler/kobload callers leave this `None` today.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sampling: Option<SamplingConfig>,
}

/// One candidate release in the LLM's accept list.
///
/// The LLM is shown a numbered candidate list (1-based) and emits an `idx`
/// referring to that ordinal — server-side resolves it back to a release name.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReviewAccept {
    /// 1-based index into the candidate list.
    pub idx: usize,
    /// One short sentence from the LLM explaining the accept.
    pub reason: String,
}

/// One row of the server-computed diff between kobload's accept set and the
/// LLM's accept set. Used in all three buckets (`agreed`, `vetoed`, `promoted`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReviewDelta {
    /// 1-based index into the candidate list (matches the prompt numbering).
    pub idx: usize,
    /// Release filename, resolved from the candidate list.
    pub name: String,
    /// LLM reason, if available (always present for `agreed` and `promoted`,
    /// `None` for `vetoed` since the LLM was silent on rejects).
    pub reason: Option<String>,
}

/// Response from the review-candidates endpoint.
///
/// The LLM emits a blind accept-list; the server diffs that against the
/// candidates kobload originally accepted to compute three buckets:
/// - `agreed`: kobload accepted AND LLM accepted (high-confidence picks).
/// - `vetoed`: kobload accepted but LLM did not (false positives).
/// - `promoted`: kobload rejected but LLM accepted (rescued false negatives).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReviewResponse {
    pub agreed: Vec<ReviewDelta>,
    pub vetoed: Vec<ReviewDelta>,
    pub promoted: Vec<ReviewDelta>,
    /// The raw `accept` array as parsed from the LLM JSON, BEFORE bucketing.
    /// Surfaced for debugging — lets the playground show "the LLM said idx 3
    /// but we have no candidate at idx 3" and similar parser-side issues.
    #[serde(default)]
    pub accepts: Vec<ReviewAccept>,
    /// Indices the LLM emitted that fell outside the candidate range. Always
    /// empty in normal operation; non-empty signals the model is hallucinating
    /// or the candidate list got truncated mid-prompt.
    #[serde(default)]
    pub dropped_indices: Vec<usize>,
    /// Which context sources contributed to the review.
    #[serde(default)]
    pub context_used: Vec<String>,
    /// Batch-level observation from the LLM (the `notes` field of its JSON).
    pub notes: Option<String>,
    /// The assembled prompt sent to the LLM (for playground debugging).
    pub debug_prompt: Option<String>,
    /// The verbatim LLM JSON response, before any parsing or stripping.
    /// Surfaced so users can see schema drift / parse failures directly.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raw_llm_response: Option<String>,
    /// llama-server's `usage.prompt_tokens` count (input size).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_tokens: Option<i32>,
    /// llama-server's `usage.completion_tokens` count (output size).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completion_tokens: Option<i32>,
    /// Time spent assembling context (milliseconds).
    pub context_assembly_ms: Option<u64>,
    /// Time spent on LLM inference (milliseconds).
    pub llm_inference_ms: Option<u64>,
    /// Review session ID (stored in DB for the decision log).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
}

// ── NATS review integration ───────────────────────────────────────

/// Envelope for a review job published to NATS (`jobs.koblem.review_candidates`).
///
/// kobload builds a `ReviewRequest` from its scored results and wraps it in
/// this envelope with a correlation ID and priority hint. koblem consumes the
/// job, runs the LLM review pipeline, and publishes a `ReviewCompletedEvent`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReviewJob {
    /// Unique ID for correlating the request with its result callback.
    pub correlation_id: Uuid,
    /// The review request payload (same shape as the HTTP endpoint).
    pub request: ReviewRequest,
    /// Priority hint: `"download-verify"` > `"backlog"` > `"rss"`.
    pub priority: String,
}

/// Payload for the review result event (`events.review.completed`).
///
/// Published by koblem after processing a `ReviewJob`. kobload subscribes
/// to these events and (Phase 1) logs them, (Phase 2) acts on vetoes/promotions.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReviewCompletedEvent {
    /// Matches the `correlation_id` from the originating `ReviewJob`.
    pub correlation_id: Uuid,
    /// The full review response (agreed/vetoed/promoted buckets).
    pub response: ReviewResponse,
    /// How long the job waited in koblem's queue before the GPU was available.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub queue_wait_ms: Option<u64>,
}

// ── TTS / STT / Vision (tier-2 kitchen-sink API) ────────────────────

// ── TTS (BreezeTTS 2) ───────────────────────────────────────────────

fn is_false(b: &bool) -> bool {
    !*b
}

/// Request body for `POST /api/v1/tts/breeze/generate`. Wraps every functional
/// knob from `tts-breeze`'s `POST /v1/audio/speech`. Fields not supplied fall
/// through to the engine's own defaults.
///
/// Two defaults differ from a bare OpenAI request and are deliberate:
/// `long` defaults to **true** server-side (chunks internally with rolling
/// context, so callers must NOT pre-chunk), and callers should always stream
/// (`stream_format = "sse"`) — streamed peak VRAM is flat in output length
/// while buffered grows with it.
///
/// `speed` is rendered SERVER-SIDE: `1.0` is a no-op, and a value the engine
/// cannot render is rejected with 400, on which koblem retries once at 1.0 and
/// reports what it actually rendered in the `x-speed-applied` response header.
/// Callers must read that header rather than assume the request took — and must
/// NOT re-apply the rate in the player, since both audio and word timings come
/// back already in the rendered timebase.
/// There is no `language` — the engine is en/zh and infers from text.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TtsGenerateRequest {
    pub input: String,
    /// Voice id (a registered clone). Omit to let the model pick a voice and
    /// hold it for the request.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub voice: Option<String>,
    /// Voice-design instructions (only meaningful without a cloned `voice`).
    /// E.g. "a gruff sea captain, slow and gravelly".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instructions: Option<String>,
    /// Built-in speaker token, `"S0"`..`"S9"` (default `"S0"`). A closed list.
    /// For a *stable* narrator prefer a cloned `voice`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub speaker: Option<String>,
    /// `wav` (default) | `pcm` | `mp3`. Use `pcm` when streaming.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response_format: Option<String>,
    /// Output codec bitrate in kbps for `mp3`. Ignored for `wav` / `pcm`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bitrate_kbps: Option<u32>,
    /// Playback rate applied during synthesis, as a WSOLA time-stretch — pitch is
    /// preserved. `1.0` (or omitted) renders at natural rate. **Range 0.25–4.0;
    /// outside it is a 400.** Anything but 1.0 is a real render at that rate, so
    /// the alignment comes back in the rendered timebase — do not divide it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub speed: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub top_p: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub top_k: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repetition_penalty: Option<f64>,
    /// Deterministic sampling seed; omit for server-random.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seed: Option<u64>,
    /// Classifier-free guidance strength for an `instructions` prompt. `1` (or
    /// absent) is a single pass; `4` is what the model recommends and makes the
    /// instruction land (clearly audible on emotion) at ~+30% render time. The
    /// engine ignores it on a prompt with no instruction.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cfg_scale: Option<f64>,
    /// Generation cap in 12.5 Hz **frames**, not audio tokens (default 750 =
    /// 60 s). Raising it does not buy longer output — past `n_ctx` it turns a
    /// silent truncation into a hard `context overflow`. Use `long` instead.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_new_frames: Option<u32>,
    /// `"sse"` (text/event-stream of base64 `speech.audio.delta` events),
    /// `"audio"` (raw chunks, no envelope), or `"none"` (one buffered blob).
    /// Anything else is a 400 naming the supported set.
    ///
    /// **Prefer `"sse"`.** It is the only transport that can carry forced
    /// alignment — `align = true` on any other is rejected 400 — and the only
    /// one with a `speech.audio.done` event, which is where the truncation flag
    /// lives. `"audio"` is cheaper to parse and that is all.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stream_format: Option<String>,
    /// Vocoder chunk size for streaming.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stream_batch_size: Option<u32>,
    /// First-chunk size — 1 minimises TTFA.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stream_first_batch_size: Option<u32>,
    /// Long-form mode: chunks internally and feeds each chunk's generated codes
    /// back as the next chunk's reference, so the voice carries across the whole
    /// document. **Defaults to true server-side**; callers should not pre-chunk.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub long: Option<bool>,
    /// Words per internal chunk in `long` mode.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chunk_words: Option<u32>,
    /// Chunk-count lookahead in `long` mode. Accepted, but the engine caps the
    /// carried reference by frames (`long_ref_frames`), so this rarely matters.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub buffer: Option<u32>,
    /// Enable forced alignment (word-level timestamps). Requires
    /// `stream_format = "sse"`. Delivery mode controlled by `align_stream`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub align: Option<bool>,
    /// `"final-only"` (one `speech.audio.alignment.final` after the audio ends)
    /// or `"partial"` (interleaved `.partial` events as audio is produced).
    /// **Defaults to `"final-only"`**, so a read-along has to ask for
    /// `"partial"` explicitly or the highlight arrives all at once at the end.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub align_stream: Option<String>,
    /// Where the forced aligner runs: `"gpu"` (default), `"cpu"` — the same
    /// timings for roughly four times the wall clock and no VRAM — or `"off"`,
    /// which keeps the chunk anchors and drops only the word timings.
    ///
    /// Leave it unset and koblem decides against what the card is doing:
    /// alignment is the half of a read-along that can degrade without
    /// silencing the voice, so it is moved rather than allowed to make a
    /// request not fit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub align_device: Option<String>,
    /// Persist generated audio under `gpu_samples/` linked to the log row.
    #[serde(default, skip_serializing_if = "is_false")]
    pub save_sample: bool,
}

/// Single word's timing from the forced aligner. `t0_ms`/`t1_ms` are
/// offsets from the start of the synthesized audio for this request; 80 ms
/// is the underlying resolution. `word_index` matches the position in the
/// server's whitespace-split of `input`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TtsWordTiming {
    pub word_index: u32,
    pub text: String,
    pub t0_ms: u32,
    pub t1_ms: u32,
}

/// `speech.audio.alignment.partial` SSE event payload. Carries the resolved
/// PREFIX of the passage — the words the voice has actually reached — with
/// timings refreshed against the audio produced so far.
///
/// The prefix is monotonic — the engine clamps it to a per-stream high-water
/// mark, so it never retracts — but a word's TIMINGS are re-estimated on every
/// pass and do change. `word_index` is absolute and stable in both `.partial`
/// and `.final`, so the client rule is **overwrite by `word_index`**; appending
/// instead duplicates the passage many times over.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TtsAlignmentPartialEvent {
    pub audio_seen_ms: u32,
    pub words: Vec<TtsWordTiming>,
}

/// `speech.audio.alignment.final` SSE event payload. Locked timings,
/// emitted after all `speech.audio.delta` events and before
/// `speech.audio.done`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TtsAlignmentFinalEvent {
    pub audio_total_ms: u32,
    pub words: Vec<TtsWordTiming>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timings: Option<serde_json::Value>,
}

/// One voice from `GET /api/v1/tts/voices`. The engine reports the encoded
/// reference (`frames`, `codebooks`) and what it holds for the voice; `name`
/// is absent upstream, so koblem mirrors `id` into it for UI uniformity.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TtsVoice {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// A voice the engine ships rather than one the operator cloned. `default`
    /// is the built-in: a real, selectable value meaning "no reference voice",
    /// i.e. the model's own narrator. Built-ins have no sample or transcript to
    /// download and cannot be deleted.
    #[serde(default)]
    pub builtin: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frames: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub codebooks: Option<u32>,
    #[serde(default)]
    pub has_ref_text: bool,
    #[serde(default)]
    pub has_sample: bool,
}

/// Response for `GET /api/v1/tts/voices`.
///
/// The engine's own body carries BOTH a legacy `{"<model_id>": ["default", ..]}`
/// map and a richer `{"model_id", "voices":[..]}` array. They list the SAME ids;
/// the array just adds `builtin`/`frames`/`codebooks`. koblem normalises to the
/// array and surfaces `model_id` separately for diagnostics.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TtsVoicesResponse {
    #[serde(default)]
    pub voices: Vec<TtsVoice>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_id: Option<String>,
}

/// Response from `POST /api/v1/tts/voices` (clone upload).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TtsUploadResponse {
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ref_frames: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frames: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub codebooks: Option<u32>,
}

/// Body for `POST /api/v1/tts/voices/from-library` — koblem fetches the WAV
/// slice from kobbler via `LibraryProvider::fetch_audiobook_clip`, optionally
/// STT-transcribes it for the reference transcript, then registers the clone.
/// Pre-slices because the engine has no `/v1/audio/speech_file` equivalent.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TtsRegisterFromLibraryRequest {
    pub name: String,
    pub audiobook_id: String,
    pub chapter_index: u32,
    pub offset_seconds: f64,
    pub duration_seconds: f64,
    /// When true and STT is configured, the server transcribes the slice and
    /// uses the resulting text as the reference transcript.
    #[serde(default)]
    pub transcribe: bool,
}

/// Multipart form fields for `POST /api/v1/stt/transcribe`. Not actually
/// deserialized from JSON — this is just a spec for the client.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SttTranscribeRequest {
    /// "segment" | "word" | "char"
    #[serde(default)]
    pub timestamp_granularity: Option<String>,
    #[serde(default)]
    pub beam_size: Option<u32>,
    #[serde(default)]
    pub language: Option<String>,
    #[serde(default)]
    pub save_sample: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SttSegment {
    pub start: f64,
    pub end: f64,
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SttTranscribeResponse {
    pub text: String,
    #[serde(default)]
    pub segments: Vec<SttSegment>,
    #[serde(default)]
    pub words: Vec<SttSegment>,
    #[serde(default)]
    pub chars: Vec<SttSegment>,
    #[serde(default)]
    pub language: Option<String>,
    /// True when parakeet preempted mid-transcription because koblem's
    /// cancel_url signalled a higher-priority job is waiting.
    #[serde(default)]
    pub preempted: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gate_wait_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gpu_log_id: Option<i64>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SttLibraryTranscribeRequest {
    pub library_audiobook_id: String,
    #[serde(default)]
    pub offset_seconds: Option<f64>,
    #[serde(default)]
    pub duration_seconds: Option<f64>,
    #[serde(default)]
    pub timestamp_granularity: Option<String>,
    #[serde(default)]
    pub beam_size: Option<u32>,
    #[serde(default)]
    pub language: Option<String>,
    #[serde(default)]
    pub save_sample: bool,
}

/// Multipart knobs of the image routes (`/api/v1/vision/embed`). The image
/// budget is fixed server-side (EmbeddingGemma-2 at 70 image tokens), so only
/// the testbed switch is left.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct VisionEmbedRequest {
    #[serde(default)]
    pub save_sample: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VisionEmbedResponse {
    /// One L2-normalised [`crate::embed::EG2_DIM`]-wide vector per image, in the
    /// same space as text from `/v1/text_embeddings`.
    pub embeddings: Vec<Vec<f32>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gate_wait_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gpu_log_id: Option<i64>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct VisionClassifyRequest {
    pub prompts: Vec<String>,
    #[serde(default)]
    pub return_logits: bool,
    #[serde(default)]
    pub save_sample: bool,
}

/// Image-major: `scores[image][prompt]`. Scores are COSINE similarity of the
/// image and prompt vectors (no sigmoid, no softmax): typically within a few
/// hundredths of each other, so rank them or compare to a per-model threshold —
/// never read one as a probability.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VisionClassifyResponse {
    pub scores: Vec<Vec<f32>>,
    /// The same values as `scores`, when asked for. Kept for callers that
    /// request it; cosine has no separate logit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub logits: Option<Vec<Vec<f32>>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gate_wait_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gpu_log_id: Option<i64>,
}

/// The original `/v1/text_embeddings` body: prompts embedded verbatim. The
/// server still accepts it — it is the `prompts` alias of
/// [`TextEmbeddingsRequest`] — so older callers keep working.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct VisionTextEmbedRequest {
    pub prompts: Vec<String>,
}

/// Reads a [`TextEmbeddingsResponse`] (which the server now sends) down to the
/// fields older callers used.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VisionTextEmbedResponse {
    /// One L2-normalised vector per input prompt, in the same space as
    /// `VisionEmbedResponse::embeddings`.
    pub embeddings: Vec<Vec<f32>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gate_wait_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gpu_log_id: Option<i64>,
}

/// `POST /v1/text_embeddings`. koblem applies the EmbeddingGemma-2 task prefix
/// ([`crate::embed::eg2_text`]), so callers send plain text and never write a
/// prompt string by hand.
///
/// An input longer than the text instance's batch (`TEXT_EMBED_MAX_TOKENS`) is
/// refused with 413 and a [`TextTooLongError`] unless `chunk` is set.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TextEmbeddingsRequest {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub input: Vec<String>,
    /// Legacy alias of `input`, appended after it.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub prompts: Vec<String>,
    /// `None` = [`crate::embed::EmbedTask::Raw`]: embed the text as given.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task: Option<crate::embed::EmbedTask>,
    /// Per-input titles, used by [`crate::embed::EmbedTask::Document`]. Shorter
    /// than `input` (or empty) leaves the rest untitled.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub titles: Vec<Option<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chunk: Option<ChunkOptions>,
}

/// Split long inputs on token boundaries instead of refusing them.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChunkOptions {
    /// Tokens per chunk INCLUDING the task prefix and title. `None` (or above
    /// the server's limit) = the server's limit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
    /// Tokens shared between consecutive chunks. Must be below the chunk's text
    /// budget, or the request is a 400.
    #[serde(default)]
    pub overlap_tokens: u32,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TextEmbeddingsResponse {
    /// One vector per input. With `chunk`, the vector of that input's FIRST chunk.
    pub embeddings: Vec<Vec<f32>>,
    /// [`crate::embed::EG2_MODEL`]. Store it next to the vectors.
    pub model: String,
    pub dim: usize,
    /// Present only when the request set `chunk`: every chunk of every input.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chunks: Option<Vec<TextEmbeddingChunk>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    /// Only when the GPU fallback served the request (the always-on text
    /// instance was down): time spent waiting on the gate.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gate_wait_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gpu_log_id: Option<i64>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct TextEmbeddingChunk {
    pub input_index: usize,
    pub chunk_index: usize,
    /// `[char_start, char_end)` in the input text, counted in Unicode scalar
    /// values (Rust `chars()`, not bytes or UTF-16 units). Consecutive chunks
    /// overlap by `overlap_tokens`.
    pub char_start: usize,
    pub char_end: usize,
    pub embedding: Vec<f32>,
}

/// The 413 body of `/v1/text_embeddings` and `/v1/embeddings`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TextTooLongError {
    pub error: String,
    /// Tokens in the longest offending input (with its prefix), when counted.
    pub tokens: usize,
    pub max_tokens: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_index: Option<usize>,
}

/// Classify *pre-computed* image embeddings against text prompts: cosine of
/// each image vector with each prompt's vector, so callers can store image
/// vectors once and re-score them against any prompt set without the GPU.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct VisionClassifyFromEmbedRequest {
    /// One row per image: the [`crate::embed::EG2_DIM`]-wide vector `/v1/embeddings`
    /// returned for it. A row of any other width (a stored SigLIP2 vector) is a
    /// 400, not a score.
    pub image_embeddings: Vec<Vec<f32>>,
    /// Embedded verbatim ([`crate::embed::EmbedTask::Raw`]).
    pub prompts: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub return_logits: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VisionClassifyFromEmbedResponse {
    /// One row per input image embedding; one column per prompt. Cosine
    /// similarity — see [`VisionClassifyResponse`] for how to read it.
    pub scores: Vec<Vec<f32>>,
    /// The same values as `scores`, when asked for.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub logits: Option<Vec<Vec<f32>>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gate_wait_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gpu_log_id: Option<i64>,
}

// ── Audiobook browsing (kobbler library → koblem playground pickers) ──

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AudiobookSummary {
    pub id: String,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub author: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub narrator: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cover_url: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AudiobookChapter {
    pub index: u32,
    pub title: String,
    /// Server-side file path (as visible to the Python STT/TTS containers via bind mount).
    pub file_path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_seconds: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AudiobookDetail {
    pub id: String,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub author: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub narrator: Option<String>,
    #[serde(default)]
    pub chapters: Vec<AudiobookChapter>,
}

// ── GPU logs (tier-2 request history + rolling retention) ─────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GpuLogEntry {
    pub id: i64,
    pub service: String,
    pub endpoint: String,
    pub request_meta: Value,
    pub response_meta: Value,
    pub gate_wait_ms: i64,
    pub duration_ms: i64,
    pub status: i32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub caller: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sample_id: Option<i64>,
    /// ISO 8601 timestamp.
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GpuLogsListResponse {
    pub entries: Vec<GpuLogEntry>,
    pub total: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GpuLogsStats {
    pub total_entries: i64,
    pub total_samples: i64,
    pub total_sample_bytes: i64,
    pub oldest_created_at: Option<String>,
    pub newest_created_at: Option<String>,
}

// ── ACE-Step music generation (koblem ↔ ace-server) ───────────────────
//
// koblem fronts acestep.cpp's ace-server as the GPU "music" engine. Three
// modes are exposed, all on the turbo DiT (see acestep.cpp/HANDOFF-koblem-
// integration.md): `generate` (text2music, runs the LM code-sketch then the
// turbo DiT), `cover` (lyric-swap via ace `cover-nofsq`, no LM), and `repaint`
// (regenerate a time window via ace `repaint`, no LM). cover/repaint carry a
// source-audio upload, so they're multipart with this request as the JSON part.

/// Which music pipeline to run. Serialises lowercase to match the UI selector;
/// koblem maps it to the ace-server `task_type`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[derive(Default)]
pub enum MusicMode {
    /// text2music — 9B/ACE-LM fills/uses lyrics, code-sketch, turbo DiT.
    #[default]
    Generate,
    /// Lyric-swap remix of a source song (ace `cover-nofsq`, no LM).
    Cover,
    /// Regenerate `[repaint_start, repaint_end)` of a source song (ace `repaint`).
    Repaint,
    /// Isolate one stem (`track`) from a source mix (ace `extract`). Base DiT only,
    /// no LM. One stem per call.
    Extract,
    /// Generate a new instrument track (`track`) over a source backing mix (ace
    /// `lego`). Base DiT only, no LM.
    Lego,
    /// Build a full mix from a single isolated source stem, adding `track` (ace
    /// `complete`). Base DiT only, no LM.
    Complete,
}

/// Who writes the lyrics when the `lyrics` field is left blank on a `generate`
/// request. Only consulted for `generate` with empty lyrics.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[derive(Default)]
pub enum MusicLyricsSource {
    /// ace-server's own small ACE LM writes them (its Phase-1 "expand" pass).
    /// The default — self-contained, no extra LLM call.
    #[default]
    Ace,
    /// koblem prompts the always-resident main LLM (Qwen3.5-9B via llama-server)
    /// to write them first, then hands them to ace-server as provided lyrics.
    Llm,
}

/// Request body for `POST /api/v1/music/generate` (JSON for `generate`; the
/// `request` multipart part for `cover`/`repaint`). Unset optionals fall to
/// the ace-server defaults, except `duration` which koblem auto-computes from
/// the lyric word count when omitted (the LM's own duration guess causes
/// lyric-skipping — see the handoff).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MusicGenerateRequest {
    #[serde(default)]
    pub mode: MusicMode,
    /// Natural-language style/mood/instrument description. Required.
    pub caption: String,
    /// Sung lyrics. `""`/None → written for you (generate only — see
    /// `lyrics_source`); `"[Instrumental]"` → no vocals. For cover, paste the
    /// full replacement lyrics matching the source structure.
    #[serde(default)]
    pub lyrics: Option<String>,
    /// When `lyrics` is blank on a `generate` request, who writes them.
    /// Defaults to the ACE engine's own LM; set to `llm` to use the main 9B.
    #[serde(default)]
    pub lyrics_source: MusicLyricsSource,
    /// Optional brief for what the lyrics should be *about* (theme, story,
    /// mood, narrator) — kept separate from `caption`, which describes the
    /// music. Only used when `lyrics_source = llm` and `lyrics` is blank; the
    /// 9B writes words on this topic in the style of `caption`. Ignored by the
    /// ACE engine (it only reads `caption`).
    #[serde(default)]
    pub lyrics_prompt: Option<String>,
    /// draft-lyrics only: an existing lyric draft to improve in place instead of
    /// writing fresh. When present, the 9B revises it under the same lyric rules
    /// rather than starting from the caption/theme. Ignored by generate.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refine_source: Option<String>,
    /// draft-lyrics only: free-form change to apply to `refine_source` (e.g.
    /// "make the chorus punchier"). Empty with a `refine_source` set → a general
    /// polish. Ignored when `refine_source` is absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refine_instruction: Option<String>,
    /// Target seconds. None → koblem computes `max(30, round(words)) + 12`.
    #[serde(default)]
    pub duration: Option<f64>,
    /// DiT seed; None/-1 → server-random.
    #[serde(default)]
    pub seed: Option<i64>,
    /// cover only: fraction of DiT steps that see the source (`audio_cover_strength`).
    /// Higher = more faithful to the source, lower = follows your new lyrics more.
    /// 0.7–0.9 suits the faithful remix; 0.2–0.5 suits the loose reference. Ignored otherwise.
    #[serde(default)]
    pub cover_strength: Option<f64>,
    /// cover only: which backend cover path to use. `false` (default) → `cover-nofsq`
    /// (faithful remix on clean 25 Hz latents — stays close to structure/melody/timbre,
    /// so it needs lyrics that match the source structure). `true` → `cover` (FSQ
    /// roundtrip — a free reinterpretation "in the style of" the source; the caption
    /// steers, the source gives loose structure, and it tolerates new/different lyrics).
    #[serde(default)]
    pub cover_reference: bool,
    /// cover only: SDEdit init blend (`cover_noise_strength`). 0.0 = pure noise (most
    /// creative freedom, default) → 1.0 = start nearly identical to the source. None →
    /// engine default (0.0). Ignored outside cover.
    #[serde(default)]
    pub cover_noise_strength: Option<f64>,
    /// extract/lego/complete only: the stem to isolate/generate/add — e.g. `vocals`,
    /// `drums`, `bass`, `guitar`, `keyboard`. `complete` also accepts a multi-stem
    /// string like `"VOCALS | DRUMS"`. Ignored by other modes.
    #[serde(default)]
    pub track: Option<String>,
    /// repaint only: region start in seconds (negative outpaints before).
    #[serde(default)]
    pub repaint_start: Option<f64>,
    /// repaint only: region end in seconds (-1 → source duration).
    #[serde(default)]
    pub repaint_end: Option<f64>,
    /// Sung-vocal language as an ISO code ace-server accepts (`en`, `es`, `fr`,
    /// `de`, `ja`, `zh`, …; `unknown` lets the model choose). None → koblem
    /// sends `en`, because ace's metadata FSM otherwise picks a language at
    /// random from its 51-language set. Ignored for `[Instrumental]` (ace
    /// forces `unknown`).
    #[serde(default)]
    pub vocal_language: Option<String>,
    /// `mp3` (default) | `wav16` | `wav24` | `wav32`.
    #[serde(default)]
    pub output_format: Option<String>,
    /// Generate only: number of *distinct* songs to compose from one prompt
    /// (`lm_batch_size`) — different melodies/lyrics, for auditioning options.
    /// Default 1. Ignored by cover/repaint (no LM).
    #[serde(default)]
    pub songs: Option<u32>,
    /// Seed-variations to render *per song* in one DiT batch
    /// (`synth_batch_size`, 1–9). Same composition, different DiT seed. For
    /// cover/repaint this is just the number of renders. Total tracks =
    /// `songs * takes` on generate.
    #[serde(default)]
    pub takes: Option<u32>,
    /// DiT GGUF filename override (empty → server default turbo).
    #[serde(default)]
    pub synth_model: Option<String>,
    /// Override DiT denoising steps (0/None → model default: turbo = 8).
    #[serde(default)]
    pub inference_steps: Option<u32>,
    /// Override flow-matching schedule shift (0/None → model default).
    #[serde(default)]
    pub shift: Option<f64>,
    /// Persist rendered audio under `SONGS_DIR` and link to the gpu_logs row.
    #[serde(default = "default_true")]
    pub save_song: bool,
}

impl Default for MusicGenerateRequest {
    fn default() -> Self {
        Self {
            mode: MusicMode::default(),
            caption: String::new(),
            lyrics: None,
            lyrics_source: MusicLyricsSource::default(),
            lyrics_prompt: None,
            refine_source: None,
            refine_instruction: None,
            duration: None,
            seed: None,
            cover_strength: None,
            cover_reference: false,
            cover_noise_strength: None,
            track: None,
            repaint_start: None,
            repaint_end: None,
            vocal_language: None,
            output_format: None,
            songs: None,
            takes: None,
            synth_model: None,
            inference_steps: None,
            shift: None,
            save_song: true,
        }
    }
}

fn default_true() -> bool {
    true
}

/// One rendered track in a [`MusicGenerateResponse`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MusicTrack {
    /// Filename under `SONGS_DIR` (and the last path segment of `url`).
    pub filename: String,
    /// koblem-relative URL to fetch the audio: `/api/v1/music/file/{filename}`.
    pub url: String,
    /// Seed actually used for this take (resolved from -1 if random).
    pub seed: i64,
    /// Audio MIME type, e.g. `audio/mpeg`.
    pub content_type: String,
    pub bytes: u64,
}

/// Response for `POST /api/v1/music/generate`. koblem ran the full pipeline
/// (LM code-sketch for `generate`, then the turbo DiT) and saved the result.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MusicGenerateResponse {
    pub tracks: Vec<MusicTrack>,
    /// The duration koblem actually requested (auto-computed or echoed).
    pub duration_used: f64,
    /// Lyrics fed to the DiT — useful when the LM wrote them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lyrics_used: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gate_wait_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gpu_log_id: Option<i64>,
}

/// Subset of ace-server `GET /props` koblem surfaces for the UI: available
/// models and the DiT default the dropdown should preselect.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MusicProps {
    /// DiT model filenames available for the `synth_model` dropdown.
    #[serde(default)]
    pub dit_models: Vec<String>,
    /// LM model filenames (informational; koblem pins the prod LM).
    #[serde(default)]
    pub lm_models: Vec<String>,
    /// Whether ace-server is reachable/healthy right now.
    pub healthy: bool,
}

/// Result of `POST /api/v1/music/understand` — what ace-server's reverse
/// pipeline heard in an uploaded song. The `caption` is the headline (drop it
/// straight into a generate/cover caption for "in the style of this song"); the
/// rest is metadata the UI can show or reuse.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MusicUnderstood {
    pub caption: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lyrics: Option<String>,
    /// Beats per minute (0/absent = not detected).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bpm: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub keyscale: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timesignature: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vocal_language: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration: Option<f64>,
}

/// A pair of line endings that only look like a rhyme: same letters, different
/// vowel ("good"/"blood" — UH1 D vs AH1 D). Survives on the page, collapses when
/// sung. Reported so the UI can offer a fix; never applied automatically, since
/// perfect rhyme is not always wanted (rap leans on slant rhyme).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RhymeFlag {
    /// The earlier line's last word.
    pub a: String,
    /// The later line's last word.
    pub b: String,
}

/// Result of `POST /api/v1/music/draft-lyrics` — lyrics written by the main LLM
/// (no render), for previewing/editing before committing to a generate.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MusicDraftLyricsResponse {
    pub lyrics: String,
    /// Line endings that do not rhyme when sung. Empty when clean. Advisory —
    /// POST them back to `/api/v1/music/fix-rhymes` only if the user asks.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub rhyme_flags: Vec<RhymeFlag>,
    /// The LLM's estimate of a good track length for these lyrics, in seconds.
    /// A hint the UI can offer for the (otherwise auto-computed) duration; the
    /// user chooses whether to apply it. None when not estimated or out of range.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub suggested_seconds: Option<f64>,
}

/// `POST /api/v1/music/fix-rhymes` — rewrite only the lines whose endings do not
/// rhyme when sung. User-triggered (the Fix button), never automatic.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MusicFixRhymesRequest {
    /// The lyrics to repair, as drafted or edited.
    pub lyrics: String,
    /// Style context, so the rewrite stays on-brief.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub caption: Option<String>,
}

/// `POST /api/v1/music/suggest` — a casual one-liner idea to expand.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MusicSuggestRequest {
    pub brief: String,
    /// Expand into a caption ONLY (no lyrics) — for the "write me a caption, let
    /// ACE write the lyrics" workflow.
    #[serde(default)]
    pub caption_only: bool,
}

/// What the LLM expands an idea into in one pass: the music caption AND matching
/// lyrics, written from the same context so they cohere. Mirrors
/// [`SonggenSuggestion`], which is the songgen engine's equivalent.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MusicSuggestion {
    pub caption: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lyrics: Option<String>,
}

// ─── Songgen (songgen.cpp / LeVo2 — the heavier, "fancier" music engine) ───────
//
// A second, co-equal music engine alongside acestep (the UI offers both as tabs:
// acestep = lighter/quicker, songgen = LeVo2, longer + clone/continue from an
// audio prompt). songgen-server is an ASYNC job queue (`POST /generate|/clone|
// /continue` → `{job_id}`, poll `GET /job?id=`, `&download=1` for the wav,
// `&cancel=1`/`POST /unload` to preempt). koblem polls and saves under SONGS_DIR
// just like music, and gates it as the same `music` GPU consumer so the two
// engines time-share the one GPU.

/// Which songgen pipeline to run. Serialises snake_case to match the UI tab.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[derive(Default)]
pub enum SonggenMode {
    /// text → song (lyric + description). songgen-generate.
    #[default]
    Generate,
    /// Clone the style of an audio prompt (vocal + bgm stems) into a new song.
    Clone,
    /// Continue/extend an audio prompt — the output is the continuation only
    /// (songgen trims the prompt by design).
    Continue,
}

/// Which stems songgen synthesises (maps to the server's `gen_type`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[derive(Default)]
pub enum SonggenGenType {
    /// Full mix (vocals + backing). The default.
    #[default]
    Mixed,
    /// Vocal stem only.
    Vocal,
    /// Backing/instrumental stem only.
    Bgm,
}

/// Request for `POST /api/v1/songgen/generate` (JSON). For `clone`/`continue`
/// this is the `request` part of a multipart upload that also carries the
/// `vocal_stem` + `bgm_stem` (+ optional `full_mix`) audio files. Unset optionals
/// fall to songgen-server's own defaults.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SonggenGenerateRequest {
    #[serde(default)]
    pub mode: SonggenMode,
    /// Sung lyrics, with `[verse]`/`[chorus]` section tags. songgen SINGS these —
    /// it cannot write them; a blank `lyric` renders instrumental unless
    /// `lyrics_source = llm` has the 9B write them first (see below).
    #[serde(default)]
    pub lyric: String,
    /// Natural-language style/mood/instrument description. Required for generate.
    #[serde(default)]
    pub description: String,
    /// What happens when `lyric` is blank on a `generate` request. `ace` (the
    /// shared-enum default) = leave it blank → instrumental; `llm` = the
    /// always-resident main 9B writes songgen-format lyrics first. Unlike the
    /// acestep `music` engine, songgen's own LeLM never writes lyrics.
    #[serde(default)]
    pub lyrics_source: MusicLyricsSource,
    /// Optional brief for what the lyrics should be *about* (theme/story/mood).
    /// Only used when `lyrics_source = llm` and `lyric` is blank.
    #[serde(default)]
    pub lyrics_prompt: Option<String>,
    /// draft-lyrics only: an existing lyric draft to improve in place. When
    /// present, the 9B revises it under songgen's structured lyric rules instead
    /// of writing fresh from the description/theme. Ignored by generate.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refine_source: Option<String>,
    /// draft-lyrics only: free-form change to apply to `refine_source`. Empty
    /// with a `refine_source` set → a general polish. Ignored otherwise.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refine_instruction: Option<String>,
    /// Target seconds. None → songgen renders to its model-max (~270 s) and stops
    /// on EOS (length is lyric-driven). `clone` hard-caps at 15 s unless set.
    #[serde(default)]
    pub duration: Option<f64>,
    /// Seed; None → server default (1234).
    #[serde(default)]
    pub seed: Option<i64>,
    /// mixed (default) | vocal | bgm.
    #[serde(default)]
    pub gen_type: SonggenGenType,
    /// Sampling temperature. None → server default (1.0; LeVo-match is 0.9).
    #[serde(default)]
    pub temp: Option<f64>,
    /// Top-k. None → server default (250; LeVo-match is 50).
    #[serde(default)]
    pub top_k: Option<u32>,
    /// Classifier-free-guidance scale. None → server default (1.5).
    #[serde(default)]
    pub cfg: Option<f64>,
    /// Output fade-in/out in ms. None → server default (200).
    #[serde(default)]
    pub fade: Option<f64>,
    /// Persist the rendered audio under `SONGS_DIR` and record it in the gallery.
    #[serde(default = "default_true")]
    pub save_song: bool,
}

impl Default for SonggenGenerateRequest {
    fn default() -> Self {
        Self {
            mode: SonggenMode::default(),
            lyric: String::new(),
            description: String::new(),
            lyrics_source: MusicLyricsSource::default(),
            lyrics_prompt: None,
            refine_source: None,
            refine_instruction: None,
            duration: None,
            seed: None,
            gen_type: SonggenGenType::default(),
            temp: None,
            top_k: None,
            cfg: None,
            fade: None,
            save_song: true,
        }
    }
}

/// Response for `POST /api/v1/songgen/{generate,clone,continue}` — koblem drove
/// the async job to completion and saved the wav. Reuses [`MusicTrack`] (same
/// shape; the `url` is `/api/v1/songgen/file/{filename}`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SonggenGenerateResponse {
    pub tracks: Vec<MusicTrack>,
    /// The duration koblem requested (echoed; None when left to model-max+EOS).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_used: Option<f64>,
    /// Lyrics fed to the model — useful when the 9B wrote them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lyrics_used: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gate_wait_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gpu_log_id: Option<i64>,
}

/// `POST /api/v1/songgen/suggest` result — the 9B expanded a one-liner idea into
/// a songgen description (comma tags) and, unless description-only, matching
/// lyrics in songgen's structured format. Pre-fills the form.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SonggenSuggestion {
    pub description: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lyrics: Option<String>,
}

/// `POST /api/v1/songgen/draft-lyrics` result — songgen-format lyrics written by
/// the main LLM (no render), for previewing/editing before a generate.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SonggenDraftLyricsResponse {
    pub lyrics: String,
    /// Line endings that do not rhyme when sung. Empty when clean. Advisory —
    /// POST them back to `/api/v1/songgen/fix-rhymes` only if the user asks.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub rhyme_flags: Vec<RhymeFlag>,
}

/// `POST /api/v1/songgen/fix-rhymes` — rewrite only the lines whose endings do not
/// rhyme when sung. User-triggered (the Fix button), never automatic.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SonggenFixRhymesRequest {
    /// The lyrics to repair, in songgen's single-line format.
    pub lyrics: String,
    /// Style context, so the rewrite stays on-brief.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// `POST /api/v1/songgen/suggest` — a casual one-liner idea to expand.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SonggenSuggestRequest {
    pub brief: String,
    /// true → description only (the caller supplies/omits the lyrics).
    #[serde(default)]
    pub description_only: bool,
}

/// `POST /api/v1/songgen/separate` — the two saved stem files from an htdemucs
/// split. Deterministic, so this is EXACTLY the separation `/clone` runs
/// internally.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SonggenSeparateResponse {
    pub vocal: MusicTrack,
    pub bgm: MusicTrack,
    #[serde(default)]
    pub duration_ms: u64,
    #[serde(default)]
    pub gate_wait_ms: u64,
}

/// Reachability of the songgen engine, for the UI to enable/disable its tab.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SonggenProps {
    /// Whether songgen-server is reachable/healthy right now.
    pub healthy: bool,
    /// songgen-server build string from `/health` (e.g. `b7fdd0e (2026-06-07)`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
}

// ─── Stable Audio 3 (sa3-server — the light audio/SFX engine) ─────────────────

/// `POST /api/v1/sa3/generate` (t2a). Unset optionals fall to the server's own
/// defaults.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Sa3GenerateRequest {
    /// Text description of the audio to generate.
    pub prompt: String,
    /// Output length in seconds (server default 10; SA3 max 380).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seconds: Option<f32>,
    /// Same prompt+seed reproduces under `euler`; `pingpong` is stochastic by design.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seed: Option<i64>,
    /// `pingpong` (default, the rf_denoiser reference) or `euler`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sampler: Option<String>,
    /// Diffusion steps (server default 8 — the SA3 reference).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub steps: Option<u32>,
    /// Weight quant: `q8` (default) or `q6`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Persist to the gallery.
    #[serde(default = "default_true")]
    pub save: bool,
}

impl Default for Sa3GenerateRequest {
    fn default() -> Self {
        Self {
            prompt: String::new(),
            seconds: None,
            seed: None,
            sampler: None,
            steps: None,
            model: None,
            // Matches the `#[serde(default = "default_true")]` wire default; a
            // derived Default would silently disable saving.
            save: true,
        }
    }
}

/// Response for both `POST /api/v1/sa3/generate` and `/edit`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Sa3GenerateResponse {
    pub tracks: Vec<MusicTrack>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seconds_used: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gate_wait_ms: Option<u64>,
}

/// `GET /api/v1/sa3/props`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Sa3Props {
    pub healthy: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
}

// ─── Avatar (LongCat-Video-Avatar) ────────────────────────────────────────────

/// Bit-slice attention block — opt-in DiT-perf knob bundle. Defaults are
/// **off** (dense); the `radius=1 + self_frame=true + bitmap=true` combo is the
/// bit-exact bitmap config (`HANDOFF-DiT-lap31.md` Stage 2). Other shapes
/// (`radius >= 2`, no bitmap) are owner-eyeball-gated quality tradeoffs.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AvatarBsaConfig {
    pub enable: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub radius: Option<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub self_frame: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bookend: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cube_h: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cube_w: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bitmap: Option<bool>,
}

/// How the avatar pipeline gets the driving audio.
///
/// - `Upload` is a multipart file uploaded with the request itself.
/// - `LibraryClipId` references a previously-saved TTS render (see
///   `TtsVoice.library` paths).
/// - `Url` lets the API fetch a remote wav.
/// - `Tts { script, voice, engine }` asks koblem to run TTS first (against the
///   already-resident engine) and use that wav. `script_from_brief` lets the
///   main 9B write the script before TTS runs.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AvatarAudioSource {
    Upload,
    LibraryClipId {
        id: String,
    },
    Url {
        url: String,
    },
    Tts {
        /// Final script. Use this OR `script_from_brief`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        script: Option<String>,
        /// Free-form brief — koblem asks the main 9B to write a short spoken
        /// script (no stage directions / markdown). Used only when `script` is
        /// `None`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        script_from_brief: Option<String>,
        /// Voice id understood by the TTS engine.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        voice: Option<String>,
        /// Forward-compat hook for picking a TTS engine. Currently ignored —
        /// koblem speaks one TTS engine (`tts-breeze`).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        engine: Option<String>,
    },
}

/// How the avatar pipeline gets the reference portrait.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AvatarImageSource {
    Upload,
    LibraryAssetId { id: String },
    Url { url: String },
}

/// `auto | true | false` — the avatar offload knob. Prod ships offload-only
/// (`true`); `Auto` keeps the supervisor's heuristic ("offload for >40f or any
/// chain"); `False` is a perf-bench knob only.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AvatarOffload {
    #[default]
    True,
    False,
    Auto,
}

/// Output orientation. The 480p/720p tiers are quality levels, not fixed
/// shapes — `Landscape` transposes the tier dims (e.g. 480p → 832×480).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AvatarOrientation {
    #[default]
    Portrait,
    Landscape,
}

/// Request body for `POST /api/v1/avatar/generate`. Mirrors the supervisor's
/// `/generate` shape plus the TTS / LLM-script preprocessing knobs.
///
/// **Defaults derived from `PERF.md` lap 22**: `segment_frames=53` (sweet
/// spot), `cont_cond_frames=13`, `steps=8`, `offload=true`. The model quant
/// (`q4_k`) is baked in at server launch, not selectable per-request.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AvatarGenerateRequest {
    pub image_source: AvatarImageSource,
    pub audio_source: AvatarAudioSource,

    /// DiT prompt — default "a person talking".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt: Option<String>,

    /// Quality tier — "480p" | "720p" (or raw "WxH"). Default "480p".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolution: Option<String>,
    /// Output shape. `Landscape` transposes the resolved tier dims.
    #[serde(default)]
    pub orientation: AvatarOrientation,
    /// Overrides `segments` when set — koblem picks the smallest segment count
    /// covering this duration at 25 fps.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_sec: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub segments: Option<u32>,
    /// Default 53 (PERF.md sweet spot).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub segment_frames: Option<u32>,
    /// Default 13 (per-segment overlap for chained renders).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cont_cond_frames: Option<u32>,

    /// DMD steps — default 8.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub steps: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cfg_scale: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seed: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audio_mouth_scale: Option<f64>,
    /// Hz; 0/None = off.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audio_lowpass_hz: Option<u32>,
    /// Default `True` — prod assumes offload (§2 of GO-LIVE-PLAN).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub offload: Option<AvatarOffload>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vae_tiling: Option<bool>,

    /// Opt-in DiT-perf bundle. Default off (dense).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bsa: Option<AvatarBsaConfig>,
    /// Debug env override for the playground (LONGCAT_* / GGML_* / CUDA_*
    /// only; the supervisor enforces this namespace).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub debug_env: Option<std::collections::BTreeMap<String, String>>,

    /// Persist the rendered clip under AVATARS_DIR and link to the gpu_logs row.
    #[serde(default = "default_true")]
    pub save_clip: bool,
}

impl Default for AvatarGenerateRequest {
    fn default() -> Self {
        Self {
            image_source: AvatarImageSource::Upload,
            audio_source: AvatarAudioSource::Upload,
            prompt: None,
            resolution: None,
            orientation: AvatarOrientation::default(),
            duration_sec: None,
            segments: None,
            segment_frames: None,
            cont_cond_frames: None,
            steps: None,
            cfg_scale: None,
            seed: None,
            audio_mouth_scale: None,
            audio_lowpass_hz: None,
            offload: None,
            vae_tiling: None,
            bsa: None,
            debug_env: None,
            save_clip: true,
        }
    }
}

/// One rendered avatar clip — mirrors `MusicTrack`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AvatarClip {
    /// Filename under AVATARS_DIR (and the last path segment of `url`).
    pub filename: String,
    /// koblem-relative URL: `/api/v1/avatar/file/{filename}`.
    pub url: String,
    /// `video/webm`.
    pub content_type: String,
    pub bytes: u64,
    /// Clip wall length in milliseconds, from the muxed audio. Optional —
    /// koblem fills it in from ffprobe when persisting.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
}

/// Response body for `POST /api/v1/avatar/generate`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AvatarGenerateResponse {
    pub clip: AvatarClip,
    pub segments_rendered: u32,
    pub render_ms: u64,
    /// Total wall spent waiting on the GPU gate (drain + light-peer eviction).
    pub gate_wait_ms: u64,
    /// The script TTS actually rendered — useful when the 9B wrote it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub script_used: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gpu_log_id: Option<i64>,
    /// `auto | true | false` actually used (post-resolution of `Auto`).
    pub offload_used: bool,
}

/// `GET /api/v1/avatar/props` — UI bootstrap.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AvatarProps {
    pub healthy: bool,
    /// PERF.md sweet spot — `53` today; surfaced so the UI can label the
    /// slider correctly without hard-coding.
    pub default_segment_frames: u32,
    pub default_cont_cond_frames: u32,
    pub default_steps: u32,
    pub default_resolution: String,
    /// `last_render_sec` from the supervisor's `/health`, when known. Used
    /// to seed a sensible UI progress estimate before the first render.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_render_sec: Option<f64>,
}

/// `POST /api/v1/avatar/script-from-llm` — koblem asks the main 9B to write a
/// short spoken script from a brief. Mirrors music's `draft-lyrics`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AvatarScriptFromLlmRequest {
    pub brief: String,
    /// Target seconds at normal speech rate (~2.5 wps). None → ~10 s default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_sec: Option<f64>,
    /// An existing spoken script to improve in place instead of writing fresh
    /// from the brief. When present, the model revises it under the same script
    /// rules (spoken words only, target duration).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refine_source: Option<String>,
    /// Free-form change to apply to `refine_source`. Empty with a `refine_source`
    /// set → a general polish. Ignored when `refine_source` is absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refine_instruction: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AvatarScriptFromLlmResponse {
    pub script: String,
}

/// One saved character portrait, from `GET /api/v1/avatar/assets`. Lets the
/// kid re-pick a character without re-uploading the same image every run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AvatarAsset {
    pub id: String,
    pub filename: String,
    pub url: String,
    pub created_at: i64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AvatarAssetsResponse {
    pub assets: Vec<AvatarAsset>,
}

// ─── LTX multi-segment video ───────────────────────────────────────────────
//
// The general-purpose video engine (T2V/I2V/AT2V/AI2V): a director script of
// per-segment prompts is rendered as one resident-DiT chain and stitched into a
// single clip. Audio is "all of the above" — a voice track drives per-segment
// lip-sync (sliced on the absolute timeline), a BGM track is muxed under the
// final clip, and a `mixed` upload is split via songgen `/separate` first
// (voice→lip-sync, bgm→mux). koblem owns that orchestration; the ltx-video
// server only ever sees per-segment 16kHz voice wavs + the chain prompts.

/// How the LTX pipeline gets the optional reference portrait (seg-0 i2v). `None`
/// = pure text-to-video.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum LtxImageSource {
    #[default]
    None,
    Upload,
    LibraryAssetId {
        id: String,
    },
    Url {
        url: String,
    },
}

/// How LTX gets driving audio. The voice component drives lip-sync (sliced per segment) and
/// is only ever a clean voice — BGM never reaches the lip-sync driver. For `mixed`/`bgm` the
/// user's original upload is the FINAL audio track (full fidelity/length): `mixed` is split
/// via songgen `/separate` solely to extract the voice for driving, then the original mix is
/// swapped back onto the output; `bgm` is swapped under the clip (no lip-sync). Voice-only
/// keeps the model's lip-synced track as-is. TTS renders a voice track from the per-segment
/// scripts. `None` = silent / model's own generated audio.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum LtxAudioSource {
    #[default]
    None,
    /// One uploaded voice clip (multipart `audio`) → sliced per segment, drives lip-sync.
    VoiceUpload,
    /// One uploaded BGM clip (multipart `bgm`) → becomes the final audio track (no lip-sync).
    BgmUpload,
    /// One uploaded mixed clip (multipart `audio`) → songgen `/separate` extracts the voice
    /// to drive lip-sync; the original mix becomes the final audio track (no separation
    /// artifacts / length cap on the output).
    MixedUpload,
    /// koblem renders a voice track via the TTS engine from the per-segment scripts (joined),
    /// then slices it for lip-sync.
    Tts {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        voice: Option<String>,
    },
}

/// Spatial upscaler ratio for the LTX hires pass on the decoded output.
///
/// - `None` — decode at base resolution only (no hires block).
/// - `X1_5` — gentler ×1.5 upscale (render a higher base then upscale = sharper faces).
/// - `X2` — the staged ×2 prod upscaler.
///
/// ⚠️ On LTX-2.5 this is a BOOLEAN in effect, not a ratio. `_resolve_upscale` in the `ltx25` plugin
/// maps a `hires` / `hires_chain` block onto a single `upscale: true` and the graph then loads its
/// own ×2 latent upsampler — "the upscaler name is also not a choice here", in the plugin's words.
/// [`scale_mult`](Self::scale_mult) is still live and still matters: it is what sizes the base pass
/// so a `dims_are_target` render lands on the resolution the author asked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[allow(non_camel_case_types)] // X1_5 keeps the ×1.5 ratio legible at the call site.
pub enum LtxUpscalerRatio {
    #[serde(rename = "none")]
    None,
    #[serde(rename = "x1.5")]
    X1_5,
    #[serde(rename = "x2")]
    X2,
}

impl LtxUpscalerRatio {
    /// Upscaler file name (sans extension) for this ratio, or `None` when no hires pass should run.
    ///
    /// 🔴 VESTIGIAL ON THE WIRE, and deliberately left naming the LTX-2.3 files rather than
    /// "fixed" to the 2.5 ones. The surviving engine ignores the name entirely — it loads the
    /// upscaler its graph is built around — so the string's only remaining job is to be a non-empty
    /// value in the `hires` block, which is what turns the two-stage arm on at all. Renaming it to
    /// `ltx-2.5-latent-spatial-upscaler-x2-bf16-1.0` would read as a selection that is not
    /// happening, and there is no 2.5 file at all for the ×1.5 rung — the only staged 2.5
    /// upscalers are ×2 spatial and ×2 temporal.
    pub fn upscaler_name(self) -> Option<&'static str> {
        match self {
            LtxUpscalerRatio::None => None,
            LtxUpscalerRatio::X1_5 => Some("ltx-2.3-spatial-upscaler-x1.5-1.0"),
            LtxUpscalerRatio::X2 => Some("ltx-2.3-spatial-upscaler-x2-1.1"),
        }
    }

    /// Numeric spatial ×-factor this ratio applies (`None` = 1.0). Used to reconstruct the
    /// desired OUTPUT resolution (`base × ratio`) so a multi-stage quality profile can size
    /// its cheaper base pass to land on that same target output instead of overshooting it.
    pub fn scale_mult(self) -> f64 {
        match self {
            LtxUpscalerRatio::None => 1.0,
            LtxUpscalerRatio::X1_5 => 1.5,
            LtxUpscalerRatio::X2 => 2.0,
        }
    }
}

/// One beat of the director script.
/// A frame-pinned image within a shot ("Director" keyframe / identity injection). The image is
/// held fixed at `seconds` into the shot; the model interpolates the motion around it. seconds=0
/// is the shot's first frame (i2v start), seconds=shot-length is the last frame, a middle time is
/// a reveal. A shot carrying keyframes renders fresh (not a continuation of the prior shot).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LtxKeyframe {
    /// Bare (or data-URI) base64 of the pinned image. Inline to avoid per-keyframe multipart parts.
    pub image_b64: String,
    /// Time within the shot, in seconds, where the image is pinned (koblem maps to a frame index).
    pub seconds: f64,
}

/// How the engine sizes a TASS reference image before VAE-encoding it.
///
/// The reference is patchified with the same `patchify_proj` as the target and appended on the
/// TOKEN axis, so it can carry its own spatial grid — which is the whole reason a native
/// 1536×1024 character sheet can condition a 768×448 render.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LtxRefResizeMode {
    /// Encode the reference at its OWN resolution. Correct for a 4-panel character sheet:
    /// `match_target` squashes it into the video bucket and throws away the per-panel face
    /// detail the checkpoint relies on.
    #[default]
    NativeResolution,
    /// Resize the reference to the render's target resolution (the close-up-checkpoint shape).
    MatchTarget,
}

/// One character-sheet identity reference attached to a shot ("Best-Face-ID" / TASS overlap
/// conditioning).
///
/// A sheet is a single 1536×1024 image with four panels of ONE character — face close-up, then
/// full body front / side / back. Attaching it makes LTX hold that character's identity in pure
/// **t2v**: no i2v opener, no continuation from a prior shot. The reference latent rides on the
/// target's frame-0 RoPE grid, separated from a real first frame only by a multiplicative rotary
/// phase keyed on [`Self::source_id`].
///
/// ## `source_id` is an identity TAG, not an index
///
/// `source_id = 0` is the target itself (phase 0, an exact no-op). References start at **2** and
/// each distinct character gets its own value. The tag is what the model keys identity off, so it
/// MUST be stable for a given character across every shot in a scene — if character "Mia" is 2 in
/// shot 1 and 3 in shot 4, the mechanism is simply broken. koblem therefore never derives it from
/// array position: the Director allocates it once per character at PROJECT level and persists it
/// (see `DirectorCharacter` in the web client). `None` lets the engine assign 2, 3, 4… in array
/// order, which is only safe for a single-shot render.
///
/// ## Multi-character is supported but NOT trained
///
/// The source-phase tag generalises to 2, 3, 4…, but upstream training was single-subject and the
/// 4-panel sheet depicts one person. Two characters in one shot is the shakiest part of this
/// feature — expect identity bleed between them.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LtxCharacterRef {
    /// Bare (or data-URI) base64 of the sheet. Inline, like [`LtxKeyframe::image_b64`], so a saved
    /// project re-renders without re-uploading and needs no extra multipart part. In a stored
    /// Director document this is an `asset:<hash>` reference, expanded at the request edge.
    pub image_b64: String,
    /// The TASS source-phase tag for this character (>= 2). `None` → the engine assigns 2, 3, 4…
    /// in array order. See the type docs: for a multi-shot render this must be set explicitly.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_id: Option<u32>,
    /// How the engine sizes the sheet. `None` → [`LtxRefResizeMode::NativeResolution`], which is
    /// the correct mode for a character sheet.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resize_mode: Option<LtxRefResizeMode>,
    /// The character's name. EDITOR/LOG ONLY — deliberately never sent to the engine (same rule as
    /// `DirectorBeat::label`). Carried on the wire so the render log and the job's shot list can
    /// say WHICH character resolved to which `source_id`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Identity attributes of this character — skin tone, hair, eyes, facial hair, glasses, face
    /// shape. The model card is explicit that identity is strongly PROMPT-driven and that a bare
    /// prompt gives weak identity, so [`ltx_ref_t2v_prompt`] folds this into the rendered prompt.
    /// Empty/`None` → the prompt is only prefixed, never rewritten.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub identity: Option<String>,
}

/// The prompt prefix the character-sheet checkpoint was trained with. Identity conditioning is
/// gated on it — a `ref_t2v:` render without the prefix falls back to a generic subject.
pub const LTX_REF_T2V_PREFIX: &str = "ref_t2v:";

// ── Runtime LoRAs that change the IDENTITY CONTRACT ───────────────────────────────────────────
//
// 🔴 [2026-07-30] These three used to be DiT variants (`faceid-sheet`, `msr-v2`) or a merged edit
// checkpoint (`lipdub`). They are runtime adapters now, and their folded checkpoints are deleted.
// The reason is purely economic and it inverted: a folded checkpoint existed because applying the
// same LoRA at runtime cost +43.8s and +686 MiB per render, and fold-at-load (`SD_LORA_FOLD` on the
// ltx-video service) reclaims all of it — +0.0s, +0 MiB, measured. So a 16.3 GB checkpoint per
// effect now buys nothing, while an adapter is tunable, stackable and switchable PER SHOT.
//
// They are named here because identity conditioning is a CONTRACT, not an effect: which of these is
// attached decides what a reference image must BE, how it is sized, whether it is rotary-tagged and
// whether the prompt carries a prefix. Getting that wrong is silent — the render succeeds and
// identity just doesn't hold — so [`LtxIdentityRegime`] has to be able to see them. Every OTHER
// adapter is an ordinary effect and is deliberately absent from this list.
//
// Values are file names as the engine's `--lora-model-dir` lists them, extension included: the
// engine matches against its directory listing and rejects a bare stem. They must stay in step with
// `LTX_VIDEO_LORAS` in kobbler's docker-compose.yml, which is what actually offers them.

/// Best-Face-ID character sheets — the adapter behind [`LtxIdentityRegime::Sheet`], and now the
/// only way to reach that regime.
pub const LTX_LORA_FACEID_SHEET: &str = "faceid-sheet-rt.gguf";
/// Best_FaceID_v1.0 close-up identity. Pairs with [`LTX_LORA_FACEID_SHEET`] at ~0.2+ — the sheet
/// handles clothing and body, this strengthens the face. Same [`LtxIdentityRegime::Sheet`] contract.
pub const LTX_LORA_FACEID_CLOSEUP: &str = "faceid-closeup-rt.gguf";
/// Licon Multiple-Subject-Reference V2. Shares Echo's [`LtxIdentityRegime::Memory`] contract exactly
/// (`match_target`, untagged, no `ref_t2v:` prefix) — measured, not assumed: the two were driven
/// with byte-identical requests differing only in the model. It differs from Echo on ONE axis, and
/// it is the axis Echo cannot cover: MSR composes an OBJECT/PROP as a separate entity where Echo
/// fuses multiple heterogeneous subjects into one. That is why a prop-carrying shot attaches this.
pub const LTX_LORA_MSR_V2: &str = "LTX-2.3-Licon-MSR-V2.gguf";
/// Relip. Replaces the deleted merged `edit` DiT (`LTX_DIFFUSION_EDIT`, now unset).
pub const LTX_LORA_LIPDUB: &str = "ltx-2.3-22b-ic-lora-lipdub-0.9.gguf";

/// WHICH identity mechanism this render implements. Both feed the engine the same
/// `character_refs` array — they disagree about what a reference IS and how it is tagged, and
/// getting that wrong is silent: the render still succeeds, identity just doesn't hold.
///
/// 🔴 [2026-07-30] THE REGIME IS NO LONGER A PROPERTY OF THE MODEL NAME ALONE. It used to be, because
/// each mechanism had its own folded checkpoint (`faceid-sheet`, `echo-e50`, `msr-v2`) and the name
/// was a complete description. Those folds are deleted and the mechanisms are runtime adapters now
/// ([`LTX_LORA_FACEID_SHEET`], [`LTX_LORA_MSR_V2`]), so `base` + the MSR adapter is a Memory render
/// while bare `base` is not, and nothing in the string "base" says which. Resolve with
/// [`Self::for_render`], which sees both. [`Self::for_model`] is kept for the callers that genuinely
/// have no LoRA set in scope, and it CANNOT see an adapter-supplied regime — using it where a set
/// exists is the silent-wrong-identity bug this note exists to prevent.
///
/// | | [`Sheet`](Self::Sheet) (`faceid-sheet-rt`) | [`Memory`](Self::Memory) (`echo-e50` / MSR) |
/// |---|---|---|
/// | reference image | 4-panel 1536×1024 character sheet | an ordinary FULL FRAME of the character |
/// | `resize_mode` | `native_resolution` (keep the per-panel detail) | `match_target` (the memory bank encodes slots at render size) |
/// | rotary tag | source-phase tag, `tass_phase_scale` 1.0 | UNTAGGED, `tass_phase_scale` **0** — Echo's native overlap layout |
/// | prompt | `ref_t2v: ` prefix (the checkpoint is gated on it) | no prefix; the checkpoint never saw one |
///
/// Both fold each character's identity clause into the prompt. That is not decoration in either
/// regime: Echo has no per-character token at all, so the byte-identical appearance sentence is
/// the ONLY thing routing a shot to the right anchor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LtxIdentityRegime {
    /// Best-Face-ID / TASS character sheets. The historical koblem behaviour, and therefore the
    /// default for any model whose name we don't recognise — an existing project must keep
    /// compiling to the bytes it always did.
    #[default]
    Sheet,
    /// JoyAI-Echo memory bank (`echo-e50`). References are clean timestep-0 latent slots on the
    /// target's own RoPE grid with NO source tag.
    Memory,
}

impl LtxIdentityRegime {
    /// The regime a DiT variant name implements ON ITS OWN. Anything unrecognised is [`Self::Sheet`]
    /// — see the variant docs for why that, and not "no identity at all", is the safe default.
    ///
    /// ⚠ MODEL-ONLY. Since the identity mechanisms became runtime adapters this can no longer see
    /// the whole picture: `base` + [`LTX_LORA_MSR_V2`] is a Memory render and this returns
    /// [`Self::Sheet`] for it. Prefer [`Self::for_render`] anywhere a LoRA set is available.
    pub fn for_model(model: Option<&str>) -> Self {
        let name = model.unwrap_or("").trim().to_ascii_lowercase();
        // Both surviving checkpoints are Echo doses. `msr` is still matched because a folded MSR
        // checkpoint is a legitimate thing for an operator to re-register in
        // LTX_DIFFUSION_VARIANTS, and it would map here rather than through the adapter path.
        if name.contains("echo") || name.contains("msr") {
            Self::Memory
        } else {
            Self::Sheet
        }
    }

    /// The regime a render actually implements, from its model AND its attached adapters.
    ///
    /// An adapter WINS over the bare model name, because that is the only way it can be true: the
    /// checkpoints that carry a regime in their name are the Echo doses, which are already Memory,
    /// so the reachable disagreement is "a Sheet-named-or-unnamed model with an MSR adapter", and
    /// there the adapter is what supplies the trained response. Attaching MSR to an Echo dose keeps
    /// Memory either way, which is the case koblem's prop routing produces.
    ///
    /// `loras` are file names as [`LtxRuntimeLora::path`] carries them; matching is substring and
    /// case-insensitive so a renamed or re-versioned file (`...MSR-V3.gguf`) still resolves.
    pub fn for_render<'a>(model: Option<&str>, loras: impl IntoIterator<Item = &'a str>) -> Self {
        let mut saw_sheet = false;
        for path in loras {
            let p = path.trim().to_ascii_lowercase();
            if p.contains("msr") {
                return Self::Memory;
            }
            if p.contains("faceid") {
                saw_sheet = true;
            }
        }
        if saw_sheet {
            // An explicit sheet adapter pins Sheet even on an Echo checkpoint. That combination is
            // author-chosen and contradictory (a 4-panel sheet fed to a memory bank), so it is
            // honoured rather than silently reinterpreted; the editor warns about it separately.
            return Self::Sheet;
        }
        Self::for_model(model)
    }

    /// True when this render has something that actually conditions on identity — either an
    /// identity-trained checkpoint or an identity adapter. Attaching references without one is not
    /// an error (the engine's overlap hook is a pipeline feature and applies regardless) but nothing
    /// has a trained response to them, so the UI says so rather than letting identity quietly do
    /// nothing.
    ///
    /// ⚠ Takes the LoRA set for the same reason [`Self::for_render`] does: since `faceid-sheet` and
    /// `msr-v2` stopped being checkpoints, `base` is identity-trained exactly when one of their
    /// adapters is attached, and the model name alone cannot tell you.
    pub fn identity_is_trained<'a>(
        model: Option<&str>,
        loras: impl IntoIterator<Item = &'a str>,
    ) -> bool {
        let name = model.unwrap_or("").trim().to_ascii_lowercase();
        if name.contains("echo") || name.contains("faceid") || name.contains("msr") {
            return true;
        }
        loras.into_iter().any(|p| {
            let p = p.trim().to_ascii_lowercase();
            p.contains("faceid") || p.contains("msr")
        })
    }

    /// How the engine must size a reference in this regime.
    pub fn resize_mode(self) -> LtxRefResizeMode {
        match self {
            Self::Sheet => LtxRefResizeMode::NativeResolution,
            Self::Memory => LtxRefResizeMode::MatchTarget,
        }
    }

    /// The `tass_phase_scale` to send when the request doesn't state one. `None` leaves the engine
    /// default (1.0) in place; `Some(0.0)` is Echo's UNTAGGED layout and must be sent explicitly —
    /// omitting it is not the same thing.
    pub fn default_tass_phase_scale(self) -> Option<f64> {
        match self {
            Self::Sheet => None,
            Self::Memory => Some(0.0),
        }
    }

    /// Whether the rendered prompt carries the [`LTX_REF_T2V_PREFIX`].
    pub fn uses_ref_t2v_prefix(self) -> bool {
        matches!(self, Self::Sheet)
    }

    /// What a reference image should BE in this regime, for the editor to say out loud. The two
    /// answers are not interchangeable and the failure is invisible in a successful render.
    pub fn reference_spec(self) -> &'static str {
        match self {
            Self::Sheet => {
                "One 1536×1024 image, four panels: face close-up, then full body front, side and \
                 back — same person, same clothes in all four."
            }
            Self::Memory => {
                "The subject CUT OUT on a plain white background, evenly lit, UNCROPPED — \
                 nothing touching the frame edge. Three measured failures, all invisible in a \
                 successful render: a crop severs the subject and the missing part is reinvented \
                 differently every shot (cutting the feet out changed the feet); an unmatted \
                 photo can leak its own background and hijack the location; and lighting with no \
                 visible source in frame is read as the subject's own colour and travels with it \
                 (a neon-lit cutout stayed pink on a moonlit beach)."
            }
        }
    }
}

/// Tokens a `native_resolution` character sheet costs: a 1536×1024 sheet patchified on a /32 grid.
/// Fixed, because a sheet has a fixed shape — that is what makes it cheap at high render
/// resolutions and expensive at low ones.
const LTX_SHEET_REF_TOKENS: f64 = (1536 / 32 * (1024 / 32)) as f64;

/// A reference's share of the attention sequence — the number that decides whether identity
/// conditioning holds the FACE or captures the FRAMING.
///
/// GPU-measured on the sheet path, and the arithmetic reproduces both points exactly:
/// 39.5% (768×448, 49 frames) CAPTURED the reference's framing; 11.8% (1280×704, 97 frames) was
/// clean. So the lever is the ratio, not the resolution: a longer shot or a bigger render dilutes
/// the same reference. See [`LTX_REF_SHARE_WARN`] / [`LTX_REF_SHARE_CAPTURE`].
///
/// Video tokens are `latent_frames × (w/32) × (h/32)` with `latent_frames = (frames-1)/8 + 1`; a
/// `match_target` reference costs exactly one latent frame's worth, a sheet a fixed
/// [`LTX_SHEET_REF_TOKENS`].
pub fn ltx_ref_token_share(
    width: u32,
    height: u32,
    frames: u32,
    refs: usize,
    regime: LtxIdentityRegime,
) -> f64 {
    if refs == 0 {
        return 0.0;
    }
    let per_frame = f64::from(width / 32) * f64::from(height / 32);
    if per_frame <= 0.0 {
        return 0.0;
    }
    let latent_frames = f64::from((frames.max(1) - 1) / 8 + 1);
    let per_ref = match regime.resize_mode() {
        LtxRefResizeMode::MatchTarget => per_frame,
        LtxRefResizeMode::NativeResolution => LTX_SHEET_REF_TOKENS,
    };
    let refs_tokens = per_ref * refs as f64;
    refs_tokens / (refs_tokens + per_frame * latent_frames)
}

/// Above this share, references start competing with the shot for the sequence. Between here and
/// [`LTX_REF_SHARE_CAPTURE`] is unmeasured ground, so the editor warns rather than refuses.
pub const LTX_REF_SHARE_WARN: f64 = 0.20;
/// At/above this share the reference CAPTURES the framing — the render reproduces the reference's
/// composition instead of the shot's. Measured at 39.5%; 11.8% was clean.
pub const LTX_REF_SHARE_CAPTURE: f64 = 0.35;

/// The prompt to actually render an identity-conditioned shot with, in the [`LtxIdentityRegime`]
/// the selected DiT implements.
///
/// Two things, in order, and both no-ops when `refs` is empty (so an ordinary shot's prompt is
/// byte-identical):
///
/// 1. Prefix `ref_t2v: ` — **[`LtxIdentityRegime::Sheet`] only** — unless the author already typed
///    it (matched case-insensitively, so a hand-written `Ref_T2V:` is not doubled). The prefix is a
///    trained token of the character-sheet checkpoint; `echo-e50` never saw it, so adding it there
///    is noise in front of every prompt.
/// 2. Append each reference's [`LtxCharacterRef::identity`] clause, once, as a trailing sentence.
///    This happens in BOTH regimes and matters more in [`LtxIdentityRegime::Memory`], where there
///    is no per-character tag at all and the byte-identical appearance sentence is the only thing
///    routing a shot to the right anchor. A clause whose text the prompt already contains is
///    skipped, so re-running this is idempotent.
///
/// Nothing else. This is deliberately the ONLY place the prompt is rewritten, and it is the hook an
/// LLM enrichment step should replace (write a richer `identity` per character; the fold-in stays
/// here).
pub fn ltx_character_prompt(
    prompt: &str,
    refs: &[LtxCharacterRef],
    regime: LtxIdentityRegime,
) -> String {
    if refs.is_empty() {
        return prompt.to_string();
    }
    let trimmed = prompt.trim();
    let mut out = if !regime.uses_ref_t2v_prefix()
        || trimmed
            .get(..LTX_REF_T2V_PREFIX.len())
            .is_some_and(|p| p.eq_ignore_ascii_case(LTX_REF_T2V_PREFIX))
    {
        trimmed.to_string()
    } else {
        format!("{LTX_REF_T2V_PREFIX} {trimmed}")
    };
    for r in refs {
        let Some(identity) = r
            .identity
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        else {
            continue;
        };
        if out.contains(identity) {
            continue;
        }
        // Sentence-join: the clause is appended as its own sentence rather than run on, so the
        // text encoder sees a description and not a mangled tail of the action.
        if !out.ends_with(['.', '!', '?']) {
            out.push('.');
        }
        out.push(' ');
        out.push_str(identity);
        if !out.ends_with(['.', '!', '?']) {
            out.push('.');
        }
    }
    out
}

/// [`ltx_character_prompt`] in the character-sheet regime. Kept as the name every existing caller
/// already uses.
pub fn ltx_ref_t2v_prompt(prompt: &str, refs: &[LtxCharacterRef]) -> String {
    ltx_character_prompt(prompt, refs, LtxIdentityRegime::Sheet)
}

// ── The runtime-LoRA CATALOG (the FALLBACK layer) ─────────────────────────────────────────────
//
// WHERE THIS SITS. Three layers, and this is the last one:
//
//   1. WHICH ADAPTERS EXIST — the engine's `--lora-model-dir`, served live by
//      `GET /sdapi/v1/loras`. Nothing else gets a vote; a name the engine cannot resolve is a hard
//      400 at render time.
//   2. WHAT THEY NEED — `loras.json` in that same (bind-mounted) directory, merged into the same
//      response. Editable without rebuilding anything, which is why it outranks this file.
//   3. THIS CATALOG — the compiled-in default for an adapter the manifest does not describe, and
//      the only layer that still answers when the engine is unreachable at bootstrap.
//
// What layers 2 and 3 supply is the part a file name cannot carry, and both halves fail SILENTLY —
// the render succeeds and the adapter simply under-fires or does nothing:
//
//   * the multiplier the adapter was TRAINED at, which is often not 1.0 and is invisible on the
//     wire (none of these files ship `.alpha` tensors, so the engine applies `multiplier` raw —
//     `omninft` at 1.0 is HALF of what its author trained, and nothing anywhere says so);
//   * the TRIGGER PHRASE some adapters need in the prompt to fire at all.
//
// It is a lookup, NOT a whitelist. `LtxLoraInfo::for_path` answers for any file name — an adapter
// dropped into the directory tomorrow gets the neutral profile (its own file name as the label,
// multiplier 1.0, no trigger) and every surface keeps working. That is the specific failure this
// shape is designed against: a name→metadata table that ROTS the moment someone adds a `.gguf`.
// Nothing here gates, filters or renames — it only annotates.
//
// A stale entry here is inert (it annotates a file nobody has); a MISSING entry is not an error,
// just an un-annotated adapter. When you change a number here, change `loras.json` too — that is
// the copy that takes effect without a rebuild, and it is the one that wins.

/// fal's audio-reactive adapter — motion driven by the audio track.
pub const LTX_LORA_AUDIO_REACTIVE: &str = "ltx2.3-audio-reactive-v2.gguf";
/// OmniNFT LTX-2.3 RL-LoRA — general aesthetic/coherence lift.
pub const LTX_LORA_OMNINFT: &str = "omninft-2.3-diffconv.gguf";
/// Upstream's IC-LoRA detailer. The "19b" is upstream's name, not a geometry mismatch.
pub const LTX_LORA_DETAILER: &str = "ltx-2-19b-ic-lora-detailer.gguf";
/// valiantcat / joyfox transition LoRA — first-to-last-frame morphs and scene transitions.
pub const LTX_LORA_TRANSITION: &str = "ltx2.3-transition.gguf";
/// vrgamedevgirl84 Pixar-style CGI toon look.
pub const LTX_LORA_PIXAR_TOON: &str = "ltx2.3-pixar-toon.gguf";
/// LiconStudio VBVR stage-3 (390K) — physical/causal motion reasoning, no style of its own.
pub const LTX_LORA_VBVR: &str = "ltx2.3-vbvr-i2v-390k.gguf";

/// Everything about a runtime LoRA that its file name does not say.
///
/// `'static` and hand-written on purpose: these are measurements and upstream README claims, and
/// both want a place to record WHERE the number came from. See [`ltx_lora_profile`].
#[derive(Debug, Clone, Copy)]
pub struct LtxLoraProfile {
    /// File name as `LTX_VIDEO_LORAS` and [`LtxRuntimeLora::path`] carry it, extension included.
    pub file: &'static str,
    /// Short human label for a picker row. The file name is still shown next to it — an operator
    /// debugging a 400 needs the string the engine rejected, not a prettified version of it.
    pub label: &'static str,
    /// One line on what it does, for a tooltip / hint row.
    pub blurb: &'static str,
    /// The multiplier this adapter should be ADDED at. Not always 1.0 and not guessable from the
    /// file: see the per-entry notes in [`LTX_LORA_CATALOG`] for where each number comes from.
    pub default_multiplier: f32,
    /// The phrase that has to appear in the prompt for the adapter to fire, if any.
    /// [`ltx_apply_lora_triggers`] appends it; the UI shows it so the author knows it is happening.
    pub trigger: Option<&'static str>,
    /// False when the adapter works without its trigger and the phrase only strengthens it. Purely
    /// informational — an optional trigger is still injected, because an author who ticked the
    /// adapter asked for its effect.
    pub trigger_required: bool,
}

/// The annotated adapters. Anything not in here still works — see the module note above.
pub const LTX_LORA_CATALOG: &[LtxLoraProfile] = &[
    LtxLoraProfile {
        file: LTX_LORA_AUDIO_REACTIVE,
        label: "Audio-reactive",
        blurb: "Motion driven by the audio track (fal's adapter).",
        // 1.5 is the trained scale, and it is also what every folded `audioreactive` checkpoint
        // this repo ever built baked in. ⚠ RESULTS-folded-ar15-reclaims-the-62s.md flags that the
        // number was inherited from the inert-fold era and has never been A/B'd against 1.0.
        default_multiplier: 1.5,
        trigger: None,
        trigger_required: false,
    },
    LtxLoraProfile {
        file: LTX_LORA_OMNINFT,
        label: "OmniNFT aesthetic",
        blurb: "General coherence/aesthetic lift (LTX-2.3 RL-LoRA).",
        // 🔴 NOT 1.0. This is a PEFT adapter with no `.alpha` tensors, so its trained scale
        // (lora_alpha/r = 64/32) survives only in the upstream adapter_config.json. The engine
        // multiplies B@A raw, so 1.0 here renders HALF the adapter and looks like a weak LoRA
        // rather than a misconfigured one.
        default_multiplier: 2.0,
        trigger: None,
        trigger_required: false,
    },
    LtxLoraProfile {
        file: LTX_LORA_DETAILER,
        label: "Detailer",
        blurb: "Upstream IC-LoRA detail pass — finer texture and micro-contrast.",
        // Upstream states none; 1.0 is the neutral scale, not a measurement.
        default_multiplier: 1.0,
        trigger: None,
        trigger_required: false,
    },
    LtxLoraProfile {
        file: LTX_LORA_FACEID_SHEET,
        label: "Face-ID sheet",
        blurb: "4-panel character sheets. Sets the Sheet identity regime.",
        // The scale every faceid-sheet fold used (HANDOFF-faceid-character-sheets.md).
        default_multiplier: 1.0,
        trigger: None,
        trigger_required: false,
    },
    LtxLoraProfile {
        file: LTX_LORA_FACEID_CLOSEUP,
        label: "Face-ID close-up",
        blurb: "Strengthens facial identity. Add ALONGSIDE the sheet, not instead of it.",
        // 0.2 per Best_FaceID_v1.0's own README: the sheet carries clothing and body, this rides
        // on top when identity is weak. Same number the 0.2 fold used.
        default_multiplier: 0.2,
        trigger: None,
        trigger_required: false,
    },
    LtxLoraProfile {
        file: LTX_LORA_MSR_V2,
        label: "MSR multi-subject",
        blurb: "Characters, locations AND props as separate entities. Memory identity regime.",
        // Licon publishes `strength_model = 1` and V2 carries no rescale metadata.
        default_multiplier: 1.0,
        trigger: None,
        trigger_required: false,
    },
    LtxLoraProfile {
        file: LTX_LORA_LIPDUB,
        label: "Relip (lipdub)",
        blurb: "Re-times mouth articulation to a new audio track.",
        // What run_ltx_relip_job has always sent.
        default_multiplier: 1.0,
        trigger: None,
        trigger_required: false,
    },
    LtxLoraProfile {
        file: LTX_LORA_TRANSITION,
        label: "Transition / morph",
        blurb: "Smooth first-to-last-frame morphs, identity swaps and scene transitions.",
        // The author's README states 1.0 outright, alongside CFG 4.0 and embedded guidance 1.0.
        default_multiplier: 1.0,
        // "Add `zhuanchang` when you want the LoRA behavior to activate more strongly", placed
        // near the END of the prompt so the description still leads.
        trigger: Some("zhuanchang"),
        trigger_required: true,
    },
    LtxLoraProfile {
        file: LTX_LORA_PIXAR_TOON,
        label: "Pixar toon",
        blurb: "Stylised CGI animated-feature look: smooth shading, expressive faces.",
        // No stated strength upstream; 1.0 is the neutral scale. Its per-module delta measures
        // ~0.0011 (median ‖A‖‖B‖/√(in·out)) — the WEAKEST of the three 2026-07-30 additions and
        // roughly half the transition LoRA's, so if it under-styles, raising it is the first move.
        default_multiplier: 1.0,
        // "This LoRA should still work without this trigger word" — hence not required.
        trigger: Some("P1x4r"),
        trigger_required: false,
    },
    LtxLoraProfile {
        file: LTX_LORA_VBVR,
        label: "VBVR motion reasoning",
        blurb: "Physically plausible motion, trajectories and multi-object interaction. No style.",
        // No stated strength upstream (LiconStudio publish training config only). 1.0 is the
        // neutral scale and is NOT a measurement. ⚠ Its delta is the LARGEST of the three
        // additions (~0.005 vs the transition LoRA's ~0.0025 on the same metric), so this is the
        // one most likely to want backing off — try 0.6–0.8 before blaming the base model.
        default_multiplier: 1.0,
        trigger: None,
        trigger_required: false,
    },
];

/// The catalog entry for an adapter file name, if it has one.
///
/// Matching is on the file name, case-insensitively, and ALSO on the stem so the same profile
/// answers for `foo.gguf` and the `foo.safetensors` it was converted from — the engine prefers the
/// gguf when both are present, and an operator who lists the other one should not silently lose the
/// adapter's trained multiplier.
pub fn ltx_lora_profile(path: &str) -> Option<&'static LtxLoraProfile> {
    fn stem(s: &str) -> &str {
        s.rsplit_once('.').map_or(s, |(base, _)| base)
    }
    let want = path.trim();
    LTX_LORA_CATALOG
        .iter()
        .find(|p| p.file.eq_ignore_ascii_case(want))
        .or_else(|| {
            let want = stem(want);
            LTX_LORA_CATALOG
                .iter()
                .find(|p| stem(p.file).eq_ignore_ascii_case(want))
        })
}

/// The multiplier to ADD an adapter at. 1.0 for anything uncatalogued — the neutral scale, and the
/// only defensible answer when nothing is known about the file.
pub fn ltx_lora_default_multiplier(path: &str) -> f32 {
    ltx_lora_profile(path).map_or(1.0, |p| p.default_multiplier)
}

/// Fold every trigger phrase the attached adapters need into `prompt`.
///
/// WHY THE SERVER DOES THIS AND NOT THE AUTHOR
///
/// A trigger is a property of the ADAPTER, not of the shot, and forgetting one fails silently: the
/// render succeeds, costs its full minutes, and comes back looking like the LoRA did nothing.
/// Ticking the adapter is the author saying they want its effect, so the phrase rides along with
/// it — for exactly the shots whose EFFECTIVE set contains it, which is the per-segment set when
/// the shot has one and the project set otherwise (see `effective_lora_paths`).
///
/// APPEND ONLY. Both trigger-carrying adapters ask for it — valiantcat's README is explicit
/// ("place the trigger word near the end of the prompt so the base prompt still clearly describes
/// subject, scene, camera movement…") — and it is also the only placement that composes with the
/// `ref_t2v: ` prefix, which must stay the first token of a Sheet-regime prompt. So there is no
/// placement enum: adding one would be speculative, and the wrong choice is invisible in a render
/// that succeeds.
///
/// IDEMPOTENT. A prompt that already contains the phrase is left alone, so an author who typed
/// `zhuanchang` themselves does not get it twice, and re-compiling a plan is a no-op. Matching is
/// case-insensitive substring — same rule [`ltx_character_prompt`] uses for identity clauses, and
/// the false-positive (a prompt that happens to contain "P1x4r" as part of a longer word) is a
/// no-op rather than a wrong render.
pub fn ltx_apply_lora_triggers<'a>(
    prompt: &str,
    loras: impl IntoIterator<Item = &'a str>,
) -> String {
    let lower = prompt.to_ascii_lowercase();
    let mut pending: Vec<&'static str> = Vec::new();
    for path in loras {
        let Some(trigger) = ltx_lora_profile(path).and_then(|p| p.trigger) else {
            continue;
        };
        if lower.contains(&trigger.to_ascii_lowercase())
            || pending.iter().any(|a| a.eq_ignore_ascii_case(trigger))
        {
            continue;
        }
        pending.push(trigger);
    }
    // Byte-for-byte pass-through when there is nothing to add — which is the overwhelmingly common
    // case, and a prompt that came back merely re-trimmed would churn every stored plan.
    if pending.is_empty() {
        return prompt.to_string();
    }

    let mut out = prompt.trim_end().to_string();
    for trigger in pending {
        // Sentence-join, matching `ltx_character_prompt`: a trigger is a bare token, so it is
        // separated from a finished sentence by a space and from an unfinished one by a comma —
        // which is exactly how the upstream example prompts are written.
        if out.is_empty() {
            // An empty prompt with a trigger attached is degenerate but reachable; the trigger
            // alone beats a leading ", ".
        } else if out.ends_with(['.', '!', '?', ',']) {
            out.push(' ');
        } else {
            out.push_str(", ");
        }
        out.push_str(trigger);
    }
    out
}

/// One adapter as the UI needs it: the wire name plus whatever the catalog knows.
///
/// Owned rather than a `&'static LtxLoraProfile` because [`Self::for_path`] must answer for a file
/// the catalog has never heard of — see the catalog module note. An uncatalogued adapter comes back
/// labelled with its own file name at multiplier 1.0 and no trigger, which is precisely how the
/// picker behaved before this type existed.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LtxLoraInfo {
    /// File name as the engine's LoRA directory lists it, extension included. THE WIRE VALUE —
    /// this is what goes into [`LtxRuntimeLora::path`].
    pub path: String,
    /// Display label. The file name itself when uncatalogued.
    pub label: String,
    /// One-line description, empty when uncatalogued.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub blurb: String,
    /// What strength to add it at.
    pub default_multiplier: f32,
    /// Phrase auto-appended to the prompts of shots this adapter is on, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trigger: Option<String>,
    /// Whether the adapter does nothing at all without its trigger.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub trigger_required: bool,
    /// Whether this adapter is for the engine that listed it.
    ///
    /// Only ever false on LTX-2.5: `h3` and `ltx25` are two plugins on ONE ComfyUI, so they share
    /// one `models/loras/` directory and 2.5's listing includes H3's adapters. The C++ engine
    /// scans its own `--lora-model-dir` and has no co-tenant, which is why the default is `true`
    /// — an engine that does not send the field is one where the question cannot arise.
    ///
    /// 🔴 Load-bearing, not advisory. A foreign adapter is not rejected by ComfyUI: its keys miss
    /// the LTX DiT entirely, so it applies to nothing and the render proceeds at full price
    /// against the unmodified model. Filtered out of the picker for that reason.
    #[serde(default = "default_true")]
    pub for_this_model: bool,
}

impl LtxLoraInfo {
    /// Annotate one adapter file name. Never fails — an unknown file gets the neutral profile.
    pub fn for_path(path: &str) -> Self {
        let path = path.trim();
        match ltx_lora_profile(path) {
            Some(p) => Self {
                path: path.to_string(),
                label: p.label.to_string(),
                blurb: p.blurb.to_string(),
                default_multiplier: p.default_multiplier,
                trigger: p.trigger.map(str::to_string),
                trigger_required: p.trigger_required,
                // This path answers for a name the OPERATOR configured, never for one an engine
                // listed, so there is no co-tenant question to answer — see the field's note.
                for_this_model: true,
            },
            None => Self {
                path: path.to_string(),
                label: path.to_string(),
                blurb: String::new(),
                default_multiplier: 1.0,
                trigger: None,
                trigger_required: false,
                for_this_model: true,
            },
        }
    }
}

/// One member of the project's cast as the PROMPT WRITER needs to see them: a stable name to call
/// them by and the appearance sentence that must come back character-for-character identical.
///
/// This is the prompt-side half of identity, and it is not optional in either regime. Upstream's
/// own convention (`ID_A`, `ID_B`, one fixed base-identity sentence repeated verbatim in every
/// shot, expression varied in a SEPARATE sentence) exists because the model routes a shot to an
/// anchor through that sentence — a paraphrase is a different character as far as it is concerned.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LtxCastMember {
    /// What the prompt calls this character — the project's display name ("Mia"). Stable for the
    /// life of the project; the writer must use it and not invent synonyms ("the woman", "she"
    /// is fine mid-sentence, a second name is not).
    #[serde(default)]
    pub name: String,
    /// The appearance sentence, to be reproduced VERBATIM in every shot this character appears in.
    #[serde(default)]
    pub appearance: String,
    /// The character's identity tag, when it has one. Editor/log context for the writer only —
    /// the tag is a rotary phase, never text the model reads.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_id: Option<u32>,
}

impl LtxCastMember {
    /// True when there is nothing for the writer to hold on to.
    pub fn is_empty(&self) -> bool {
        self.name.trim().is_empty() && self.appearance.trim().is_empty()
    }
}

/// One Prompt Relay beat inside a shot (arXiv 2604.10030). The shot's own `prompt` stays the
/// SETTING and conditions the whole shot; a beat is a short clause describing what changes at
/// that moment, given cross-attention priority around it. A shot with zero beats renders exactly
/// as before — the engine keeps its null-mask fast path.
///
/// Copy rule, enforced in the UI and in the LLM director prompt: never repeat the setting in a
/// beat. The setting lives in the shot prompt precisely so it anchors every frame with no
/// penalty, which is the paper's own mitigation for appearance drift between events.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LtxBeat {
    /// Time of the beat in seconds on the shot's VISIBLE clip timeline — i.e. what the viewer
    /// sees, with the continuation frames the engine trims already excluded. koblem shifts this
    /// onto the render timeline by that shot's seam drop.
    pub seconds: f64,
    /// The beat clause. Short: one action, not a whole prompt.
    pub text: String,
    /// Penalty multiplier for this beat. `None` → 1.0 (the paper).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub strength: Option<f64>,
    /// Flat-top half-width in seconds — the span around `seconds` that carries no penalty at all.
    /// `None` → the engine default, which is the beat's own half-span (CAPPED at 1.0s) minus two
    /// latent frames. Leave it unset: the cap is what makes a beat local enough to bite at all, and
    /// pinning a wide window here reproduces the bug where the mask was flat across most of a shot.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window: Option<f64>,
}

/// Per-shot generation mode. `None` (the default) = INFER t2v/i2v from the shot's `image` /
/// `keyframes` exactly as the pre-v2v director did (byte-identical). An explicit `v2v_*` mode
/// routes the shot through the engine's video-to-video path, taking its source from
/// [`LtxSegment::v2v_source`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LtxSegmentMode {
    /// Infer from `image`/`keyframes` (legacy behaviour).
    Auto,
    /// Text → video (force; ignore any attached image).
    T2v,
    /// Image → video scene start (equivalent to an `image` scene-cut).
    I2v,
    /// SDEdit restyle — engine `v2v_mode = 1`. Re-render the source under a new prompt; strength
    /// 1.0 = ignore the source, lower = keep more of it. A *restyle*, not a structure-preserving
    /// edit (the source washes out over a deep denoise).
    V2vRestyle,
    /// Guide-edit — engine `v2v_mode = 2`. Hold the source scene as a partial-strength guide and
    /// add the prompt's change ("keep the scene, add an explosion"). Strength runs the SAME
    /// direction as [`Self::V2vRestyle`] — the engine takes both through one `t_enc = steps *
    /// strength` schedule truncation and mode 2 only sources the value from `v2v_guide_strength`
    /// instead of `strength`. So 1.0 = redo the whole schedule (furthest from the source), lower =
    /// keep more of it; ~0.45 adds an element while holding the scene. This is the Director-2
    /// "edit a video" mechanism.
    V2vEdit,
}

impl LtxSegmentMode {
    /// The engine `v2v_mode` integer this maps to, or `None` for a non-v2v shot.
    pub fn engine_v2v_mode(self) -> Option<u32> {
        match self {
            LtxSegmentMode::V2vRestyle => Some(1),
            LtxSegmentMode::V2vEdit => Some(2),
            _ => None,
        }
    }
    /// True for the restyle / edit modes (renders from a source clip).
    pub fn is_v2v(self) -> bool {
        self.engine_v2v_mode().is_some()
    }
}

/// Where a `v2v_*` shot's source clip comes from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum LtxV2vSource {
    /// A foreign / uploaded clip. Its bytes ride as multipart `seg_v2v_<raw i>` (the /generate
    /// path) or inline base64 in the director timeline's `assets.seg_v2v_b64` (the /run path);
    /// koblem decodes it to per-frame PNGs and sends them as the engine's `control_frames` (the
    /// pixel-encode source).
    Upload,
    /// A clip WE previously rendered: a prior LTX render's `job_id` + the FILTERED (rendered)
    /// segment index within it (matching the banked `seg_<segment>.bin`). koblem maps `job_id` to
    /// the ltx-video engine's own job id and emits it by reference (`v2v_source_job_id` +
    /// `v2v_source_segment`); the engine resolves the banked diffusion latent against its artifact
    /// roots (persist store first, then FIFO). No filesystem path crosses the wire and koblem needs
    /// no fs access. A Director render's latents live in the engine's non-swept persist store, so a
    /// saved shot stays retake-able for the life of the project.
    PriorRender { job_id: String, segment: u32 },
}

/// Drop the leading frame(s) a TASS character reference contaminates on a bare t2v shot.
///
/// A reference sits at the target's latent-frame-0 rotary address, and on some checkpoints the
/// decoder hands that address straight back: pixel frame 0 renders the reference VERBATIM. With a
/// location plate in the set, frame 0 is the empty plate (which reads as "the subject never
/// appears"); with only a character sheet, it is that sheet's own source photo.
///
/// Whether it happens is a property of the CHECKPOINT, which is why this is a setting and not
/// engine behaviour: `echo-e50` leaks, `msr-v2` and `echo-full` do not.
///
/// The engine SELF-GATES the trim to a no-op on any shot that already pins frame 0 — an i2v scene
/// image, a keyframe at frame 0, or a continuation shot. That covers the SHOT, but NOT the
/// checkpoint: the engine sees a loaded DiT, not a variant name, so it cannot tell `echo-e50` from
/// `msr-v2`.
///
/// So `auto` is resolved BY KOBLEM, against the resolved per-shot variant, and only ever reaches
/// the engine as `-1` for a checkpoint that actually leaks — see
/// `ltx_model_leaks_reference_into_frame_zero` in `api/src/routes/ltx_video.rs`, which mirrors
/// `ltxModelLeaksReferenceIntoFrameZero` in `web/src/lib/director/characters.ts`. Passing `-1`
/// straight through would cut a good frame, and 1/fps of audio, off every bare t2v shot carrying
/// references on every model that does not leak.
///
/// With that resolution in place `auto` is safe to leave on for a whole project: only a bare t2v
/// shot that carries references AND runs on a leaking checkpoint is touched, and it comes back one
/// frame shorter rather than being re-rendered.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum LtxFrameDrop {
    /// `"auto"` or `"off"`.
    Mode(LtxFrameDropMode),
    /// An explicit number of leading PIXEL frames to drop. Almost never needed — the engine
    /// derives the right count from the references it encoded, and a hand-set value that is too
    /// small leaves the artifact partly visible.
    Frames(u32),
}

/// The named halves of [`LtxFrameDrop`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LtxFrameDropMode {
    /// Let the ENGINE decide how many frames the references contaminated. The default.
    #[default]
    Auto,
    /// Never trim. What a checkpoint that does not leak wants.
    Off,
}

impl Default for LtxFrameDrop {
    fn default() -> Self {
        Self::Mode(LtxFrameDropMode::Auto)
    }
}

impl LtxFrameDrop {
    /// The engine's `reference_head_trim` value: `-1` = auto (the engine derives the count),
    /// `0` = off, `n > 0` = exactly that many pixel frames.
    ///
    /// Clamped to the engine's own ceiling so a nonsense value is a no-op rather than a 400.
    #[must_use]
    pub fn engine_reference_head_trim(self) -> i32 {
        match self {
            Self::Mode(LtxFrameDropMode::Auto) => -1,
            Self::Mode(LtxFrameDropMode::Off) => 0,
            Self::Frames(n) => i32::try_from(n.min(512)).unwrap_or(0),
        }
    }
}

/// What a shot's own audio clip is FOR. Mirrors the chain-level [`LtxAudioSource`] split:
/// a voice clip both drives lip-sync and is delivered; a bgm clip is delivered only.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LtxShotAudioKind {
    /// Drives this shot's lip-sync AND is muxed as its deliverable audio.
    #[default]
    Voice,
    /// Muxed as this shot's deliverable audio only — no lip-sync conditioning.
    Bgm,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LtxSegment {
    pub prompt: String,
    /// Optional DiT variant for this shot. `None` inherits the request's top-level
    /// [`LtxVideoGenerateRequest::model`]. The engine switches variants only at a chain
    /// segment boundary, retaining CPU/mmap sources while releasing the inactive GPU DiT.
    ///
    /// Only ever an AUTHOR'S PIN. The automatic routing used to set this — a prop-carrying shot was
    /// moved onto the `msr-v2` checkpoint — and since MSR became a runtime adapter that routing does
    /// not write to the wire at all: it ADVISES, and the author attaches MSR through `loras` if they
    /// want it. Echo weights + MSR adapter has never been rendered, so opting every prop shot into it
    /// silently is not a default (see `resolveClipVariants` in koblem's web/director).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Optional runtime LoRAs for THIS shot only, each at its own strength. Empty inherits the
    /// request's top-level [`LtxVideoGenerateRequest::loras`]; a non-empty list REPLACES them
    /// for this shot (it is not additive, so a shot can also drop an inherited adapter by
    /// naming a different set).
    ///
    /// Like `model`, this can only change at a chain segment boundary. Unlike `model` — which swaps
    /// a whole DiT — this is the cheap axis, so per-shot LoRA is the natural way to vary an effect's
    /// STRENGTH across a timeline rather than its base model. It is also the ONLY axis for the
    /// identity mechanisms that used to be checkpoints: see [`LTX_LORA_MSR_V2`].
    ///
    /// ⚠ Not free per shot, though. `SD_LORA_FOLD` on the engine merges adapters into the resident
    /// weights, which is what makes an attached LoRA cost base speed and base VRAM — but the fold is
    /// paid once per lora-SET CHANGE (~12-30 s of GPU work depending on module count). A timeline
    /// that alternates sets shot to shot re-folds at every change, so group shots that share a set.
    ///
    /// Requires an engine that parses a per-segment `lora` key (`routes_ltx.cpp`, with
    /// `refresh_lora_cache` running BEFORE the per-segment names resolve — it did not, and every
    /// adapter 400'd on a cold worker until `3e8a3a1`). An engine without it parses `lora` only at
    /// the request root and silently renders every shot with the top-level set.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub loras: Vec<LtxRuntimeLora>,
    /// Per-segment length in seconds (UI-facing; koblem maps to frames via fps). None →
    /// the engine default segment length.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seconds: Option<f64>,
    /// Optional per-shot scene image (i2v). When set, this shot starts a NEW scene from
    /// this image instead of continuing the previous shot's motion — the audio timeline
    /// stays continuous across the cut. `None`/`LtxImageSource::None` → a continuation
    /// shot. Shot 0's image, if set, is the opening frame (overrides the top-level
    /// `image_source`); for later shots it is a mid-chain scene change.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image: Option<LtxImageSource>,
    /// Optional frame-pinned images within this shot (identity injection / reveals / first+last
    /// frame). A shot with keyframes renders fresh (i2v keyframe path). Mutually exclusive with a
    /// plain `image` scene-cut for the same shot — keyframes win.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub keyframes: Option<Vec<LtxKeyframe>>,
    /// Generation mode for this shot. `None` → inferred from `image`/`keyframes` (legacy t2v/i2v).
    /// A `v2v_*` mode renders the shot from [`Self::v2v_source`] (video-to-video).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<LtxSegmentMode>,
    /// Per-segment v2v guide/edit strength (0..1). Greenlit as PER-SEGMENT: koblem emits it on
    /// each v2v shot, and also sets the chain-global `v2v_guide_strength` (the value today's
    /// engine actually applies) from the first v2v shot — per-segment application is a small
    /// forward-compatible engine follow-up (a `segment_v2v_guide_strength[]` array; see the
    /// handoff). Higher = more of the schedule redone = further from the source, for both v2v
    /// modes; `v2v_edit` defaults to 0.45.
    /// `v2v_restyle`: 1.0 = full re-render, lower = keep more source. `None` → engine default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub v2v_strength: Option<f64>,
    /// Source clip for a `v2v_*` shot. Ignored for non-v2v shots.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub v2v_source: Option<LtxV2vSource>,
    /// Which TAKE this shot is RESTORED from, as the engine job id whose bank holds it.
    ///
    /// Only meaningful on a resume/retake, and only for shots the render does not re-render —
    /// a shot being rolled fresh has no take to restore. `None` means "the bank named by the
    /// chain's `resume_job_id`", which is the historical behaviour.
    ///
    /// WHY IT EXISTS: `resume_job_id` names ONE bank for the whole chain, so every restored shot
    /// came from whichever take was rolled LAST rather than the one the user selected — "pick a
    /// take, render again, it forgot the one I picked". Sending each shot's own selected take
    /// lets a render restore a MIXTURE (shot 0 from take B, shot 1 from take C) that no single
    /// bank holds.
    ///
    /// The engine 400s on an id it cannot resolve rather than falling back to the chain bank:
    /// falling back would render a different take from the one the UI shows and still report
    /// success.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bank_job_id: Option<String>,
    /// Force a FRESH scene from this shot's PROMPT ALONE — a hard cut with no init image and no
    /// continuation from the prior shot. `None`/`false` → default (shot 0 and image/keyframe/v2v
    /// shots are already fresh; every other shot continues). Ignored when the shot has an
    /// `image`/`keyframes`/`v2v_source`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scene_cut: Option<bool>,
    /// Per-shot image-pin hold strength for THIS shot's keyframes / scene image (0..1). 1.0 = the
    /// pinned image is held exactly at its frame (crispest); lower lets the frame flex toward the
    /// generated motion (softer blend). Greenlit as PER-SHOT, mirroring [`Self::v2v_strength`]:
    /// koblem emits it per-segment (forward-compatible) AND derives the chain-global conditioning
    /// `strength` — the value today's engine applies to every pin — from the FIRST shot that sets
    /// it. `None` → the render's top-level [`LtxVideoGenerateRequest::pin_strength`], else koblem's
    /// default. Only meaningful for a shot that actually pins an image (keyframes / scene image).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pin_strength: Option<f64>,
    /// What this shot's own audio clip is for. Only meaningful alongside a clip (multipart
    /// `seg_audio_<raw i>` or [`Self::audio_b64`]). `None` → [`LtxShotAudioKind::Voice`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audio_kind: Option<LtxShotAudioKind>,
    /// This shot's own audio, inline base64 — the persisted form, so a saved project re-renders
    /// without re-uploading. A multipart `seg_audio_<raw i>` part for the same shot wins.
    ///
    /// The clip starts at ITS OWN shot and is cut at the shot boundary, so it can never bleed
    /// into the next shot; shorter than the shot, it is padded with silence. Per-shot clips are
    /// composited OVER the chain-level track ([`LtxVideoGenerateRequest::audio_source`]), which
    /// therefore still works as a bed each shot replaces its own span of (music + per-shot VO).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audio_b64: Option<String>,
    /// Prompt Relay beats for this shot. `None`/empty → one prompt for the whole shot, which is
    /// today's behaviour byte-for-byte. Two or three beats is the practical ceiling for a 4–5s
    /// shot: the decay is a fixed ~2 latent frames per side, so shorter cells stop separating.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub beats: Option<Vec<LtxBeat>>,
    /// Per-shot seed. `None` → the chain seed (offset by shot index, as the engine has always
    /// done). Set on a retake so ONE shot rerolls without disturbing the others.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seed: Option<i64>,
    /// Per-shot base step count. `None` → the render's step count. Raising it for a relay shot is
    /// the fallback if the distilled base pass commits its layout too early to respond to the mask.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub steps: Option<u32>,
    /// Per-shot text CFG. `None` → the render's cfg. Only the `dev` variant responds to > 1.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cfg: Option<f64>,
    /// Per-shot negative prompt. `None`/empty → the render's negative prompt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub negative_prompt: Option<String>,
    /// Character sheets conditioning THIS shot's identity (TASS overlap references). `None`/empty
    /// → today's behaviour byte-for-byte: the engine sees no `character_refs` and keeps its
    /// untagged path (`source_id = 0` ⇒ phase 0 ⇒ an exact no-op).
    ///
    /// A sheet does NOT make the shot fresh — it is not an i2v opener and does not break the
    /// continuation chain. That is the point: identity holds through cuts AND through continuation
    /// shots without pinning a frame. koblem prefixes the rendered prompt with `ref_t2v: ` (see
    /// [`ltx_ref_t2v_prompt`]) whenever this is non-empty.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub character_refs: Option<Vec<LtxCharacterRef>>,
    /// Drop the reference-contaminated leading frame(s) of THIS shot. `None` inherits the
    /// request's top-level [`LtxVideoGenerateRequest::frame_drop`]; a value here is authoritative
    /// for this shot, [`LtxFrameDropMode::Off`] included — which is the point, since it is the only
    /// way to switch the trim off for one shot of a project that has it on.
    ///
    /// Only ever does anything on a bare t2v shot carrying references: the engine self-gates it on
    /// i2v / keyframe-at-0 / continuation shots, which were measured not to leak.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frame_drop: Option<LtxFrameDrop>,
}

/// Named LTX quality profile. Each profile materialises the shared engine request
/// contract (base sampler/sigmas/steps/cfg, negative prompt, and the multi-stage
/// `hires_chain`). `Standard` is the default and is byte-identical to the historical
/// single-hires ver3 request. See HANDOFF-ltx-quality-profiles.md.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LtxQualityProfile {
    /// ver3 distilled: 6-step LCM base + a single 2-step Euler hires. Default — no
    /// negative prompt, cfg 1, no `hires_chain` (the exact historical request). The hires
    /// CONTINUES the base trajectory from σ0=0.725 (where the base ends), NOT an SDEdit
    /// renoise — a different regime from High/4x's σ0=0.85 (see the "LTX-2.3 SIGMA SCHEDULES"
    /// banner in koblem's `routes/ltx_video.rs`).
    #[default]
    Standard,
    /// 8-step `euler_ancestral_cfg_pp` base (runs to 0.0) + one ×2 `euler_cfg_pp` refine stage
    /// that SDEdit-RENOISES to σ0=0.85 + the default quality negative prompt. This is the
    /// community "make videos" upscale recipe — do NOT confuse σ0=0.85 with the ver3 continue
    /// @0.725 or the relip-only @0.909375 (banner in koblem's `routes/ltx_video.rs`).
    High,
    /// 8-step base + TWO ×2 `euler_cfg_pp` refine stages (each SDEdit-renoise @σ0=0.85) + the
    /// default quality negative prompt (replicates the catbox "4x Upscale" workflow).
    #[serde(rename = "upscale_4x")]
    Upscale4x,
    /// Everything editable: base {sigmas, sample_method, steps, cfg}, negative prompt,
    /// and a repeatable list of upscale stages (`hires_chain`).
    Custom,
}

/// One upsample + SDEdit-refine stage in an [`LtxVideoGenerateRequest::hires_chain`].
/// Every field is optional so the Custom UI can dial any subset; koblem fills the
/// contract defaults (×2 upscaler, `euler_cfg_pp`, cfg 1.0, 3 steps, sigmas
/// `[0.85, 0.725, 0.421875, 0.0]`) for anything left unset.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LtxHiresStage {
    /// Spatial ×2 upscaler name. `None` → koblem's default ×2 upscaler.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub upscaler: Option<String>,
    /// SDEdit sigma schedule: re-noise to `custom_sigmas[0]`, then sample down.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub custom_sigmas: Vec<f32>,
    /// Refine sampler for this stage. `None` → `euler_cfg_pp`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sample_method: Option<String>,
    /// Guidance for this stage. `None` → 1.0.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cfg: Option<f64>,
    /// Refine step count for this stage. `None` → 3.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub steps: Option<u32>,
}

/// One runtime LoRA to apply on top of the base DiT for a single render.
///
/// This is the *tunable* path: the adapter is loaded per request and its strength is a
/// per-render knob, unlike a folded variant where the LoRA is baked into the weights at
/// build time. It costs VRAM and time (measured 2026-07-28 at 1920x1088/145f: +686 MiB and
/// +16% wall-clock for one adapter), so prefer a folded variant for always-on fixed-strength
/// effects and use this for tuning, experiments, and stacking.
///
/// `path` is the file name as the engine's `--lora-model-dir` sees it, **including the
/// extension** (e.g. `"ltx2.3-audio-reactive-v2.gguf"`). The engine resolves it against a
/// cache keyed on the path relative to that directory; a bare stem does NOT resolve and the
/// request is rejected. See `LtxVideoProps::loras` for the names the server offers.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LtxRuntimeLora {
    /// File name under the engine's LoRA directory, extension included.
    pub path: String,
    /// Strength. 1.0 = the LoRA's native scale. The engine applies it as a plain multiplier
    /// (these LoRAs carry no `.alpha` tensors, so there is no alpha/rank rescaling).
    pub multiplier: f32,
}

/// Request body for `POST /api/v1/ltx-video/generate`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LtxVideoGenerateRequest {
    /// Per-segment director prompts (>=1). Segment count = `segments.len()`.
    pub segments: Vec<LtxSegment>,
    /// Base DiT variant to render with (server-registered name, e.g. "base",
    /// "audioreactive", "lipdub"). `None` → "base". Switching variants triggers a
    /// leak-free unload/reload on the engine. See `LtxVideoProps::variants`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Runtime LoRAs to apply on top of `model` for this render, each with its own strength.
    /// Empty → none. Stackable; every adapter costs VRAM and sampling time, so keep the list
    /// short. Orthogonal to `model`: a folded variant already has its LoRA in the weights and
    /// does not need (and should not be given) the same adapter again here.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub loras: Vec<LtxRuntimeLora>,
    #[serde(default)]
    pub image_source: LtxImageSource,
    #[serde(default)]
    pub audio_source: LtxAudioSource,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub negative_prompt: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub width: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub height: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fps: Option<u32>,
    /// Per-segment frame count (default 97 ≈ 4s @ 24fps).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub segment_frames: Option<u32>,
    /// K overlap latent frames carried between segments (default 3).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cont_latent_frames: Option<u32>,
    /// How a continuation carries the previous shot: `"pixel"` (default) re-encodes its last
    /// `8K + 1` DECODED frames through the VAE and appends them with `LTXVAddGuide`; `"latent"`
    /// takes the same temporal window straight off the previous segment's latent with
    /// `LTXVAddLatentGuide`, which is how Lightricks' own looping sampler joins tiles — no decode,
    /// no re-encode, no round trip.
    ///
    /// ⚠️ Measured 2026-08-15 on a two-shot A/B: this is a WASH on picture, not a win. Best
    /// contrast of the reference arms (−3.4% against −5.3%) and the worst saturation step
    /// (+4.46pp against +3.83pp); the motion step at the join does not improve. It is here because
    /// it removes a VAE round trip per join, not because it looked better.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cont_carry_mode: Option<String>,
    /// Distilled DMD steps — default 8.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub steps: Option<u32>,
    /// Distilled is cfg-free (1.0); only the dev model responds to >1.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cfg_scale: Option<f64>,
    /// How hard image pins (keyframes, per-shot scene images, opener) are held: 1.0 = exact
    /// (the pinned image IS that frame, crispest), lower = the frame can flex toward the
    /// generated motion (softer blend). Maps to the engine's conditioning `strength`. None →
    /// koblem's default. Only sent when a render actually pins an image.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pin_strength: Option<f64>,
    /// Prompt Relay Gaussian tail floor, chain-global. `None` → the engine default (0.01). Lower
    /// = harder partitioning between beats; the paper's 0.001 is too sharp at our latent-frame
    /// counts. Only meaningful when some shot carries [`LtxSegment::beats`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub relay_eps: Option<f64>,
    /// Fraction of the base sampling schedule that carries the relay mask, chain-global. `None` →
    /// 1.0 (the paper). Semantic layout is committed early, so a lower value costs less mask
    /// bandwidth for most of the schedule.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub relay_steps_frac: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sampling_method: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scheduler: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seed: Option<i64>,
    /// Legacy spatial latent upscaler toggle. The production default is the staged ×2
    /// path; set `upscaler_ratio` explicitly to `None` to opt out.
    ///
    /// Legacy boolean kept for backward compatibility: `true` ≡ `upscaler_ratio = X2` when
    /// `upscaler_ratio` is absent. Prefer `upscaler_ratio`.
    #[serde(default)]
    pub upscale: bool,
    /// Spatial upscaler ratio for the hires pass. When absent, falls back to the legacy
    /// `upscale` bool (`true` → ×2). Absent requests use the production ×2 path.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub upscaler_ratio: Option<LtxUpscalerRatio>,

    /// Refine (hires) pass sampler. `None` → koblem's ver3 default ("euler").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refine_sampler: Option<String>,
    /// Refine (hires) pass step count. `None` → koblem's ver3 default (2).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refine_steps: Option<u32>,
    /// Explicit base-pass sigma schedule. `None` → koblem's ver3 default array.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_custom_sigmas: Option<Vec<f32>>,
    /// Explicit refine-pass sigma schedule. `None` → koblem's ver3 default array.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refine_custom_sigmas: Option<Vec<f32>>,

    /// Named quality profile. `None` → [`LtxQualityProfile::Standard`] (byte-identical
    /// to the historical single-hires ver3 request). High / 4x / Custom materialise the
    /// multi-stage upscale contract; see [`LtxQualityProfile`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<LtxQualityProfile>,
    /// Custom-profile multi-stage upscale+refine chain. Consulted only when
    /// `profile == Custom`; each stage is one upsample + SDEdit refine, applied in
    /// order. Empty/`None` → no upscale chain (Custom renders base only). High / 4x
    /// build their own chain and ignore this field.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hires_chain: Option<Vec<LtxHiresStage>>,

    /// "Detail / upscale strength" for the FINAL SDEdit refine stage of the baked High / 4×
    /// recipes: the renoise `sigma0` that stage restarts from (default ~0.85). Higher (→1.0) =
    /// more new detail synthesised at the last upscale (sharper, can drift); lower (→0.5) = more
    /// faithful to the base. koblem rescales that stage's whole sigma schedule by
    /// `value / 0.85` (keeping the terminal 0.0) so the schedule stays monotonic. `None` → the
    /// exact baked schedule (byte-identical). Ignored by Standard (no upscale-refine stage) and
    /// Custom (whose per-stage `custom_sigmas` are authoritative).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub upscale_detail_strength: Option<f64>,

    /// Interpret `width`/`height` as the desired OUTPUT resolution rather than the engine
    /// base pass. When set, the quality profile sizes its base DOWN by the total upscale
    /// factor (a 4×-upscale render starts from a quarter-size base) so every profile lands
    /// on the SAME output. `None`/false = legacy: the dims ARE the base pass and the upscale
    /// chain multiplies from there. The Director sets this true and snaps the target to a
    /// multiple of `32 × factor`; other callers leave it unset (byte-identical behaviour).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dims_are_target: Option<bool>,

    /// Persist the rendered clip under the LTX dir + link to the gpu_logs row.
    #[serde(default = "default_true")]
    pub save_clip: bool,

    /// TASS source-phase scale, chain-global. `None` → the engine default (1.0, the value the
    /// character-sheet LoRA's own metadata declares). Scales the whole rotary phase the reference
    /// tokens are tagged with, so it is the one dial for "how hard is identity held" — but the
    /// checkpoint was trained at 1.0, so moving it is an experiment, not a setting. Only sent when
    /// a render actually carries [`LtxSegment::character_refs`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tass_phase_scale: Option<f64>,

    /// Resume a failed/cancelled render from its banked segments: the ltx-video-server's
    /// own job id (surfaced as `server_job_id`/`resume_job_id` in the job status). The
    /// server skips segments already on disk and continues into the same artifact dir.
    /// Absent/empty = a fresh full render.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resume_job_id: Option<String>,

    /// AUDIO GAP-FILL. Generate the SILENT stretches of the supplied drive clip while holding the
    /// stretches that carry real signal, instead of holding the whole clip. `None`/`false` is the
    /// ordinary behaviour — a supplied drive clip conditions the render entire.
    ///
    /// Use it for "dialogue with generated ambience between the lines", a music bed extended into
    /// an uncovered tail, that shape of thing. Coverage is measured against a dB floor so quiet
    /// room tone reads as signal rather than as a gap.
    ///
    /// ⚠️ Judge the result on the GENERATED regions, not the seams. Everything the engine returns
    /// has been through the audio VAE (64-band log-mel, fmax 8 kHz, invented phase), so the
    /// generated stretches are audibly duller than clean source no matter how smooth the boundary
    /// is. The deliverable still wants the SOURCE re-substituted over the held regions at mux
    /// time; that makes the contrast starker, not gentler. It suits material meant to sit back —
    /// room tone, distant ambience, a tail that fades — and not anything that must hold equal
    /// presence beside clean dialogue.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audio_fill_gaps: Option<bool>,

    /// Project-level default for [`LtxSegment::frame_drop`]. `None` is [`LtxFrameDropMode::Auto`] —
    /// the engine decides per shot, and self-gates itself off every shot that already pins frame 0,
    /// so leaving it unset changes nothing for a project without character references.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frame_drop: Option<LtxFrameDrop>,
}

impl Default for LtxVideoGenerateRequest {
    fn default() -> Self {
        Self {
            segments: Vec::new(),
            model: None,
            loras: Vec::new(),
            image_source: LtxImageSource::default(),
            audio_source: LtxAudioSource::default(),
            negative_prompt: None,
            width: None,
            height: None,
            fps: None,
            segment_frames: None,
            cont_latent_frames: None,
            cont_carry_mode: None,
            steps: None,
            cfg_scale: None,
            pin_strength: None,
            relay_eps: None,
            relay_steps_frac: None,
            sampling_method: None,
            scheduler: None,
            seed: None,
            upscale: true,
            upscaler_ratio: None,
            refine_sampler: None,
            refine_steps: None,
            base_custom_sigmas: None,
            refine_custom_sigmas: None,
            profile: None,
            hires_chain: None,
            upscale_detail_strength: None,
            dims_are_target: None,
            tass_phase_scale: None,
            save_clip: true,
            resume_job_id: None,
            audio_fill_gaps: None,
            frame_drop: None,
        }
    }
}

impl LtxVideoGenerateRequest {
    /// Resolve the effective upscaler ratio: the explicit `upscaler_ratio` when set,
    /// otherwise the legacy `upscale` bool. Absent ratio uses the production ×2 path.
    pub fn effective_upscaler_ratio(&self) -> LtxUpscalerRatio {
        match self.upscaler_ratio {
            Some(r) => r,
            None => LtxUpscalerRatio::X2,
        }
    }
}

/// One rendered LTX clip — mirrors `AvatarClip`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LtxVideoClip {
    pub filename: String,
    pub url: String,
    pub content_type: String,
    pub bytes: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
}

/// Response body for `POST /api/v1/ltx-video/generate`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LtxVideoGenerateResponse {
    pub clip: LtxVideoClip,
    pub segments_rendered: u32,
    pub render_ms: u64,
    pub gate_wait_ms: u64,
    /// The per-segment scripts actually used (after any LLM drafting).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub scripts_used: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gpu_log_id: Option<i64>,
}

/// `GET /api/v1/ltx-video/props` — UI bootstrap.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LtxVideoProps {
    pub healthy: bool,
    pub default_segment_frames: u32,
    pub default_cont_latent_frames: u32,
    pub default_steps: u32,
    pub default_fps: u32,
    pub default_resolution: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_render_sec: Option<f64>,
    /// Server-registered base DiT variant names selectable per render (e.g.
    /// ["base", "audioreactive", "lipdub"]). Populates the model-selector dropdown.
    /// Empty → only the default "base" is available.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub variants: Vec<String>,
    /// Runtime LoRAs the engine's LoRA directory offers, in `LTX_VIDEO_LORAS` order. Populates the
    /// LoRA picker; each entry can be added to a render with its own strength.
    /// Empty → the server exposes no runtime LoRAs.
    ///
    /// 🔴 [2026-07-30] WAS `Vec<String>` of bare file names. It carries [`LtxLoraInfo`] now because
    /// three things a picker needs are not derivable from a file name and were being guessed:
    /// the multiplier the adapter was trained at (1.0 is WRONG for `omninft` and for the close-up
    /// face adapter), the trigger phrase some adapters need in the prompt, and a label. `path` is
    /// still the exact wire string — the offered set is still `LTX_VIDEO_LORAS` and nothing here
    /// filters it, so a file the catalog has never seen comes back annotated with its own name.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub loras: Vec<LtxLoraInfo>,
    /// Whether THIS engine build can resume a render and retake a single shot right now, read live
    /// from its own capabilities (`bank.enabled` AND `resume.supported` AND `retake.supported`).
    ///
    /// 🔴 A DEPLOYMENT FACT, not a feature flag. LTX-2.5 is fully capable of both and answers 501 to
    /// each when its bank directory is unconfigured, so a client that assumed the feature from the
    /// engine's name would offer a control that fails at submit. `None` = the engine did not answer,
    /// or publishes no bank block at all (every C++ ltx-video build, whose banked latents live in a
    /// job dir rather than a swept store) — no live fact, so the caller keeps its own record.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bank_enabled: Option<bool>,
    /// Is the Prompt Relay custom node mounted — i.e. will a shot carrying `beats` render?
    ///
    /// 🔴 The SAME shape as `bank_enabled` and for the same reason: LTX-2.5 implements Prompt Relay
    /// fully, but it is served by `custom_nodes/ltx25_prompt_relay`, which only appears in ComfyUI's
    /// `/object_info` after the **comfyui** container restarts — separately from the comfy-api one
    /// that answers this endpoint. Until it does, `beats` is refused at graph build with a 501, and
    /// that refusal lands after the GPU gate has been waited on. `None` = the engine did not answer
    /// or does not publish the key; the caller then keeps its own record.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub beats_enabled: Option<bool>,
}

// ─── Async video jobs (ltx-video / ltx-2b / wan-vace share one wire shape) ────

/// Lifecycle of a koblem-side video render job. Mirrors the server's
/// `ltx_jobs::LtxJobState` / `wan_jobs::WanJobState`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VideoJobState {
    Queued,
    Rendering,
    Completed,
    Failed,
    Cancelled,
}

impl VideoJobState {
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Failed | Self::Cancelled)
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Rendering => "rendering",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }
}

// ── MiniMax-H3 (the Director's second engine) ───────────────────────────────
//
// # The one thing to understand before reading any of this
//
// **LTX's unit is a segment = one render. H3's unit is a GENERATION = up to ~15 s containing
// 1-N shots.** A shot boundary on H3 is a *cut expressed as prompt text* — `[Shot N] At
// MM:SS.mmm` inside ONE prompt — and the model renders the cut itself. MiniMax's own
// reproducible 768p T2VA request is a single 10-second generation with two shots and an
// explicit cut at `00:04.500`; cross-shot identity is carried by prose ("the captain from
// Shot 1"), and `<scenetrans>` audio continuity across a cut exists ONLY within one
// generation.
//
// ⇒ Rendering shots separately is the DEGRADATION you accept when the timeline exceeds the
// 4-15 s budget. Chaining in order to *get* a cut is strictly worse: it loses `<scenetrans>`,
// loses the prose identity carry, and adds a seam.
//
// Consequences these types encode:
//
// | | LTX | H3 |
// |---|---|---|
// | delivery unit | segment (= shot) | **generation** (= 1-N shots) |
// | retake unit | segment | **generation** |
// | cut between shots | new segment | `[Shot N]` inside one prompt |
// | continuity across the unit boundary | banked latents, frame-exact | a pin or an anchor frame, **never frame-exact** |
//
// So there is no `H3Segment` any more: a request is `generations[]`, and each generation
// carries an ADVISORY `shots[]` recording what koblem packed into it. The AUTHORITY for shot
// structure is the `[Shot N]` markers inside `prompt` — `shots[]` exists so the plan echo, the
// ETA and the UI can talk about shots without reparsing the prompt.
//
// koblem does the packing, because it owns the timeline and the 15 s budget arithmetic. The
// shim receives an explicit `generations[]` list and does not re-pack.
//
// # What H3 does not have
//
// `negative_prompt`, CFG, LoRAs, model variants, the hires/upscale chain, v2v, mid-shot
// keyframes, Prompt Relay beats, `pin_strength`, `resume_job_id`, `retake_segment`, and the
// whole LTX audio-input sheet. These are not fields the engine 400s on — they DO NOT EXIST,
// and anything unrecognised comes back in the 202's `ignored[]`. Their absence here is what
// makes such a request unbuildable rather than merely rejected.
//
// ⚠️ These are koblem's OWN document types, and koblem does NOT serialise them straight to the
// engine — `routes::minimax_h3::build_chain` hand-builds the `/h3/v1/generate` JSON from an
// allowlist, exactly as `routes/ltx_video.rs` hand-builds the LTX chain. That indirection is
// the point: an engine-side key rename is one line in one function instead of a koblibs
// commit, a push and a `cargo update` before anything can build.
//
// The authoritative wire contract is `kobbler/docker/h3-wangp/API.md`; the Python half of it
// is `app/contract.py`, and these types must round-trip against that.

/// The three request shapes — and **two different checkpoints**, not a flag.
///
/// `fl2va` and `ref2va` are separate weights (`minimax_h3_handler.py:25-28`). One generation
/// cannot carry both first/last-frame keyframes and omni references; switching between them
/// inside one job costs a full DiT + text-encoder reload, which the plan echo reports as
/// `reload_required` and folds into the ETA. `t2va` runs on the FL2VA checkpoint with no
/// references at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum H3Task {
    #[default]
    T2va,
    Fl2va,
    Ref2va,
}

impl H3Task {
    /// Which checkpoint family this task loads. Two generations that agree here cost no
    /// reload between them; two that disagree pay a full release + reload.
    pub fn checkpoint_family(self) -> &'static str {
        match self {
            Self::Ref2va => "ref2va",
            Self::T2va | Self::Fl2va => "fl2va",
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::T2va => "t2va",
            Self::Fl2va => "fl2va",
            Self::Ref2va => "ref2va",
        }
    }
}

/// What a MiniMax-H3 reference IS. Ordinal-labelled and routed through the Qwen3-VL text
/// encoder, so the prompt can ADDRESS it (`"<Picture 2> wears the red coat"`).
///
/// This is a different mechanism from LTX's [`LtxCharacterRef`], not a renaming of it. An LTX
/// character ref is a rotary-tagged latent token: you inject an identity and cannot name it.
/// The two do not translate, which is why `character_refs` are never mapped across — a silent
/// translation would look like success.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum H3ReferenceKind {
    /// A still. Up to [`H3_MAX_IMAGE_REFS`] per generation. May NOT carry a soundtrack.
    #[default]
    Image,
    /// A clip at 24 fps, one array entry per frame. Up to [`H3_MAX_VIDEO_REFS`] (3 — it was 2
    /// while we rendered through WanGP). May carry a
    /// soundtrack, which is labelled `<Audio j>` before its own `<Video k>` — see
    /// [`H3Reference::with_audio`], which is a CHOICE and not an inference.
    Video,
    /// A standalone soundtrack. Up to [`H3_MAX_AUDIO_REFS`].
    Audio,
}

/// What a reference is *for*.
///
/// 🔴 **This vocabulary is deliberately RICHER than the engine's.** `app/contract.py` knows five
/// roles (`first_frame | last_frame | reference | continuation_source | reuse`) because that is
/// all its slot placement needs. These eleven are PROMPT-CRAFT distinctions: they decide which of
/// `keyframe completion | reference generation | video editing | video continuation | audio reuse
/// | audio reference` compose into the `summary:` bracket prefix, and what parenthetical each
/// `retention_analysis` line carries. The engine has no slot for the difference between an edit
/// source and a structure source; the prompt very much does.
///
/// [`engine_role`](Self::engine_role) maps down. Widening the ENGINE contract to match this set
/// would be the wrong direction — it would add five values the shim would have to collapse anyway,
/// on a surface where every extra value is another thing `validate_generation` can disagree about.
///
/// 🔴 **This is the ONLY role enum.** koblem's prompt emitter (`routes::h3_prompting`) kept a
/// byte-identical copy of these eleven variants and of [`default_for`](Self::default_for), and the
/// two were cross-referenced by comment rather than by the compiler. That drift fails SILENTLY and
/// asymmetrically: the render path decides what conditioning is actually sent (through
/// [`engine_role`](Self::engine_role)) while the prompt path decides what the `summary:` bracket
/// prefix CLAIMS was sent, so one variant meaning "continuation source" on one side and
/// "reference" on the other produces a prompt whose stated task type contradicts the request. The
/// prompt-CRAFT that cannot live in a wire crate — the English, and `retention_scope` — is free
/// functions over this type in `h3_prompting`, never a second enum.
///
/// | role | kind | what it claims |
/// |---|---|---|
/// | `first_frame` / `last_frame` | image | FL2VA keyframe at t=0 / t=duration — the ONLY frame-exact mechanism here |
/// | `identity_source` | image | defines a character/scene/costume/style (`GUIDE-ref:68`) |
/// | `frame_anchor` | image | the image IS a concrete frame, declared in prose (`GUIDE-ref:65`) |
/// | `storyboard` | image | viewpoint, subject placement, shot order (`GUIDE-ref:73`) |
/// | `edit_source` | video | the video is directly modified (`GUIDE-ref:136`) ⇒ *video editing* |
/// | `continuation_source` | video | new content continues from it (`GUIDE-ref:137`) ⇒ *video continuation* |
/// | `structure_source` | video | camera movement / cuts / rhythm only (`GUIDE-ref:143`) ⇒ *reference generation* |
/// | `voice_timbre` | audio | referenced for a speaker's voice and delivery (`GUIDE-ref:96`) |
/// | `audio_reuse` | audio | its actual samples are reused (`GUIDE-ref:94`) ⇒ *audio reuse* |
/// | `audio_continuity` | audio | referenced for beat / rhythm / continuity (`GUIDE-ref:98`) |
///
/// 🔴 `first_frame`/`last_frame` are the FL2VA CHECKPOINT and everything else is Ref2VA, so the
/// role is not decoration: mixing the two families inside one generation is a hard error.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum H3ReferenceRole {
    // ── FL2VA checkpoint: a real `frame_index` condition ──
    FirstFrame,
    LastFrame,
    // ── Ref2VA checkpoint ──
    IdentitySource,
    FrameAnchor,
    Storyboard,
    EditSource,
    ContinuationSource,
    StructureSource,
    VoiceTimbre,
    AudioReuse,
    AudioContinuity,
}

impl H3ReferenceRole {
    /// The role assumed when the caller did not send one — for the RENDER path and the PROMPT path
    /// alike, which is the point: two copies of this rule that disagreed would make the same
    /// reference claim one task type in the prompt and send another on the wire.
    ///
    /// Deliberately the CONSERVATIVE choice per kind, because every alternative makes a STRONGER
    /// claim that shows up in the `summary:` prefix:
    ///
    ///   * image → `identity_source`: the common case, and the one that does not assert a
    ///     concrete frame anchor.
    ///   * video → `structure_source`: `GUIDE-ref:143` — *"the mere presence of video or audio
    ///     does not automatically create a corresponding task type"*. Defaulting to `edit_source`
    ///     would claim *video editing* for a clip nobody is editing.
    ///   * audio → `voice_timbre`: a reference, never a reuse. Copying is the stronger claim.
    ///
    /// 🔴 This is why the field is an `Option` and not a plain `#[derive(Default)]`: the default
    /// depends on the KIND, which a serde default cannot see. A single flat default would have to
    /// pick one, and whichever it picked would be a wrong claim for two of the three kinds.
    pub fn default_for(kind: H3ReferenceKind) -> Self {
        match kind {
            H3ReferenceKind::Image => Self::IdentitySource,
            H3ReferenceKind::Video => Self::StructureSource,
            H3ReferenceKind::Audio => Self::VoiceTimbre,
        }
    }

    /// True for the two FL2VA keyframe roles. Kept as a method rather than an open-coded
    /// `matches!` at each site because the fl2va/ref2va exclusion check is made in several
    /// places and a new keyframe role would otherwise have to be remembered in all of them.
    pub fn is_keyframe(self) -> bool {
        matches!(self, Self::FirstFrame | Self::LastFrame)
    }

    /// The five-value role the ENGINE understands (`app/contract.py::RefRole`).
    ///
    /// Everything the shim has no slot for collapses to `reference`, which is what it would have
    /// done with the extra value anyway — the distinction is carried in the PROMPT, which is the
    /// only place the model can read it.
    pub fn engine_role(self) -> &'static str {
        match self {
            Self::FirstFrame => "first_frame",
            Self::LastFrame => "last_frame",
            Self::ContinuationSource => "continuation_source",
            Self::AudioReuse => "reuse",
            Self::IdentitySource
            | Self::FrameAnchor
            | Self::Storyboard
            | Self::EditSource
            | Self::StructureSource
            | Self::VoiceTimbre
            | Self::AudioContinuity => "reference",
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::FirstFrame => "first_frame",
            Self::LastFrame => "last_frame",
            Self::IdentitySource => "identity_source",
            Self::FrameAnchor => "frame_anchor",
            Self::Storyboard => "storyboard",
            Self::EditSource => "edit_source",
            Self::ContinuationSource => "continuation_source",
            Self::StructureSource => "structure_source",
            Self::VoiceTimbre => "voice_timbre",
            Self::AudioReuse => "audio_reuse",
            Self::AudioContinuity => "audio_continuity",
        }
    }

    /// Parse a role off the wire, tolerating the ENGINE's five-value vocabulary.
    ///
    /// A document may carry `"reference"` — the engine's catch-all, and what koblem itself sent
    /// before these eleven existed. It has no single answer here (it means `identity_source` on an
    /// image, `structure_source` on a video, `voice_timbre` on audio), so it deliberately parses
    /// as `None` and lets [`default_for`](Self::default_for) resolve it against the kind. That is
    /// the same conservative reading `"reference"` always had.
    ///
    /// `"reuse"` maps, because it only ever applied to audio and so has exactly one meaning.
    pub fn from_wire(s: &str) -> Option<Self> {
        Some(match s {
            "first_frame" => Self::FirstFrame,
            "last_frame" => Self::LastFrame,
            "identity_source" => Self::IdentitySource,
            "frame_anchor" => Self::FrameAnchor,
            "storyboard" => Self::Storyboard,
            "edit_source" => Self::EditSource,
            "continuation_source" => Self::ContinuationSource,
            "structure_source" => Self::StructureSource,
            "voice_timbre" => Self::VoiceTimbre,
            "audio_reuse" | "reuse" => Self::AudioReuse,
            "audio_continuity" => Self::AudioContinuity,
            // "reference", and anything a newer client invents. Falls back to the kind's default
            // rather than failing the whole document: a timeline that will not deserialise is a
            // project the user cannot open, which is a far worse outcome than one reference making
            // a slightly weaker claim in the summary prefix.
            _ => return None,
        })
    }
}

/// Lenient `role` deserialiser — see [`H3ReferenceRole::from_wire`] for why an unrecognised value
/// is `None` and not an error.
///
/// `pub` because EVERY struct carrying an `Option<H3ReferenceRole>` must use it, in this crate and
/// in koblem's prompt emitter. A derived deserialiser on that field would reject the engine's own
/// legacy `"reference"` spelling and fail the whole enclosing document — for the Director that is
/// a project the user cannot open, which is a far worse outcome than one reference making a
/// slightly weaker claim in the `summary:` prefix.
pub fn h3_role_lenient<'de, D>(d: D) -> Result<Option<H3ReferenceRole>, D::Error>
where
    D: Deserializer<'de>,
{
    let raw = Option::<String>::deserialize(d)?;
    Ok(raw
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .and_then(H3ReferenceRole::from_wire))
}

/// One conditioning asset, listed on the GENERATION that uses it.
///
/// ⚠️ **Order is semantic.** A reference's position in the array assigns its
/// `<Picture i>` / `<Video k>` / `<Audio j>` ordinal AND advances a shared rotary clock, so
/// the same references in a different order are a different request. Nothing may sort, group
/// by kind, or de-duplicate this list.
///
/// Numbering is per-modality and follows WanGP's pipeline add-order: reference images
/// (`pipeline.py:311-312`), then reference videos (`:330-333`), then standalone audio
/// (`:334-337`). A video carrying a soundtrack yields its `<Audio k>` **before** its
/// `<Video k>` (`pipeline.py:249-254`).
///
/// 🔴 There is no `segments` scope any more. Under the old contract a reference was
/// request-level and narrowed to shots by an index list; scope is now expressed by WHICH
/// GENERATION lists it. That removes the whole class of bug where a scope array and a
/// filtered shot list disagreed about index space — and it is why a generation retake can
/// take its generation's `references` verbatim instead of rewriting every scope.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct H3Reference {
    pub kind: H3ReferenceKind,
    /// What this reference is FOR. Additive: absent falls back to
    /// [`H3ReferenceRole::default_for`] the kind — see there for why the default cannot be a flat
    /// `Default` impl, and why the conservative choice per kind is the right one.
    ///
    /// 🔴 Sending a real role is what UNLOCKS three of the six task types. Without one,
    /// `default_for` picks the weakest claim per kind and the `summary:` bracket prefix can only
    /// ever read `[reference generation …]` — *video editing*, *video continuation* and *audio
    /// reuse* become unreachable, silently, with a render that looks merely mediocre.
    #[serde(
        default,
        deserialize_with = "h3_role_lenient",
        skip_serializing_if = "Option::is_none"
    )]
    pub role: Option<H3ReferenceRole>,
    /// `kind: Image` — base64 (or an `asset:<hash>` ref in a saved Director document). Becomes
    /// a multipart `ref_<n>` part on the wire.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image: Option<String>,
    /// `kind: Video` — a CONTAINER (webm/mp4), base64 or an `asset:<hash>` ref. Becomes a
    /// multipart `ref_<n>` part on the wire.
    ///
    /// 🔴 New carrier, and the reason [`frames`](Self::frames) is now legacy: the C++ engine took
    /// a decoded frame array, WanGP's `_add_video_reference` takes a FILE. There is no frame-array
    /// path on the wire at all, so a document still holding `frames` is refused with a message
    /// rather than having its array silently dropped.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub video: Option<String>,
    /// LEGACY `kind: Video` carrier — the clip at 24 fps, ONE ENTRY PER FRAME, as the C++ engine
    /// wanted it. Kept so a saved document still PARSES (and so the refusal can name what it
    /// found), never sent: see [`video`](Self::video).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frames: Option<Vec<String>>,
    /// A soundtrack, for a `Video` or `Audio` reference. An `Image` reference carrying one
    /// fails the generation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audio: Option<String>,
    /// Absolute path already visible to the engine container (a koblem-staged asset), instead
    /// of shipping bytes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// Latent-bank key — a previous generation's tail, without a round trip through pixels.
    /// The shim mints these as `"<engine job id>:<generation index>"` and reports them on the
    /// poll body's `generations[].bank_key`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bank: Option<String>,
    /// Video only. Does this file's soundtrack become an `<Audio n>` of its own?
    ///
    /// A reference video does NOT create an `<Audio N>` just because the file has sound
    /// (`GUIDE-ref:112`) — it is a deliberate choice, which is why this is a field and not an
    /// inference from `audio.is_some()`.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub with_audio: bool,
    /// Video only. How many frames of the tail to actually condition on.
    ///
    /// 🔴 `_add_video_reference` truncates to the 5+17k grid keeping the **FIRST** frames, so
    /// an off-grid tail is silently shortened and the model is told a lie about time. Legal
    /// values are [`h3_legal_tails`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tail_frames: Option<u32>,
    /// Editor-only label. Never reaches the model: the prompt addresses an ORDINAL, and
    /// sending a name would imply otherwise.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

impl H3Reference {
    /// This reference's role, or the conservative default for its kind. Every reader goes
    /// through here rather than through `role` directly, so "absent" cannot be read as one thing
    /// on the prompt path and another on the render path.
    pub fn effective_role(&self) -> H3ReferenceRole {
        self.role
            .unwrap_or_else(|| H3ReferenceRole::default_for(self.kind))
    }

    /// Does this reference carry renderable bytes / a pointer to them? Exactly one of the
    /// payload carriers, `path` or `bank` must be present — the shim rejects zero or two.
    pub fn source_count(&self) -> usize {
        let payload = match self.kind {
            H3ReferenceKind::Image => self.image.as_deref().is_some_and(|s| !s.trim().is_empty()),
            H3ReferenceKind::Video => self.video.as_deref().is_some_and(|s| !s.trim().is_empty()),
            H3ReferenceKind::Audio => self.audio.as_deref().is_some_and(|s| !s.trim().is_empty()),
        };
        usize::from(payload)
            + usize::from(self.path.as_deref().is_some_and(|s| !s.trim().is_empty()))
            + usize::from(self.bank.as_deref().is_some_and(|s| !s.trim().is_empty()))
    }
}

/// How a generation picks up from the one before it.
///
/// Only relevant **across a generation boundary** — inside one generation continuity is free
/// and a cut is a `[Shot N]` marker.
///
/// | mode | keeps | loses |
/// |---|---|---|
/// | `latents` | velocity / motion continuity | slower; contrast + grade shift across the seam |
/// | `frame` | image / framing / identity / grade | some velocity (measured: FREEZE) |
/// | `none` | nothing — a hard cut | — |
///
/// 🔴 Neither is frame-exact and no UI may promise LTX-style exactness: MiniMax say Ref2VA
/// *"treats the input video as reference material, not as a pixel-aligned edit source"* and
/// expose no denoising-strength control. Our latent pin is an out-of-distribution patch of our
/// own — the best known path, not the model's contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum H3ContinuationMode {
    #[default]
    Latents,
    Frame,
    None,
}

impl H3ContinuationMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Latents => "latents",
            Self::Frame => "frame",
            Self::None => "none",
        }
    }
}

/// Cross-generation continuation. Absent on the first generation of a job.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct H3Continuation {
    #[serde(default)]
    pub mode: H3ContinuationMode,
    /// Bank key, or [`H3_CONTINUATION_FROM_PREV`] for the immediately preceding generation in
    /// this job. `"prev"` is resolved at RUN time, so it survives a retake that re-renders an
    /// earlier generation — a resolved-at-build key would point at the take that was replaced.
    ///
    /// Serialises as `"from"`; `from` is not a Rust keyword, so no rename is needed.
    #[serde(default = "h3_continuation_from_default")]
    pub from: String,
    /// Latent frames carried. K=7 (22 pixel frames) is the saturation point: K=2 scores 6%,
    /// K=7 91%, K=12 91%, K=17 93% — and K=17 costs 2.4x K=7's conditioning rows. The cliff is
    /// between 2 and 7, which is why the default is 7 and not the largest value that works.
    /// Constraint the shim checks: `(T_prev - k) % 5 == 0`, where **`T_prev` is the previous
    /// generation's LATENT TOKEN count** ([`h3_latents_for_frames`]) and not its frame count.
    /// See [`h3_carry_is_on_phase`] — the legal depths are `{2, 7, 12, …}` at every length.
    #[serde(default = "h3_continuation_k_default")]
    pub k: u32,
    /// Add a frame anchor at the target origin (`H3_PIN_ANCHOR`, byte-identical to FL2VA's
    /// "first" keyframe). Leading candidate for the grade/contrast shift: the pin never shares
    /// a coordinate with any target row, so shot 2's global level is otherwise unconstrained.
    ///
    /// ⚠️ Not `H3_ANCHOR_REF`, which is a generic image reference and not an anchor.
    #[serde(default = "h3_default_true")]
    pub anchor: bool,
}

/// 🔴 GONE, and deliberately not `#[serde(deny_unknown_fields)]`-fatal: `tail_frames`,
/// `drop_guide` and `posterior_noise` were WanGP continuation fields. The ComfyUI backend
/// sets the carry's depth from `k` alone (k=7 IS 22 pixel frames), has no video guide to
/// drop, and offers a MIX FACTOR rather than an additive sigma. Serde ignores them on the
/// way in, so a project saved before this still loads; they simply stop reaching the engine,
/// which used to answer every continued shot with two warnings about defaults nobody set.
impl Default for H3Continuation {
    fn default() -> Self {
        Self {
            mode: H3ContinuationMode::default(),
            from: h3_continuation_from_default(),
            k: h3_continuation_k_default(),
            anchor: true,
        }
    }
}

/// One shot inside a generation. **Advisory** — see the module note: the authority for shot
/// structure is the `[Shot N] At MM:SS.mmm` markers inside the generation's `prompt`, because
/// the model reads the text and renders the cut from it.
///
/// This array exists so the plan echo, the ETA and the UI can talk about shots without
/// reparsing the prompt, and so that if shot-packing ever moves engine-side it can become
/// authoritative without a wire change.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct H3Shot {
    pub index: u32,
    /// `None` for the first shot — `[Shot 1]` is untimed. Later shots carry an ABSOLUTE cut
    /// time, strictly increasing, inside the generation's duration.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub at_seconds: Option<f64>,
    /// Editor-only. Which timeline clip this shot came from, for the UI's benefit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

/// One render: 1-N shots, 4-15 s, one checkpoint, one prompt.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct H3Generation {
    /// `None` = derive it from the conditioning actually present (which is what koblem does,
    /// in `build_chain`); `Some` = an assertion, checked against the derivation so a
    /// hand-edited or older document cannot silently render on the other checkpoint.
    ///
    /// The wire always carries a stated task — API.md §2, *"derived by koblem, stated"* — so
    /// this being optional is a DOCUMENT-side convenience, not a wire-side one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task: Option<H3Task>,
    /// Length in frames. Snapped UP to the 5+17k grid and clamped into
    /// [[`H3_MIN_FRAMES`], [`H3_MAX_FRAMES`]] — see [`h3_snap_frames`].
    #[serde(
        default,
        alias = "video_frames",
        skip_serializing_if = "Option::is_none"
    )]
    pub frames: Option<u32>,
    /// The full composed schema block. **Not** prose — H3's prompt is a schema the model was
    /// trained on, and bare prose supplies no `overall_soundscape` and no `non_diegetic_music`
    /// at all, leaving the model to invent the entire audio bed unguided.
    ///
    /// Carries the `[Shot N] At MM:SS.mmm` markers that ARE the shot structure.
    pub prompt: String,
    /// FL2VA keyframes, inline on the generation.
    ///
    /// The wire carries these as `references` entries with `role: first_frame` / `last_frame`
    /// (API.md §2), and `routes::minimax_h3::build_chain` does that translation. They are a
    /// separate field HERE because the editor authors them as a property of the render — "this
    /// generation opens on this still" — and folding them into the reference list at authoring
    /// time would put two things with opposite checkpoint requirements in one array that the user
    /// can reorder.
    ///
    /// 🔴 Mutually exclusive with [`references`](Self::references): different CHECKPOINTS, not a
    /// flag. And frame-exact in a way nothing else here is — `frame_index` accepts only `[0]`,
    /// `[-1]` and `[0,-1]`, so these pin t=0 and t=duration of the WHOLE generation, never of a
    /// shot inside it.
    #[serde(default, alias = "init_image", skip_serializing_if = "Option::is_none")]
    pub first_frame: Option<String>,
    #[serde(default, alias = "end_image", skip_serializing_if = "Option::is_none")]
    pub last_frame: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub shots: Vec<H3Shot>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub references: Vec<H3Reference>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub continuation: Option<H3Continuation>,
    /// `< 0` = engine-random.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seed: Option<i64>,
    /// `0`/absent inherits the engine default (RUNBOOK: 20).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub steps: Option<u32>,
    /// LoRA filenames to apply to THIS generation. Empty = none, which is why plain `Vec`s are
    /// enough here despite the struct deriving `Default`.
    ///
    /// 🔴 The reason this field exists is the turbo LoRA: `h3-turbo-4step-ema-pruned.safetensors`
    /// at 8 [`steps`](Self::steps) is 465.4 s against the 20-step control's 762.8 s — **−39% wall
    /// for ZERO extra VRAM** (6528 MiB both). Per-step cost rises ~5%, so the whole win is the
    /// step COUNT: this field does not change `steps`, and a turbo LoRA at 20 steps buys nothing.
    ///
    /// 🔴 A BARE FILENAME inside the engine's mounted loras dir, never a path. A path would
    /// resolve against WanGP's loras root and could escape it, so `/`, `\` and a leading `.` are
    /// refused (`contract.py:380`, and again in `routes::minimax_h3::build_chain` so the error can
    /// name the generation).
    ///
    /// ⚠️ `"(N/N) applied"` in the engine log proves the FILE LOADED, not that any adapter
    /// attached to anything. A LoRA that loads but fails to attach renders 20-step quality in 8
    /// steps' time — it looks BROKEN rather than fast, which is the opposite of how a missing
    /// speedup usually presents. Judge the output, not the log line.
    ///
    /// ⚠️ Cost scales with the NUMBER of LoRAs, not their rank: runtime adapter cost is activation
    /// volume, so a rank-8 costs what a rank-128 costs and a second LoRA is another ~5%/step. At
    /// most 8 (`contract.py:346`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub loras: Vec<String>,
    /// Per-LoRA multiplier, matched POSITIONALLY to [`loras`](Self::loras). A shorter list is
    /// padded with 1.0 by the engine; a LONGER one is an error rather than a silent truncation,
    /// because it means the caller has a different lora list in mind than the one it sent.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub lora_multipliers: Vec<f64>,
    /// Per-generation override of the job canvas. Rarely used.
    ///
    /// (The old note here said the RUNBOOK's advice was to "adjust LENGTH, not resolution". That
    /// rule had no source and was retracted 2026-08-07: 1344x768 is MiniMax's own 768p canvas and
    /// a sensible DEFAULT, not a constraint. `768x1344` costs identically, `1152x640` is ~0.71x
    /// rows and ~0.51x attention.)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub width: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub height: Option<u32>,
    /// 🔴 **The only lever on what an image reference COSTS.** `pipeline.py:210` canvases every
    /// reference image to `width * height * this / 100` pixels, rounded to the 32-grid — so the
    /// DiT rows a still adds, `(H/32) * (W/32)`, come from THIS and the output canvas and are
    /// very nearly independent of the file's own dimensions.
    ///
    /// A 3840x2160 photo and a 220x300 thumbnail both land on ~1008 rows at 1344x768 / 100%,
    /// paid on every step of every shot they are scoped to. **Shrinking the upload saves
    /// nothing**; this does. 50 roughly halves it.
    ///
    /// What it spends is spatial sampling density, not field of view — H3's reference RoPE is
    /// normalised by the clip's own latent area, so a lower value is the same framing sampled
    /// more coarsely and fine detail (identity, texture, small text) degrades before
    /// composition does.
    ///
    /// 🔴 **ACCEPTED, NOT HONOURED, since WanGP was replaced on 2026-08-12.** The ComfyUI
    /// node has a two-valued `ref_image_size` combo (`match` / `max`) where WanGP had this
    /// percentage, so the shim validates the 50-400 range, maps anything in it to `match`, and
    /// says so in the plan warnings. See [`H3_REF_RELATIVE_SIZE_MIN`]. `None` is the same thing
    /// without the warning, and is what a new caller should send.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image_refs_relative_size: Option<u32>,
}

/// A saved H3 Director timeline's compiled `request` — the H3 counterpart of
/// [`LtxVideoGenerateRequest`].
///
/// Everything the LTX request carries that H3 has no concept of is simply not here:
/// `sample_method`, `scheduler`, `sigma_shift_video`, `sigma_shift_audio`, `adapt_canvas`,
/// `output_format`, request-level `init_image`/`end_image` and `seconds` are all GONE. Flow
/// shift is fixed at 12.0 and the sampler is driven by the engine's own env (`H3_SOLVER`,
/// which is read only by `h3_sampler.py` — setting it without that patch applied is a silent
/// no-op, which is why it is not a wire field), so a caller cannot express them and cannot
/// believe it did.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct H3GenerateRequest {
    /// The ordered renders (>= 1). Generations with an empty prompt are filtered before
    /// submit, so the rendered indices are the ones the Director's persisted render rows use.
    #[serde(default)]
    pub generations: Vec<H3Generation>,
    /// Job canvas. Both sides MUST be multiples of [`H3_DIM_MULTIPLE`] (rejected otherwise);
    /// `width * height` over [`H3_ADVISORY_MAX_PIXELS`] is a WARNING, not a refusal.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub width: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub height: Option<u32>,
    /// [`H3_FPS`] and nothing else. Present so a document that somehow carried another rate
    /// fails at submit with the engine's own message rather than rendering at a rate the
    /// timeline's arithmetic disagrees with.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fps: Option<u32>,
    /// In-flight preview PNGs. Off by default: `generate_preview` decodes latents through the
    /// RGB-factor path per emission and the cost is UNMEASURED, so it must not silently tax a
    /// production render.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preview: Option<bool>,
    /// Injected by koblem's `tier2_helpers::inject_gpu`. The shim is pinned to one card at
    /// import time (`CUDA_VISIBLE_DEVICES` is fixed before torch loads), so this is VALIDATED,
    /// not honoured: a mismatch is a 400 rather than a silently-wrong placement.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gpu: Option<String>,
    /// Advisory, echoed in the plan and in the engine's logs. koblem's job id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_job_id: Option<String>,
    /// Save the finished clip to the gallery. koblem-side; never sent to the engine.
    #[serde(default = "h3_default_true")]
    pub save_clip: bool,
    /// One song across the whole job. See [`H3Soundtrack`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub soundtrack: Option<H3Soundtrack>,
}

fn h3_default_true() -> bool {
    true
}

/// What happens to H3's OWN generated audio once a soundtrack is supplied.
///
/// 🔴 There is always something to displace, which is what makes this a choice rather than a
/// formality: H3 denoises a synchronised stereo track in the same pass as the picture, so a
/// supplied song is never filling silence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum H3SoundtrackMode {
    /// The song becomes the deliverable's whole audio track. The default, and what a music video
    /// wants: the generated bed is a reconstruction of the song anyway (see [`H3Soundtrack`]), so
    /// keeping it would mean shipping the copy instead of the original.
    #[default]
    Replace,
    /// Keep what the model rendered. The song still CONDITIONS the render — performers still sing
    /// to it — but the deliverable carries H3's own audio, including whatever diegetic sound it
    /// invented. Useful when the song is a timing guide rather than the final track.
    Keep,
    /// The song over a ducked version of the model's audio, so footsteps and room tone survive
    /// under the music. Ducking depth is [`H3Soundtrack::mix_duck_db`].
    Mix,
}

impl H3SoundtrackMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Replace => "replace",
            Self::Keep => "keep",
            Self::Mix => "mix",
        }
    }
}

/// One song laid across a whole H3 job — the music-video path.
///
/// ## It does two separate jobs and needs both
///
/// 1. **Conditioning.** Each generation is handed its own slice as a numbered `<Audio N>` omni
///    reference with the `fully_copy` retention marker, so performers sing the actual song and
///    motion lands on the actual beat. This is the half that requires the engine.
/// 2. **Delivery.** The original file is remuxed over the finished cut.
///
/// 🔴 **The second is not redundant, because `fully_copy` does not copy.** It is a marker in the
/// prompt's `retention_analysis` section declaring a relationship; the output audio still comes out
/// of the joint denoise, so what H3 returns is a RECONSTRUCTION of the song rather than the song.
/// Shipping that would be shipping a lossy cover version. LTX has always worked the same way and
/// for the same reason — drive with the upload, then remux it, because the model's audio is
/// conditioning only.
///
/// ## What koblem does with it
///
/// koblem — not the browser and not the shim — cuts the slices and performs the mux, with ffmpeg.
/// The shim takes a FILE per generation and validates its duration with librosa
/// (`minimax_h3_handler.py:348`, 2–15 s), so a three-minute song handed over whole is rejected on
/// arrival; and WanGP's `audio_guide_window_slicing` does not help, because Ref2VA sets
/// `sliding_window_size = video_length` and there is one window per generation.
///
/// ## The clock
///
/// Slice `i` starts at `deliveredAt(i) - trim(i)/fps` into the song, where `deliveredAt` accumulates
/// `frames - trim` rather than `frames`. Two corrections, both easy to miss and both cumulative:
///
/// - generations snap UP onto `17n+5`, so the slice must follow what RENDERS, not what was authored;
/// - an anchored seam DROPS a frame at delivery (the anchor frame is a reproduction of the previous
///   generation's last frame; delivering both would stutter), so the cut is one frame shorter per
///   seam than the sum of the generation lengths.
///
/// The reach-back is what makes the muxed song land correctly: the dropped frame is a duplicate, so
/// the song moment that would have played over it plays over its surviving twin in the previous
/// generation instead. Nothing is skipped and the overlay is continuous from t=0.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct H3Soundtrack {
    /// The song — an `asset:<hash>` reference, or literal base64 on a legacy document.
    pub audio: String,
    #[serde(default)]
    pub mode: H3SoundtrackMode,
    /// Ducking applied to the model's own audio under the song in [`H3SoundtrackMode::Mix`], in dB
    /// below unity. Ignored in the other modes. `None` uses [`H3_SOUNDTRACK_DEFAULT_DUCK_DB`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mix_duck_db: Option<f32>,
}

/// The placeholder an unresolved soundtrack slice carries in place of an asset hash.
///
/// 🔴 **The soundtrack is an ORDINARY audio reference, authored up front, not something injected at
/// render time.** That is what makes the prompt able to name it: `<Audio N>` ordinals are allocated
/// from the reference list, so a reference that only appears after the prompt was written can never
/// be addressed by it — and an unaddressed `<Audio N>` conditions far more weakly than a declared
/// one. Authoring it up front means `h3_prompting` emits its `subject_definitions` entry and its
/// `fully_copy` retention line with no special-casing at all.
///
/// What the server fills in is only the PAYLOAD: which span of the song this particular generation
/// gets. koblem replaces the sentinel with a real `asset:<hash>` before staging.
///
/// The corollary worth stating: **opting a shot out of the song is ordinary reference scoping.**
/// There is no separate skip list, because "which shots is this reference in scope for" is a
/// control the Director already has and the author already understands.
pub const H3_SOUNDTRACK_SENTINEL: &str = "soundtrack:";

/// Default ducking under [`H3SoundtrackMode::Mix`]. Deep enough that the song is plainly the
/// foreground, shallow enough that room tone and footsteps survive underneath.
pub const H3_SOUNDTRACK_DEFAULT_DUCK_DB: f32 = 12.0;

/// The longest slice [`h3_plan_soundtrack`] will cut, seconds.
///
/// 🔴 **koblem POLICY, NOT AN ENGINE RULE — RECLASSIFIED 2026-08-21.** This said "the longest
/// audio reference WanGP accepts (`minimax_h3_handler.py:348`)", and that citation was the whole
/// basis for it. Checked against the backend that actually renders: `_encode_ref_audio` resamples
/// the waveform to the audio VAE's rate, encodes it, and returns `z.shape[-1]`. **It checks
/// nothing.** There is no minimum, no maximum, and no successor to WanGP's window; what a long
/// reference costs is `duration * 40` conditioning rows on every sampling step, which the engine
/// now publishes as `references.ref_audio.latent_rows_per_second`.
///
/// The value survives because the SLICER still needs one: 15.0 s is [`H3_MAX_FRAMES`] (362 frames
/// = 15.083 s) rounded down, i.e. "never cut a slice longer than the longest legal generation",
/// which is a real fact wearing a WanGP citation. It is not a limit to enforce on a caller's own
/// audio reference, and it is deliberately NOT in `H3_CAPABILITY_MIRRORS`.
///
/// ⚠️ **[`H3_MAX_FRAMES_WITH_AUDIO`] = 345 EXISTS ONLY BECAUSE OF THE WanGP FIGURE and is now a
/// restriction with nothing behind it.** With no 15.0 s reference cap, a soundtracked generation
/// could run the full 362. Left in place because raising it changes the packing arithmetic of
/// every existing timeline; flagged for a decision rather than changed silently.
pub const H3_MAX_AUDIO_REF_SECONDS: f64 = 15.0;
/// The shortest slice [`h3_plan_soundtrack`] will cut, seconds. **Also koblem policy** — see
/// [`H3_MAX_AUDIO_REF_SECONDS`]; the engine has no audio-reference floor either. A sub-2 s musical
/// slice is not worth conditioning on, which is a product judgement, not a wire fact.
pub const H3_MIN_AUDIO_REF_SECONDS: f64 = 2.0;

/// The generation ceiling for a job carrying a soundtrack: 345 frames = 14.375 s.
///
/// Chosen so a generation is never longer than the audio slice it is allowed to carry, which makes
/// "how long is this generation" and "how long is its audio" one number instead of two that can
/// drift. The alternative — keep 362 and trim every slice to 15.0 — leaves the two clocks
/// permanently separated, and any code that advances the song by the slice length instead of the
/// generation length then compounds 0.083 s per generation into a second of drift across a song.
pub const H3_MAX_FRAMES_WITH_AUDIO: u32 = 345;

/// The frame ceiling in force, given whether the job carries a soundtrack.
///
/// One definition, shared by the packer, the timeline and the slice planner, so a generation can
/// never be packed longer than its audio is permitted to be.
pub fn h3_max_frames(has_soundtrack: bool) -> u32 {
    if has_soundtrack {
        H3_MAX_FRAMES_WITH_AUDIO
    } else {
        H3_MAX_FRAMES
    }
}

/// Head frames an anchored continuation loses at delivery.
///
/// 🔴 **A MIRROR of a value koblem does not send.** [`H3Continuation`] deliberately does not carry
/// the field, so the engine applies its own. If that default ever moves, every slice offset computed
/// here is wrong by that many frames per seam — a drift that grows along the cut and that no request
/// would fail on. Guarded by comparing the engine's own `result.trimmed_head_total` against
/// [`H3SoundtrackPlan::delivered_seconds`].
///
/// 🔴 **CHANGED 1 -> 22 ON 2026-08-14, AND IT IS NOT A TWEAK.** comfy-api's continuation now
/// defaults to `placement="on_target"`, which overlays the carry on the target's OWN first frames
/// and trims them: `frames_for_latents(k)`, which is **22 at K=7**. The old value of 1 was the
/// `frame0_anchor="keyframe"` overlay, and that knob is now forced OFF whenever the placement is
/// on-target — the node refuses the pair, because an on-target carry already pins frame 0.
///
/// Why the change was made: a chain's picture gains detail and contrast monotonically down its
/// length, and placement is the whole lever — measured, 2 seeds, 4-segment chains, before_target
/// +34..+44% against on_target +11..+18%.
///
/// ⚠️ **This is `frames_for_latents(7)` and nothing enforces K=7.** koblem's editor sends
/// [`H3_CONTINUATION_K_DEFAULT`] everywhere today, and a different K makes this wrong by
/// `frames_for_latents(K) - 22` per seam.
///
/// The function it should be is [`h3_trim_join`], and it exists — this constant is defined as
/// `h3_trim_join(H3_CONTINUATION_K_DEFAULT)` and a unit test asserts exactly that, so the day K
/// becomes author-settable the compiler cannot be the thing that notices but the test can. It is
/// still a constant because [`h3_plan_soundtrack`] takes `(frames, anchored, wants_slice)` tuples
/// and threading a per-generation K through that signature is a koblem-wide change that no caller
/// currently needs. **If you make K author-settable, change `h3_plan_soundtrack` in the same
/// commit.**
///
/// The rest of [`h3_plan_soundtrack`] is already trim-agnostic: the slice still covers the FULL
/// render (the model renders the trimmed head and needs its audio), the offset reaches back by
/// exactly the trim, and `delivered_at` advances by `frames - trim`. Only this number moved.
pub const H3_TRIM_JOIN: u32 = 22;

/// One generation's share of the song.
#[derive(Debug, Clone, PartialEq)]
pub struct H3AudioSlice {
    /// Zero-based generation index.
    pub generation: usize,
    /// Where the slice starts IN THE SONG. Feeds `ffmpeg -ss`. Never negative.
    pub song_offset_seconds: f64,
    /// How long the slice is. Zero when [`Self::skipped`] is set.
    pub slice_seconds: f64,
    /// Where this generation's first SURVIVING frame lands in the finished cut.
    pub delivered_at_seconds: f64,
    /// Why this generation gets no slice, or `None` when it gets one.
    ///
    /// It still appears in the cut under the muxed song — what it loses is the conditioning, so its
    /// performers are not singing. Reported rather than silent: a generation quietly rendering on
    /// its own bed is indistinguishable from a slice that worked until you listen to it.
    pub skipped: Option<String>,
}

/// The whole job's soundtrack plan. See [`h3_plan_soundtrack`].
#[derive(Debug, Clone, PartialEq)]
pub struct H3SoundtrackPlan {
    pub slices: Vec<H3AudioSlice>,
    /// Length of the finished cut — generation lengths MINUS the seam trims.
    pub delivered_seconds: f64,
    /// Song left over past the end of the last generation, or 0.
    pub song_tail_seconds: f64,
}

/// Cut one song into per-generation slices.
///
/// `generations` is `(frames, is_anchored_continuation, wants_a_slice)` in render order.
/// `wants_a_slice` is simply whether that generation has a soundtrack reference in scope — opting a
/// shot out of the song is ordinary reference scoping, not a separate list. See
/// [`H3_SOUNDTRACK_SENTINEL`].
///
/// Pure arithmetic — it touches no files. The caller turns each `(song_offset_seconds,
/// slice_seconds)` into bytes with `ffmpeg -ss -t` and attaches the result as an `<Audio N>`
/// reference.
///
/// See [`H3Soundtrack`] for why the offsets reach back a frame and why `delivered_at` accumulates
/// `frames - trim`.
pub fn h3_plan_soundtrack(
    generations: &[(u32, bool, bool)],
    song_seconds: f64,
) -> H3SoundtrackPlan {
    let fps = H3_FPS as f64;
    let mut slices = Vec::with_capacity(generations.len());
    let mut delivered_at = 0.0_f64;

    for (i, &(frames, anchored, wants_slice)) in generations.iter().enumerate() {
        let trim = if anchored { H3_TRIM_JOIN } else { 0 };
        // The anchor frame is dropped from delivery but still RENDERED, so the model needs its
        // audio — the slice reaches back by exactly the trim.
        let song_offset = (delivered_at - trim as f64 / fps).max(0.0);
        let generation_seconds = frames as f64 / fps;
        let remaining = song_seconds - song_offset;

        let mut slice_seconds = 0.0;
        let skipped = if song_seconds <= 0.0 {
            Some("no soundtrack supplied".to_string())
        } else if !wants_slice {
            Some(
                "no soundtrack reference is in scope for this generation — it renders on the \
                 model's own generated bed"
                    .to_string(),
            )
        } else if generation_seconds < H3_MIN_AUDIO_REF_SECONDS {
            Some(format!(
                "generation is {generation_seconds:.2}s, under the {H3_MIN_AUDIO_REF_SECONDS}s \
                 minimum WanGP accepts for an audio reference"
            ))
        } else if remaining < H3_MIN_AUDIO_REF_SECONDS {
            Some(format!(
                "the song ends {:.2}s into this generation, less than the \
                 {H3_MIN_AUDIO_REF_SECONDS}s minimum for a slice",
                remaining.max(0.0)
            ))
        } else {
            // The `min` against the 15.0s cap is a BACKSTOP. With `H3_MAX_FRAMES_WITH_AUDIO` in
            // force a generation is already inside it; this catches a document packed before the cap
            // existed, where the alternative is a 400 from librosa after the queue wait.
            slice_seconds = generation_seconds
                .min(H3_MAX_AUDIO_REF_SECONDS)
                .min(remaining);
            None
        };

        slices.push(H3AudioSlice {
            generation: i,
            song_offset_seconds: song_offset,
            slice_seconds,
            delivered_at_seconds: delivered_at,
            skipped,
        });
        delivered_at += (frames - trim) as f64 / fps;
    }

    H3SoundtrackPlan {
        slices,
        delivered_seconds: delivered_at,
        song_tail_seconds: (song_seconds - delivered_at).max(0.0),
    }
}

/// See [`H3Continuation::from`].
pub const H3_CONTINUATION_FROM_PREV: &str = "prev";

fn h3_continuation_from_default() -> String {
    H3_CONTINUATION_FROM_PREV.to_string()
}
fn h3_continuation_k_default() -> u32 {
    H3_CONTINUATION_K_DEFAULT
}

/// The carry depth the editor sends everywhere, and the engine's own default
/// (`continuation.k.default`).
///
/// Measured saturation point: at 107f/12, K=2 scores 6%, K=7 91%, K=12 91%, K=17 93%. The cliff is
/// between 2 and 7. `latents_for_frames` is 2 mod 5 at every legal frame count, so the legal
/// depths are {2, 7, 12, …} and there is nothing between 2 and 7 to choose.
pub const H3_CONTINUATION_K_DEFAULT: u32 = 7;

/// H3's frame grid: a legal length is `17n + 5`. (LTX's is `8k + 1`.)
pub const H3_FRAME_PERIOD: u32 = 17;
/// The offset of H3's frame grid.
pub const H3_FRAME_BASE: u32 = 5;
/// The floor on a generation, in frames (0.917 s). **Mechanical, and measured.**
///
/// 22 = 5 + 17*1 is the shortest clip the video VAE can actually DECODE. Its chunk arithmetic
/// subtracts a whole chunk whenever `token_drop > 0`, so a 2-latent block (5 frames) yields
/// `5 // 5 - 1 == 0` chunks, the decode loop never runs and the VAE raises
/// `decoded 0 frames, expected 5`. 7 latents / 22 frames is the first block that decodes.
/// **5 frames is genuinely rejected**; everything from 22 up on the `5 + 17k` grid is not.
///
/// 🔴 This was **107** and the comment here called it *"not a preference — anything shorter is an
/// engine-side exception"*. That was FALSE and is corrected. 107 is the grid-snap of MiniMax's
/// documented 4-second DURATION guideline (README: *"Output duration | 4-15 seconds"*), which
/// WanGP hardened into a `raise` at `pipeline.py:296`. Nothing else agrees with it: ComfyUI's
/// floor is **5** (`nodes_minimax_h3.py:90`), diffusers raises only below 1, and our own
/// `docker/wan2gp-dev/h3_short_clips.py` renders 22-frame clips.
///
/// ⚠️ **The engine still has to be told.** WanGP's 4.0 s gate is in force unless the container
/// runs `h3_short_clips` (`H3_IMPORT` + `H3_SHORT_CLIPS=1`). Where it is not, the shim raises a
/// sub-107 request to 107 and SAYS SO in the plan warnings — see
/// `docker/h3-wangp/app/config.py::effective_min_frames`. It is never silent either way.
pub const H3_MIN_FRAMES: u32 = 22;
/// The grid-snap of MiniMax's documented 4-second duration guideline (README: *"Output duration |
/// 4-15 seconds"*). 4.0 s * 24 = 96 frames, and the first grid point at or above 96 is 107.
///
/// **Advisory, not a limit.** Shorter renders work (see [`H3_MIN_FRAMES`]); they are simply
/// outside the range MiniMax document, so a caller that goes under it is told once, clearly,
/// rather than having its request rewritten.
pub const H3_DOCUMENTED_MIN_FRAMES: u32 = 107;
/// The cap on a generation, in frames (15.083 s).
///
/// 362 = 5 + 17*21. MiniMax document 4-15 s and 362 is upstream's own `window_default`;
/// WanGP's 481 (20.0 s) is well outside. Defined HERE, once, and echoed by the shim through
/// `/h3/v1/capabilities` so a UI reads the ceiling off the engine rather than keeping a second
/// copy that drifts.
pub const H3_MAX_FRAMES: u32 = 362;
/// The ONLY legal H3 frame rate. The frame grid, the 4x temporal VAE and the 40 Hz audio
/// latent rate are all DEFINED at 24 fps, so this is a model fact and not a default — the
/// engine refuses any other value rather than resampling.
pub const H3_FPS: u32 = 24;
/// Canvas grid: both dimensions must be a multiple of this. **Model law, and hard.**
///
/// The VAE downsamples 16x spatially and the DiT patchifies 2x on top of it, so an off-grid side
/// has no whole-number latent width. Rejected, never snapped — snapping silently renders a
/// canvas nobody asked for.
///
/// 🔴 The two sizes every user types are BOTH illegal: 720 / 32 = 22.5 and 1080 / 32 = 33.75.
/// The legal neighbours are 1280x704 (or 1280x736) and 1920x1088.
pub const H3_DIM_MULTIPLE: u32 = 32;
/// Canvas AREA guidance: `width * height` above this will probably not fit the 16 GB card.
///
/// 🔴 **Advisory, not a rule.** `diffusers`' `packing.py:53-54` calls this exact number a
/// *"soft area cap"*; ComfyUI does not enforce it at all; and there is **no max-resolution key
/// in any config in the MiniMax weights tree**. We used to answer 400 on it, and the concrete
/// cost was that true 16:9 at a 768 short edge — **1376x768 = 1,056,768 px, 2.4% over** — could
/// not be rendered at all. Nobody noticed because MiniMax's own 768p canvas is 1344x768, which
/// is 7:4 (1.75), not 16:9 (1.7778): the area cap is exactly what squashes it.
///
/// What IS real is VRAM. 1344x768 is the shape every number in the RUNBOOK was measured at, and
/// a much larger canvas will OOM. So the honest message is *"this may not fit"*, not *"this is
/// illegal"* — over-budget is a warning that reaches the plan echo and the logs, and the render
/// is attempted.
pub const H3_ADVISORY_MAX_PIXELS: u32 = 1_032_192;
/// Prompt cap, in characters. MiniMax's own serving limit.
///
/// REJECTED, never truncated: the six-section schema puts `non_diegetic_music` LAST, so a
/// truncation drops the whole music direction and the render merely sounds wrong.
pub const H3_MAX_PROMPT_CHARS: usize = 7000;

/// Reference caps. MiniMax's own documented figures, and what the ComfyUI node accepts.
///
/// Mirrors `references.max_image` in `/h3/v1/capabilities`; see [`h3_capability_drift`].
pub const H3_MAX_IMAGE_REFS: usize = 9;
/// 🔴 **3 since 2026-08-21, and the 2 it replaced was a WanGP fact that outlived WanGP.**
///
/// The old comment here argued the cap was structural: WanGP holds exactly two named slots per
/// kind (`video_guide`/`video_guide2`, `audio_guide`/`audio_guide2`) and its `audio_prompt_type`
/// alphabet only spells `""`, `"A"`, `"AB"`, `"K"`, so there was nowhere to put a third and
/// raising the constant alone would have been refused at validation. That was TRUE and is now
/// obsolete — **ComfyUI replaced WanGP in production on 2026-08-12** and there are no named slots
/// any more. `MiniMaxH3ReferenceToVideo`'s `ref_videos` / `ref_video_audios` / `ref_audios` are
/// `COMFY_AUTOGROW_V3` inputs whose templates DECLARE `max: 3` (and `ref_images` `max: 9`),
/// verified against a live `/object_info`. So the 3 is not an absence of a limit — it is the
/// node's own declared maximum, and it agrees with MiniMax's documented figure. The engine says
/// the same in its own source (`plugins/h3/contract.py`: `MAX_VIDEO_REFS = 3  # the ComfyUI node
/// declares 3, not WanGP's 2`) and publishes `references.limited_by.max_video = "model"`.
///
/// Mirrors `references.max_video` in `/h3/v1/capabilities`; see [`h3_capability_drift`].
pub const H3_MAX_VIDEO_REFS: usize = 3;
/// 🔴 **3** — same story as [`H3_MAX_VIDEO_REFS`]: autogrow, no slot ceiling, MiniMax's
/// documented figure. Mirrors `references.max_audio`.
///
/// ⚠️ The OTHER audio rule is NOT WanGP-era and still stands: a reference video's own
/// soundtrack (`with_audio`) and a standalone audio reference are two SOURCES the engine keeps in
/// two different autogrow groups (`ref_video_audios` vs `ref_audios`). They do not compete for the
/// same slot here, but they do both count toward [`H3_MAX_TOTAL_REFS`].
pub const H3_MAX_AUDIO_REFS: usize = 3;
/// Total across all kinds, per generation. Mirrors `references.max_total`.
pub const H3_MAX_TOTAL_REFS: usize = 12;
/// Accepted range of [`H3Generation::image_refs_relative_size`], still validated 50-400 by the
/// engine (`plugins/h3/contract.py`, `_int(rel_size, …, lo=50, hi=400)`).
///
/// 🔴 **ACCEPTED IS NOT HONOURED.** The percentage was a WanGP lever. ComfyUI's node has a
/// two-valued combo instead — `ref_image_size` ∈ {`match`, `max`} — so the shim maps any value in
/// this range onto `match` (i.e. 100) and WARNS that it did. Sending 400 does not make a reference
/// four times bigger; it makes a warning. The live lever is
/// `references.ref_image_size` in `/h3/v1/capabilities`, and a UI that wants to move a reference's
/// cost should offer that enum rather than this slider.
///
/// 🔴 There is still NO equivalent lever on the FILE. A reference image is canvased to the
/// generation's own area, so a 4K photo and a 220px crop cost identical DiT rows.
pub const H3_REF_RELATIVE_SIZE_MIN: u32 = 50;
/// See [`H3_REF_RELATIVE_SIZE_MIN`].
pub const H3_REF_RELATIVE_SIZE_MAX: u32 = 400;
/// The shortest reference video the model will accept: **5 frames (~0.21 s at 24 fps)**.
///
/// 🔴 **HARD, AND IT WAS 48 UNTIL 2026-08-21.** 48 was 2.0 s at 24 fps out of WanGP's
/// `validate_generative_settings:274-275`. The comment that set it there said the previous value
/// of 5 "was WRONG — that was the VAE grid cell, not the validator's floor". That correction was
/// right FOR WanGP and is wrong again for ComfyUI: `nodes_minimax_h3.py:248` raises
/// `ValueError("MiniMax H3 reference videos need at least 5 frames (~0.2s at 24 fps)")`, in its
/// own words, and there is no other length check anywhere in the node. The node's `ref_video`
/// tooltip still SAYS "2-15s" and nothing reads it.
///
/// This is the one reference-video rule that is a REFUSAL, because it is the only one that ends
/// in an exception — minutes into the render, after the DiT and the text encoder have loaded. The
/// engine now probes the staged container with `ffprobe` and answers 400 at submit; see
/// `plugins/h3/contract.py::check_ref_video_frames`.
///
/// ⚠️ koblem cannot check it. It sees a container and cannot count frames — which is why this
/// belongs in the editor, at UPLOAD time, where the browser knows the duration and the user can
/// still pick a different clip.
pub const H3_MIN_REF_VIDEO_FRAMES: usize = 5;

/// The longest reference video, in frames — **and it is deliberately a FUNCTION, not a constant.**
///
/// 🔴 `H3_MAX_REF_VIDEO_FRAMES = 360` (15.0 s) WAS a constant and is deleted. It was the other
/// half of WanGP's 2-15 s window. The node has no fixed ceiling at all: `if frames.shape[0] >
/// frame_count: frames = frames[:frame_count]` — a longer reference is TRUNCATED to **this
/// generation's own length**. So the ceiling moves with every request, and a constant could only
/// ever have been wrong in one direction or the other.
///
/// Not a refusal: the render is legal, it just does not show the model what the caller thinks it
/// does. The engine warns in `plan.warnings` with the number it will actually see.
pub fn h3_max_ref_video_frames(generation_frames: u32) -> u32 {
    generation_frames
}

/// What the model will actually be shown, given a reference of `frames` inside a generation of
/// `generation_frames`: truncate to the generation, then snap DOWN to the `5+17k` grid.
///
/// 🔴 ORDER IS LOAD-BEARING and mirrors `nodes_minimax_h3.py:245-252`. Snapping first and
/// truncating second gives a different answer whenever the generation length is itself off-grid.
/// Returns 0 for anything under [`H3_MIN_REF_VIDEO_FRAMES`], which the engine refuses outright.
pub fn h3_effective_ref_video_frames(frames: u32, generation_frames: u32) -> u32 {
    let mut n = frames.min(generation_frames.max(1));
    while n > 0 && n % H3_FRAME_PERIOD != H3_FRAME_BASE {
        n -= 1;
    }
    n
}

/// The tail lengths an editor should OFFER: `5 + 17k` within `[56, 345]`. **Guidance, not a rule.**
///
/// 🔴 **NO LONGER ENFORCED ANYWHERE — 2026-08-21.** koblem answered 400 to any `tail_frames`
/// off this grid, because WanGP's `_add_video_reference` truncated to the grid keeping the FIRST
/// frames and the model was then told a lie about time. On the ComfyUI path `tail_frames` reaches
/// no node at all: the engine validates only that it is on a video reference, spends it in the ETA
/// row estimate, and publishes `references.tail_frames.applied = false`. A reference's real length
/// is decided from the FILE — truncated to the generation, then snapped down to `5+17k` (see
/// [`h3_effective_ref_video_frames`]).
///
/// The range survives as a PICKER: a tail on the grid is the one whose ETA is right and whose
/// meaning survives if the field is ever wired again. The floor of 56 is the weakest part of it —
/// it was 2.0 s at 24 fps out of the same dead WanGP validator, and the node's real floor is 5
/// frames — so treat the low end as taste, not law.
pub fn h3_legal_tails() -> impl Iterator<Item = u32> {
    (56..=345).step_by(H3_FRAME_PERIOD as usize)
}

/// Is `n` a legal tail length? See [`h3_legal_tails`].
pub fn h3_is_legal_tail(n: u32) -> bool {
    (56..=345).contains(&n) && (n - H3_FRAME_BASE).is_multiple_of(H3_FRAME_PERIOD)
}

/// The period of the video VAE's `(1, 4, 4, 4, 4)` token cycle, in LATENT TOKENS.
///
/// 🔴 It is 5 and so is [`H3_FRAME_BASE`], and they are **not the same quantity**. Reusing the
/// frame constant for the carry's phase rule type-checks, reads plausibly, and is wrong: it
/// rejected `k=7` off a 345-frame shot (a carry the engine accepts) and offered `k=5` instead
/// (one it refuses). Latent arithmetic uses this; frame arithmetic uses `H3_FRAME_BASE`.
pub const H3_LATENT_CYCLE: u32 = 5;
/// The continuation node's own `k` bounds (`nodes_minimax_h3.py`, `io.Int.Input("k", …)`).
pub const H3_CONTINUATION_K_MIN: u32 = 2;
/// See [`H3_CONTINUATION_K_MIN`].
pub const H3_CONTINUATION_K_MAX: u32 = 64;

/// `video_latent_t` — the latent tokens the video VAE produces for `frames` pixel frames.
///
/// Mirror of the backend's `plugins/h3/contract.py:latents_for_frames`, which is itself
/// `nodes_minimax_h3.py:39-40` via `grid.latents_for_frames`. The two must agree: it is the
/// unit every continuation rule is stated in.
pub fn h3_latents_for_frames(frames: u32) -> u32 {
    if frames <= H3_FRAME_BASE {
        2
    } else {
        (frames - H3_FRAME_BASE) / H3_FRAME_PERIOD * H3_LATENT_CYCLE + 2
    }
}

/// May a carry of `k` latent tokens be sliced off the tail of a `prev_frames`-frame generation?
///
/// 🔴 THE PHASE RULE, and it is fatal: `(T_prev - k) % 5 == 0` where **`T_prev` is in LATENT
/// TOKENS**, not pixel frames. An off-phase slice lands on the wrong phase of the
/// `(1, 4, 4, 4, 4)` token cycle and the model is told a lie about time.
///
/// Because [`h3_latents_for_frames`] is `≡ 2 (mod 5)` at every legal frame count, the legal
/// depths are `{2, 7, 12, 17, …}` for EVERY shot length — the frame count only bounds how deep
/// the carry may go, it never changes the phase. That is why the default `k = 7` is always
/// legal, and why a validator that tests the pixel count instead accepts and refuses exactly the
/// wrong set.
pub fn h3_carry_is_on_phase(prev_frames: u32, k: u32) -> bool {
    let t_prev = h3_latents_for_frames(prev_frames);
    (H3_CONTINUATION_K_MIN..=H3_CONTINUATION_K_MAX).contains(&k)
        && k <= t_prev
        && (t_prev - k).is_multiple_of(H3_LATENT_CYCLE)
}

/// Every carry depth a `prev_frames`-frame generation admits. See [`h3_carry_is_on_phase`].
pub fn h3_legal_carry_depths(prev_frames: u32) -> Vec<u32> {
    let hi = h3_latents_for_frames(prev_frames).min(H3_CONTINUATION_K_MAX);
    (H3_CONTINUATION_K_MIN..=hi)
        .filter(|k| h3_carry_is_on_phase(prev_frames, *k))
        .collect()
}

/// Round a generation length UP onto H3's `17n+5` grid, clamped into
/// [[`H3_MIN_FRAMES`], [`H3_MAX_FRAMES`]].
///
/// Shared rather than duplicated per caller because the direction matters and is easy to get
/// wrong: the engine snaps UP, so a generation authored at 5.0 s (120 frames) renders as 124.
/// A client that rounds the other way disagrees with the engine about the duration of every
/// render in the timeline, and on H3 that means every timed audio cue the prompt places lands
/// late by an accumulating amount.
///
/// The CLAMP is at [`H3_MIN_FRAMES`] (22 — the VAE's own decodable floor, below which there is
/// genuinely no render to offer) and [`H3_MAX_FRAMES`]. Over the cap the caller has asked for
/// something the model cannot produce in one generation — which is the packer's problem, not
/// this function's, and the packer is what stops it happening.
///
/// 🔴 The floor used to be 107 and lengthened a 2-second beat to 4.458 s **silently**. It no
/// longer does. Anything under [`H3_DOCUMENTED_MIN_FRAMES`] is outside MiniMax's documented
/// 4-15 s range and callers say so once — see `H3PromptComposer.svelte` and the shim's plan
/// warnings — but it is not rewritten here.
pub fn h3_snap_frames(n: u32) -> u32 {
    let n = n.max(H3_MIN_FRAMES);
    let snapped = H3_FRAME_BASE + (n - H3_FRAME_BASE).div_ceil(H3_FRAME_PERIOD) * H3_FRAME_PERIOD;
    snapped.min(H3_MAX_FRAMES)
}

/// Frames for a duration in seconds, on the grid. 24 fps is not a parameter — it is the only
/// rate the model is defined at.
///
/// 🔴 This is THE definition. `routes/ltx_director.rs` used to re-derive it and the two could
/// drift; it now calls this.
pub fn h3_frames_from_seconds(seconds: f64) -> u32 {
    h3_snap_frames((seconds.max(0.0) * H3_FPS as f64).round() as u32)
}

/// The duration a generation will ACTUALLY render at, once its frames are snapped.
///
/// Callers want this rather than the authored length because on H3 the prompt itself carries
/// timings — a shot list in timecoded blocks, audio cues at named seconds — so anything
/// working from the authored number places its last cue past the end of the render.
pub fn h3_seconds_from_frames(frames: u32) -> f64 {
    frames as f64 / H3_FPS as f64
}

/// `frames_for_latents` — the pixel frames a `t`-token latent block decodes to.
///
/// Inverse of [`h3_latents_for_frames`] on the legal `17j+5` grid, and a mirror of the backend's
/// `plugins/h3/contract.py::_frames_for_latent_t`. This is the unit a continuation trim is
/// measured in: an on-target carry overlays `frames_for_latents(k)` of the target's OWN first
/// frames and the trim node removes exactly that many.
pub fn h3_frames_for_latents(t: u32) -> u32 {
    if t <= 2 {
        H3_FRAME_BASE
    } else {
        (t - 2) / H3_LATENT_CYCLE * H3_FRAME_PERIOD + H3_FRAME_BASE
    }
}

/// Head frames an on-target continuation at depth `k` loses at delivery. See [`H3_TRIM_JOIN`].
pub fn h3_trim_join(k: u32) -> u32 {
    h3_frames_for_latents(k)
}

// ── Drift protection: the constants above vs. the engine that actually renders ───────────────
//
// 🔴 WHY THIS EXISTS. Every number above is a SECOND COPY of something `/h3/v1/capabilities`
// already publishes, kept only because validation happens at submit with no engine handy and
// because the editor has to draw a form when the engine is down. A second copy of a number is a
// number that drifts, and the drift is SILENT in the direction that matters: a client cap BELOW
// the engine's is not an error anywhere — the request is simply refused by us before the engine
// ever sees it, and the user is told a limit that does not exist.
//
// That is not hypothetical. `H3_MAX_VIDEO_REFS`/`H3_MAX_AUDIO_REFS` sat at 2 for the nine days
// after ComfyUI replaced WanGP on 2026-08-12, refusing a third reference the engine had been
// accepting the whole time, and nothing anywhere said the word "stale".
//
// The guard is deliberately NOT a unit test against a checked-in capabilities snapshot: a snapshot
// is a third copy, and it goes stale the same way for the same reason. It is a pure function over
// the LIVE document koblem already fetches on `/api/v1/minimax-h3/props`, so it is exercised in
// production against the engine that is really running, and it is unit-testable without one
// because it takes the document as an argument.

/// Every H3 constant the engine ALSO publishes, as `(constant name, JSON Pointer, our value)`.
///
/// The pointer is into a `/h3/v1/capabilities` body. Constants deliberately ABSENT, and why:
///
/// * [`H3_SOUNDTRACK_SENTINEL`], [`H3_SOUNDTRACK_DEFAULT_DUCK_DB`], [`H3_MAX_FRAMES_WITH_AUDIO`] —
///   **product policy, not wire facts.** The engine has no opinion about soundtracks; koblem
///   invented the sentinel, the ducking depth, and the 345-frame ceiling that keeps a generation
///   no longer than the audio slice it is allowed to carry. There is nothing to drift against.
/// * [`H3_CONTINUATION_FROM_PREV`] and the tail grid ([`h3_legal_tails`]) — a string and a
///   sequence, not scalars. A mismatch in either is a 400 at submit, which is loud already.
/// * [`H3_MIN_AUDIO_REF_SECONDS`] / [`H3_MAX_AUDIO_REF_SECONDS`] — **koblem's soundtrack-slicing
///   policy, and the engine has no audio-reference length rule to diff them against.** Putting
///   them in this table would have manufactured a wire fact out of a product judgement, which is
///   the exact mistake that put a WanGP number here in the first place.
/// * The reference-video CEILING — it is the generation's own frame count, not a constant. See
///   [`h3_max_ref_video_frames`]. Only the 5-frame floor is a scalar worth diffing.
///
/// 🔴 **THESE POINTERS ARE A WIRE CONTRACT WITH A PYTHON TREE THAT CANNOT COMPILE THIS ONE.**
/// Rename a field on the engine and this does not report a mismatch — it reports `unpublished`,
/// falls back to our own copy, and we are back to two numbers with no referee.
/// `api/tests/test_service.py::test_capabilities_publishes_every_number_koblem_hardcodes` is the
/// guard on the other side; move the two together.
pub const H3_CAPABILITY_MIRRORS: &[(&str, &str, f64)] = &[
    ("H3_FPS", "/fps", H3_FPS as f64),
    ("H3_FRAME_BASE", "/frames/base", H3_FRAME_BASE as f64),
    ("H3_FRAME_PERIOD", "/frames/period", H3_FRAME_PERIOD as f64),
    ("H3_MIN_FRAMES", "/frames/min", H3_MIN_FRAMES as f64),
    (
        "H3_DOCUMENTED_MIN_FRAMES",
        "/frames/documented_min",
        H3_DOCUMENTED_MIN_FRAMES as f64,
    ),
    ("H3_MAX_FRAMES", "/frames/max", H3_MAX_FRAMES as f64),
    (
        "H3_DIM_MULTIPLE",
        "/canvas/dim_multiple",
        H3_DIM_MULTIPLE as f64,
    ),
    (
        "H3_ADVISORY_MAX_PIXELS",
        "/canvas/max_pixels",
        H3_ADVISORY_MAX_PIXELS as f64,
    ),
    (
        "H3_MAX_PROMPT_CHARS",
        "/prompt/max_chars",
        H3_MAX_PROMPT_CHARS as f64,
    ),
    (
        "H3_MAX_IMAGE_REFS",
        "/references/max_image",
        H3_MAX_IMAGE_REFS as f64,
    ),
    (
        "H3_MAX_VIDEO_REFS",
        "/references/max_video",
        H3_MAX_VIDEO_REFS as f64,
    ),
    (
        "H3_MAX_AUDIO_REFS",
        "/references/max_audio",
        H3_MAX_AUDIO_REFS as f64,
    ),
    (
        "H3_MAX_TOTAL_REFS",
        "/references/max_total",
        H3_MAX_TOTAL_REFS as f64,
    ),
    (
        "H3_REF_RELATIVE_SIZE_MIN",
        "/references/image_refs_relative_size/min",
        H3_REF_RELATIVE_SIZE_MIN as f64,
    ),
    (
        "H3_REF_RELATIVE_SIZE_MAX",
        "/references/image_refs_relative_size/max",
        H3_REF_RELATIVE_SIZE_MAX as f64,
    ),
    (
        "H3_MIN_REF_VIDEO_FRAMES",
        "/references/ref_video_frames/min",
        H3_MIN_REF_VIDEO_FRAMES as f64,
    ),
    (
        "H3_CONTINUATION_K_MIN",
        "/continuation/k/min",
        H3_CONTINUATION_K_MIN as f64,
    ),
    (
        "H3_CONTINUATION_K_MAX",
        "/continuation/k/max",
        H3_CONTINUATION_K_MAX as f64,
    ),
    (
        "H3_CONTINUATION_K_DEFAULT",
        "/continuation/k/default",
        H3_CONTINUATION_K_DEFAULT as f64,
    ),
    (
        "H3_LATENT_CYCLE",
        "/continuation/latent_cycle",
        H3_LATENT_CYCLE as f64,
    ),
    (
        "H3_TRIM_JOIN",
        "/continuation/trim_join/frames_at_default_k",
        H3_TRIM_JOIN as f64,
    ),
];

/// What [`h3_capability_drift`] found.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct H3CapabilityDrift {
    /// The engine publishes a DIFFERENT number than we compiled in. **This is a bug in one of the
    /// two trees and somebody has to move.** A client cap below the engine's silently withholds a
    /// capability; above it, a request is accepted here and refused there after the queue wait.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub mismatched: Vec<String>,
    /// The engine publishes no value at that pointer, so the constant is UNVERIFIABLE against the
    /// thing that is really rendering. Expected transiently when koblibs lands ahead of the engine
    /// image; permanent means the pointer names a field that no longer exists.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unpublished: Vec<String>,
}

impl H3CapabilityDrift {
    pub fn is_clean(&self) -> bool {
        self.mismatched.is_empty() && self.unpublished.is_empty()
    }
}

/// Compare every entry in [`H3_CAPABILITY_MIRRORS`] against a live `/h3/v1/capabilities` body.
///
/// Pure: hand it a document, get back what disagrees. No engine needed to test it.
pub fn h3_capability_drift(caps: &Value) -> H3CapabilityDrift {
    let mut out = H3CapabilityDrift::default();
    for &(konst, ptr, ours) in H3_CAPABILITY_MIRRORS {
        match caps.pointer(ptr).and_then(|v| v.as_f64()) {
            Some(live) if (live - ours).abs() > 1e-9 => out.mismatched.push(format!(
                "{konst}: we compiled in {ours}, the engine publishes {live} at `{ptr}`"
            )),
            Some(_) => {}
            None => out
                .unpublished
                .push(format!("{konst}: the engine publishes nothing at `{ptr}`")),
        }
    }
    out
}

/// Read a limit LIVE, falling back to the compiled-in constant when the engine is unreachable or
/// too old to publish it. `ptr` is a JSON Pointer into a capabilities body.
///
/// 🔴 **Prefer this over the bare constant anywhere the answer is handed to a UI.** The
/// constants exist so that request validation and an offline editor have an answer at all; they
/// are not the authority, and a props document quoting them BESIDE the engine's own capabilities
/// block is how the two came to disagree in the first place. Validation itself still uses the
/// constants — it runs at submit with nothing fetched — which is exactly why
/// [`h3_capability_drift`] has to be logged loudly rather than merely returned.
///
/// A negative or non-integral live value is nonsense from a broken engine, not a limit, and falls
/// back too.
pub fn h3_cap_u64_or(caps: Option<&Value>, ptr: &str, fallback: u64) -> u64 {
    caps.and_then(|c| c.pointer(ptr))
        .and_then(|v| v.as_f64())
        .filter(|v| *v >= 0.0 && v.fract() == 0.0)
        .map(|v| v as u64)
        .unwrap_or(fallback)
}

// ── The structured prompt draft ─────────────────────────────────────────────
//
// The wire shape between koblem's LLM endpoints and the Director document, defined by
// `kobbler/docker/h3-wangp/PROMPT-CONTRACT.md` §2. One shared data model, three consumers:
// `routes::h3_prompting` teaches the LLM to FILL IT IN, `web/src/lib/director/h3-prompt.ts`
// ASSEMBLES it into the final schema block, and `H3PromptComposer` is a section-per-panel EDITOR
// over it.
//
// 🔴 Why this is a type and not a `String`. H3's prompt IS a schema the model was trained on, and
// bare prose is out-of-distribution conditioning that — the part that actually costs picture
// quality — supplies **no `overall_soundscape` and no `non_diegetic_music` at all**, leaving the
// model to invent the whole audio bed unguided. On the engine whose headline feature is joint
// video+audio, that is the largest capability on the table. So the LLM returns FIELDS, and only
// the composer turns them into a block.
//
// It was riding as its own compact JSON *inside* the response's `String` field, which the client
// then had to `JSON.parse`. That worked and would have rotted: a renamed field is a runtime
// surprise on both sides instead of a compile error on either.
//
// Round-tripping an assembled block back into these fields is explicitly NOT required. The block
// is the artifact; this is the source it is assembled from.

/// A Ref2VA task type, for the `summary:` bracket prefix (`"[video continuation + audio
/// reference]"`). Composed with `" + "`, never repeated.
///
/// The string values are English literals the model was trained on, not identifiers — which is
/// why they carry spaces and why this is a closed set rather than free text. ⚠️ No ordering rule
/// is stated anywhere in the corpus and the two official examples order them differently, so the
/// canonical order is ours (`SPEC-h3-prompt-emitter.md` §K, row 7).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum H3TaskType {
    #[serde(rename = "keyframe completion")]
    KeyframeCompletion,
    #[serde(rename = "reference generation")]
    ReferenceGeneration,
    #[serde(rename = "video editing")]
    VideoEditing,
    #[serde(rename = "video continuation")]
    VideoContinuation,
    #[serde(rename = "audio reuse")]
    AudioReuse,
    #[serde(rename = "audio reference")]
    AudioReference,
}

impl H3TaskType {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::KeyframeCompletion => "keyframe completion",
            Self::ReferenceGeneration => "reference generation",
            Self::VideoEditing => "video editing",
            Self::VideoContinuation => "video continuation",
            Self::AudioReuse => "audio reuse",
            Self::AudioReference => "audio reference",
        }
    }
}

/// One `subject_definitions` line.
///
/// `label` is assigned by the COMPOSER from the reference's position, never typed by the model —
/// it is carried here only so the model can address it in the surrounding prose. See
/// `PROMPT-CONTRACT.md` §4: the ordinals follow WanGP's pipeline add-order, and a label that does
/// not match what the conditioner emitted simply does not resolve.
///
/// 🔴 The angle brackets are PART OF THE PROMPT. Official prompts write `<Picture 1>`,
/// `<Subject 1>`, `<Video 1>` verbatim and it is the same string `text_encoder.py:137` emits.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct H3SubjectDefinition {
    /// `<Subject 1>`, `<Video 1>`, `<Audio 1>` — brackets included.
    #[serde(default)]
    pub label: String,
    #[serde(default)]
    pub text: String,
}

/// One `retention_analysis` line: `<Label> (role note): marker - text`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct H3RetentionEntry {
    #[serde(default)]
    pub label: String,
    /// The parenthesised scope note — `appears in [Shot 1]`, `continuation starting point`.
    /// Audio lines carry none at all (`GUIDE-ref:200-204`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role_note: Option<String>,
    /// 🔴 A `String` rather than an enum because the two vocabularies are **disjoint by label
    /// kind** and a single enum would have to union them, which is exactly the check worth
    /// keeping: visible assets (`<Subject N>` / `<Picture N>` / `<Video N>`) take
    /// `fully_preserved | partially_preserved | attribute_transfer | weak_reference`, audio
    /// (`<Audio N>`) takes `fully_copy | partially_copy | reference | weak_reference`.
    /// `<Audio N>: partially_preserved` is a schema error, not a stylistic slip, and it is caught
    /// where the label kind is known (`lib/director/h3-prompt.ts`).
    #[serde(default)]
    pub marker: String,
    /// The explanation after the hyphen.
    #[serde(default)]
    pub text: String,
}

/// One `[Shot N]` body inside a generation.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct H3DraftShot {
    /// 0-based within the generation. `[Shot 1]` is index 0.
    #[serde(default)]
    pub index: u32,
    /// `None` on index 0, which is untimed. Later shots carry an absolute cut time, strictly
    /// increasing, inside the generation's duration.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub at_seconds: Option<f64>,
    /// The body text ONLY — no `[Shot N]` tag, no timestamp, no section header. The composer
    /// writes those, because it is the only thing that knows the generation's real duration and
    /// therefore whether a cut time is even legal.
    #[serde(default)]
    pub body: String,
}

/// What the LLM returns for ONE generation. `PROMPT-CONTRACT.md` §2.
///
/// Every field is optional except `shots` — an omitted field means "a human or a later pass fills
/// it in", which is what makes the section-per-panel editor a real editor rather than a viewer of
/// something already finished.
///
/// The Ref2VA-only block at the top is genuinely Ref2VA-only: which schema applies is decided by
/// the CHECKPOINT, which is decided by the references, and using the wrong main field is a schema
/// error rather than a style choice.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct H3PromptDraft {
    // ── Ref2VA only ──────────────────────────────────────────────────────────
    /// Composed with `" + "` into the `summary:` bracket prefix.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub task_types: Vec<H3TaskType>,
    /// One entry per `<Subject N>`.
    ///
    /// An image used only for identity or style gets NO standalone `<Picture N>` retention entry
    /// — it is cited inside the `<Subject N>` definition that uses it (`GUIDE-ref:68`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub subject_definitions: Vec<H3SubjectDefinition>,
    /// The summary paragraph, prose only — the composer prepends the `[task types]` bracket.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub retention_analysis: Vec<H3RetentionEntry>,

    // ── both schemas ─────────────────────────────────────────────────────────
    /// One or two sentences of style.
    ///
    /// 🔴 Where it goes FLIPS with the checkpoint: Ref2VA writes it on its own line BEFORE
    /// `[Shot 1]` inside `detailed_description`; the FL2VA family writes it AFTER the `[Shot 1]`
    /// tag, inside the shot. Stored once here, placed by the composer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub style_opening: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub shots: Vec<H3DraftShot>,
    /// 1-4 sentences, ONE paragraph: ambience, physical action sounds, non-verbal human sounds.
    /// Not dialogue, not diegetic music. `"N/A"` only for requested total silence.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub overall_soundscape: Option<String>,
    /// 1-3 sentences of audience-only score: instrumentation, tempo, rhythm, dynamics. `"N/A"` if
    /// none.
    ///
    /// 🔴 The LAST section, and the reason the 7000-character prompt cap REJECTS instead of
    /// truncating: a truncated prompt drops this entire field and the render merely sounds wrong.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub non_diegetic_music: Option<String>,
}

impl H3PromptDraft {
    /// Did the model actually fill this in, or is it the empty shell a failed parse leaves?
    ///
    /// Keyed on `shots` because that is the one field `PROMPT-CONTRACT.md` §2 makes mandatory: a
    /// draft with sections but no shot bodies has nothing to assemble a `detailed_description`
    /// from, so it is not a partial draft — it is a parse that did not happen.
    pub fn is_empty(&self) -> bool {
        self.shots.iter().all(|s| s.body.trim().is_empty())
    }
}

/// `MM:SS.mmm`, the cut-time format the prompt's `[Shot N] At …` markers use.
///
/// Formatted here rather than at each call site because the model parses it: `4.5` seconds is
/// `00:04.500` and not `0:4.5`, and a marker it cannot parse is a cut that does not happen —
/// with no error anywhere, because the text simply reads as prose.
pub fn h3_format_cut_time(seconds: f64) -> String {
    let seconds = seconds.max(0.0);
    let total_ms = (seconds * 1000.0).round() as u64;
    let minutes = total_ms / 60_000;
    let secs = (total_ms % 60_000) / 1000;
    let millis = total_ms % 1000;
    format!("{minutes:02}:{secs:02}.{millis:03}")
}

/// One progressively-delivered unit, served before the stitched clip.
///
/// LTX-2.3 and MiniMax-H3 only — the 2B and Wan engines never emit partials. ⚠️ The UNIT differs:
/// on LTX it is one shot, on H3 it is one whole GENERATION (1-N shots in a single render). The
/// field name is `segment_index` on both because everything downstream — the store, the harvest,
/// the Director's take rows — indexes by it and does not otherwise need to know.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VideoJobPartial {
    /// Engine unit index (0-based, in the engine's filtered render order).
    pub segment_index: u32,
    /// koblem-served URL for the persisted unit (`…/jobs/{id}/segments/{n}`).
    pub url: String,
}

/// `GET /api/v1/{ltx-video,ltx-2b,wan-vace}/jobs/{id}`.
///
/// On LTX there is no numeric progress: state transitions plus
/// `created_at`/`gate_wait_ms`/`render_ms` and `partials` are the whole signal. **MiniMax-H3 does
/// have one** — see the progress block at the bottom — because WanGP surfaces per-step callbacks
/// the LTX stack has never had. Every field of it is `None` on an LTX job and always will be.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VideoJobStatus {
    pub job_id: String,
    pub status: VideoJobState,
    /// When koblem accepted the job. A client attaching to a render started on another
    /// device measures elapsed time from here — without it, elapsed restarts at zero on
    /// every adoption and the progress bar and ETA under-report for the rest of the run.
    ///
    /// `None` from a koblem predating this field, or for a stored timestamp outside the
    /// representable range. Also the explicit ordering key behind the job list's
    /// newest-first contract.
    #[serde(
        default,
        with = "time::serde::rfc3339::option",
        skip_serializing_if = "Option::is_none"
    )]
    pub created_at: Option<time::OffsetDateTime>,
    /// The render engine's own job id. Pass back as
    /// [`LtxVideoGenerateRequest::resume_job_id`] to continue a failed render
    /// from its banked segments.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server_job_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resume_job_id: Option<String>,
    #[serde(default)]
    pub resumable: bool,
    #[serde(default)]
    pub segments: u32,
    #[serde(default)]
    pub scripts: Vec<String>,
    #[serde(default)]
    pub gate_wait_ms: u64,
    #[serde(default)]
    pub render_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// `…/jobs/{id}/media`. Set once the clip exists.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result_url: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub partials: Vec<VideoJobPartial>,
    /// 🔴 **`result_url` IS NOT THE TIMELINE.** Two unrelated situations set it, and the client's
    /// action is the same for both: never adopt these bytes as the project's stitched cut.
    ///
    ///   * a resume or a retake — the media is the RENDERED WINDOW. A client can predict this one
    ///     from the engine's capabilities at dispatch, a poll before any job body arrives.
    ///   * a full render whose concatenation FAILED — the media is segment 0 alone. A runtime
    ///     failure, so no capability predicts it; this flag is the only warning a client gets.
    ///
    /// Emitted only when `true`, so an unaffected job's body is unchanged and a koblem predating
    /// the field reads as `false` — which every one of its jobs was.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub job_is_partial: bool,

    // ── MiniMax-H3 progress ──────────────────────────────────────────────────
    //
    // All optional and all skipped when absent, so an LTX job's body is byte-identical to what it
    // was and no existing client sees a new key. A client that wants a progress bar reads these
    // and treats absence as "this engine does not report progress", never as zero.
    /// The engine's ETA for the WHOLE job at submit time, including any checkpoint reload a mixed
    /// fl2va/ref2va chain forces. The number worth showing before an hour-long render starts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_eta_seconds: Option<f64>,
    /// 0.0-1.0 across the whole job.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub progress: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_step: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_steps: Option<u32>,
    /// `denoising` | `decoding` | `loading` | `encoding`. `loading` is the full DiT +
    /// text-encoder reload a chain pays when it switches checkpoint family, and is the difference
    /// between a stuck render and a 20 GB swap.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phase: Option<String>,
    /// Bumps when a new preview frame is available at
    /// `/api/v1/minimax-h3/jobs/{id}/preview`. A sequence and not a URL, because the URL never
    /// changes — a client watches the number to know when to re-fetch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preview_seq: Option<u64>,
    /// Which generation is rendering. `segments` holds how many there are.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub generation_index: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub generations_done: Option<u32>,
}

/// `GET /api/v1/{ltx-video,ltx-2b,wan-vace}/jobs`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct VideoJobListResponse {
    #[serde(default)]
    pub jobs: Vec<VideoJobStatus>,
}

/// HTTP 202 body from a `generate`/`relip` submit.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VideoJobSubmitResponse {
    pub job_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<VideoJobState>,
}

/// One established character or mood-board entry the director LLM must treat as canon.
/// Steers the writing (identity, look, palette) — it is NOT rendered as a scene frame.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct StoryReference {
    /// Short name/tag, e.g. "ARIA-7 (robot)" or "mood: rainy neon".
    #[serde(default)]
    pub label: String,
    /// Free-text identity notes the LLM must honour (appearance, personality, palette).
    #[serde(default)]
    pub description: String,
    /// Optional bare (or `data:`-prefixed) base64 image — a character/mood reference the VL
    /// model studies. LLM-only: never injected into the video as a frame.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image_b64: Option<String>,
}

/// Project-level "story bible" fed as canonical grounding into every director LLM call
/// (auto-direct + per-shot prompt drafting) so shots don't hallucinate the scenario or a
/// character's details. Persisted per-project; kept separate from the generated per-shot
/// prompts so the user's direction is never overwritten.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct StoryContext {
    /// World / scenario / plot — the canon the LLM builds every shot from.
    #[serde(default)]
    pub backstory: String,
    /// Overall mood, visual style, colour palette.
    #[serde(default)]
    pub tone: String,
    /// Hard always/never constraints.
    #[serde(default)]
    pub rules: String,
    /// Characters + mood-board entries.
    #[serde(default)]
    pub references: Vec<StoryReference>,
}

impl StoryContext {
    /// True when nothing is set — callers skip injection entirely.
    pub fn is_empty(&self) -> bool {
        self.backstory.trim().is_empty()
            && self.tone.trim().is_empty()
            && self.rules.trim().is_empty()
            && self.references.iter().all(|r| {
                r.label.trim().is_empty()
                    && r.description.trim().is_empty()
                    && r.image_b64.is_none()
            })
    }
}

/// `POST /api/v1/ltx-video/script-from-llm` — koblem asks the main LLM to expand a
/// brief (and/or a set of lines) into N per-segment director prompts, each ~seconds long.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LtxScriptFromLlmRequest {
    pub brief: String,
    /// How many segments to produce.
    pub n_segments: u32,
    /// Target seconds per segment (informs the per-beat action density). None → engine default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seconds_per_segment: Option<f64>,
    /// Bare base64 opening frame (no `data:` prefix). Present → image-to-video: the VL model
    /// sees the first frame and writes shots that continue from it. Absent → text-to-video.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image: Option<String>,
    /// The user is supplying their own audio (voice / bgm / mixed / tts), so the prompts must
    /// NOT describe sound. False → LTX generates its own audio and the prompts should weave the
    /// soundscape into each shot.
    #[serde(default)]
    pub audio_supplied: bool,
    /// Project canon (backstory / characters / tone / rules) grounding the shots so the LLM
    /// doesn't invent scenario or character details.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub story: Option<StoryContext>,
    /// The project's recurring cast, with the appearance sentence each character must carry
    /// verbatim into every shot they appear in. Empty ⇒ byte-identical to before.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub cast: Vec<LtxCastMember>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LtxScriptFromLlmResponse {
    /// One director prompt per segment.
    ///
    /// ⚠️ On the H3 arm this is the LEGACY carrier: each entry is the compact JSON of the matching
    /// [`h3_generations`](Self::h3_generations) entry, kept so a client that has not moved to the
    /// structured field yet still works. Delete it from the H3 path once nothing reads it. On LTX
    /// it is, and remains, the prose paragraph.
    pub segments: Vec<String>,
    /// **H3 only.** One structured draft per GENERATION (`PROMPT-CONTRACT.md` §2), index-aligned
    /// with `segments`.
    ///
    /// All-or-nothing, deliberately: it is populated only when EVERY generation parsed, so a
    /// reader never has to work out whether entry 3 is missing or merely index 3 of a shorter
    /// list. Empty means "fall back to `segments`", which is what an LTX response and a
    /// contract-ignoring model both produce.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub h3_generations: Vec<H3PromptDraft>,
}

/// One image handed to the per-shot prompt writer, tagged with its role + position so the VL
/// model can describe the transition (an opening frame, a mid-shot keyframe/reveal, a final frame).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LtxPromptImage {
    /// Bare (or `data:`-prefixed) base64 of the image.
    pub image_b64: String,
    /// Role label shown to the model: "opening" | "scene" | "keyframe" | "end" (free-form).
    #[serde(default)]
    pub role: String,
    /// Time within the shot (seconds) this image sits at, when positional (keyframes).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seconds: Option<f64>,
}

/// `POST /api/v1/ltx-video/prompt-from-llm` — write ONE detailed LTX shot prompt for a single
/// prompt field. Image-aware (scene / keyframes with positions) and mode-aware (t2v vs i2v).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LtxPromptFromLlmRequest {
    /// Optional seed idea / the field's current text to build on. Empty ⇒ invent from images/mode.
    #[serde(default)]
    pub idea: String,
    /// Shot mode (drives t2v vs i2v guidance). `None`/`Auto` ⇒ inferred from whether images attach.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<LtxSegmentMode>,
    /// Target shot length (seconds); the prompt's detail is scaled to fill it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seconds: Option<f64>,
    /// FPS (carried for symmetry / future frame hints; unused server-side today).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fps: Option<u32>,
    /// The user supplies their own audio ⇒ the prompt must not narrate sound.
    #[serde(default)]
    pub audio_supplied: bool,
    /// Whether anyone is audibly speaking or singing DURING this shot, read off the transcript
    /// lane's word onsets. `Some(false)` ⇒ the shot sits in a gap between lines or over
    /// instrumental/ambient audio, and the prompt must not put a moving mouth on anyone — writers
    /// otherwise reach for "singing" as generic liveliness and the render lip-syncs to silence.
    /// `None` ⇒ no transcript to judge from; say nothing rather than guess.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub speech_in_shot: Option<bool>,
    /// Whether THIS shot renders from the previous shot's final frame (a plain continuation) rather
    /// than fresh. `true` ⇒ treat it as image-to-video even without an attached image: the prior
    /// frame is already the opening frame, so describe only what changes, not the standing scene.
    /// `false`/absent ⇒ shot 0, a hard cut, or an i2v/v2v shot that starts from its own source.
    #[serde(default)]
    pub continuation: bool,
    /// Whether this shot explicitly CLOSES its scene at the END — `true` ⇒ nothing continues out of
    /// its final frame (the next shot is a hard cut, or it is a deliberate final beat), so it may
    /// close on any framing. `false` ⇒ the next shot continues from its final frame (or none exists
    /// yet while building), so it must end on a frame still showing the whole scene — that frame is
    /// the only thing the next shot can see. `None` ⇒ unknown; no end-framing guidance either way.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub closes_scene: Option<bool>,
    /// Attached images (scene / keyframes) with positional roles for the transition.
    #[serde(default)]
    pub images: Vec<LtxPromptImage>,
    /// Per-shot visual references to MATCH (framing / composition / lighting / mood). Unlike
    /// `images` these are NOT frames of this shot and are never rendered — they steer the writing.
    #[serde(default)]
    pub references: Vec<LtxPromptImage>,
    /// Neighbouring shot prompts for continuity (optional).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prev_prompt: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_prompt: Option<String>,
    /// Compact summaries of every shot BEFORE this one (earliest → latest) so a draft/reroll knows
    /// the story it is continuing. One short plain-language line per shot; the immediate neighbour's
    /// full text still rides in `prev_prompt` for tight continuity.
    #[serde(default)]
    pub prior_shots: Vec<String>,
    /// Reroll: an existing full prompt to rewrite (keep the scene/subject/action/intent, vary the
    /// wording and framing). When set it replaces `idea` as the thing being written.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rewrite: Option<String>,
    /// Rework: a directed change to apply ("make her angry", "move to night", a continuity note).
    /// Combined with `rewrite`/`idea` it revises the shot toward the instruction while holding
    /// everything the instruction doesn't touch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instruction: Option<String>,
    /// This shot's Prompt Relay beats, if it has any — the timed moments that are conditioned
    /// SEPARATELY and must therefore be left OUT of the prompt being written.
    ///
    /// Without this the two halves of the authoring pipeline fight each other: the planner decides
    /// "drink at 0.5s, turn at 2.5s, exit at 4.2s" as beats, and a beat-unaware draft then narrates
    /// all three into the prompt as well. That is precisely the case relay exists to remove — the
    /// prompt has to stay the SETTING, because it is the zero-penalty global anchor conditioning
    /// every frame, and duplicating the beats in it re-averages exactly what the beats separated.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub beats: Vec<LtxBeat>,
    /// Also return a one-sentence plain-language `gist` of the shot alongside the prompt.
    #[serde(default)]
    pub want_gist: bool,
    /// Let the writer PROPOSE Prompt Relay beats for this shot when the idea contains a moment
    /// rather than one continuous action. Ignored unless `beats` is empty — a draft never
    /// overwrites timing the director placed by hand. Off by default, so every existing caller
    /// keeps producing a single flat prompt.
    #[serde(default)]
    pub want_beats: bool,
    /// Produce ONLY the gist for the supplied prompt (`rewrite` or `idea`) — do not (re)write the
    /// prompt. Implies `want_gist`.
    #[serde(default)]
    pub gist_only: bool,
    /// Project canon (backstory / characters / tone / rules) so a lone draft describes the
    /// established subject instead of inventing it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub story: Option<StoryContext>,
    /// The characters that appear in THIS shot, with the appearance sentence each one's prompt must
    /// reproduce verbatim. Empty ⇒ no cast discipline is imposed and the draft is byte-identical to
    /// before. See [`LtxCastMember`].
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub cast: Vec<LtxCastMember>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LtxPromptFromLlmResponse {
    /// ⚠️ On the H3 arm this is the LEGACY carrier — the compact JSON of
    /// [`h3_draft`](Self::h3_draft). See [`LtxScriptFromLlmResponse::segments`].
    #[serde(default)]
    pub prompt: String,
    /// **H3 only.** The structured draft (`PROMPT-CONTRACT.md` §2) for this generation.
    ///
    /// `None` on LTX, and on an H3 reply the model wrote as prose — in which case `prompt` still
    /// carries that prose, so the surface never returns nothing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub h3_draft: Option<H3PromptDraft>,
    /// One-sentence plain-language summary of the shot (no camera/technical jargon). Present only
    /// when the request set `want_gist`/`gist_only`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gist: Option<String>,
    /// Prompt Relay beats the writer proposed for this shot, in shot-relative seconds. Only ever
    /// populated when the request set `want_beats` AND the shot had none — a drafted beat is a
    /// suggestion for the editor to keep or delete, never something that overwrites hand-placed
    /// timing. Empty for a shot that is one uninterrupted action, which is most of them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub beats: Vec<LtxBeat>,
}

/// Request body for `POST /api/v1/ltx-video/relip` — lip-dub / relip: attach a
/// drive-audio track to a source video and re-render the mouths in sync. Posted
/// as multipart with a `video` part (the source clip / frames) and an `audio`
/// part (the drive wav); this JSON rides in the `request` part.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LtxRelipRequest {
    /// Optional mouth-style / scene prompt for the relip pass.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt: Option<String>,
    /// Audio→video guidance. 1.0 = distilled default; the owner pushes up to ~3.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub a2v_guidance: Option<f64>,
    /// Two-stage (coarse→refine) relip. **Off**, on both paths into this struct.
    ///
    /// 🔴 It read "production default ON" while `#[serde(default)]` gave `false`, so the wire and
    /// [`Default`] disagreed — a Rust caller spreading `..Default::default()` got the opposite of
    /// what an absent JSON field got. Both are `false` now, and that is the safe answer on LTX-2.5:
    /// the upscaler crops guides before the refine pass, and `_resolve_upscale`'s conditioned check
    /// looks at `init_image` / `character_refs` / `keyframes` — none of which a relip sets, because
    /// its source arrives as top-level `control_frames`. So the most heavily conditioned request
    /// there is, is the one shape that check does not see.
    #[serde(default)]
    pub two_stage: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub width: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub height: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fps: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seed: Option<i64>,
    /// Persist the relipped clip to the generations gallery.
    #[serde(default = "default_true")]
    pub save_clip: bool,
    /// Strength of the lipdub adapter, `None` → 1.0 (its trained scale).
    ///
    /// Exists because relip moved off the pre-merged `edit` DiT onto the runtime LoRA on 2026-07-30,
    /// and the two are NOT the same sample: with an adapter attached the engine stops folding the
    /// weight-global scalar into the FP4 GEMM alpha and applies it as a graph multiply instead, so
    /// the arithmetic differs. 1.0 matches the multiplier the deleted merge was built at, which makes
    /// it the right default and not a verified optimum — the merge's multiplier was itself never
    /// quality-judged. This is the knob for settling that by eye.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lipdub_strength: Option<f64>,
}

impl Default for LtxRelipRequest {
    fn default() -> Self {
        Self {
            prompt: None,
            a2v_guidance: None,
            two_stage: false,
            width: None,
            height: None,
            fps: None,
            seed: None,
            save_clip: true,
            lipdub_strength: None,
        }
    }
}

// ─── Wan2.2-VACE video ──────────────────────────────────────────────────────
//
// The Wan2.2-VACE general-purpose video engine (T2V / I2V / continuation): a
// prompt (or a per-segment prompt list for continuation) rendered by the warm
// wan-vace video-server into a single stitched clip. koblem posts the render
// request + optional init/reference images to `POST /wan/v1/generate` (returns a
// server job id) and polls `GET /sdcpp/v1/jobs/{id}` to completion — the SAME
// async job contract as the ltx-video-server. The internal VACE tuning flags
// (skip-blocks / strength-tail / anchor-frames) are baked server-side and never
// crossed the wire.

/// Which Wan2.2-VACE render path to run. `Continuation` windows a per-segment
/// prompt list into one long clip (server-side); `T2v`/`I2v` are single prompts.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WanVaceMode {
    #[default]
    T2v,
    I2v,
    Continuation,
}

/// How the Wan pipeline gets an optional image (init frame for i2v, or the
/// identity-anchor reference). `None` = no image for this slot.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum WanImageSource {
    #[default]
    None,
    Upload,
    Url {
        url: String,
    },
}

/// One beat of a Wan continuation script (single prompt for t2v/i2v).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct WanSegment {
    pub prompt: String,
    /// Per-segment length in seconds (UI-facing; koblem maps to frames via fps).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seconds: Option<f64>,
}

/// Request body for `POST /api/v1/wan-vace/generate`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WanVaceGenerateRequest {
    #[serde(default)]
    pub mode: WanVaceMode,
    /// Per-segment prompts (>=1). t2v/i2v use the first; continuation uses all.
    pub segments: Vec<WanSegment>,
    /// Init frame (i2v) — the clip starts from this image.
    #[serde(default)]
    pub init_source: WanImageSource,
    /// Identity-anchor reference image (keeps the subject consistent).
    #[serde(default)]
    pub reference_source: WanImageSource,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub negative_prompt: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub width: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub height: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fps: Option<u32>,
    /// Total frames for a monolithic render (cap ~65; longer → server windowing).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frames: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seed: Option<i64>,
    /// Weight quant: "nvfp4" (default) or "q4_k" (fallback).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quant: Option<String>,

    /// Persist the rendered clip to the generations gallery.
    #[serde(default = "default_true")]
    pub save_clip: bool,

    /// Resume a failed/cancelled render from the server's banked segments.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resume_job_id: Option<String>,
}

impl Default for WanVaceGenerateRequest {
    fn default() -> Self {
        Self {
            mode: WanVaceMode::default(),
            segments: Vec::new(),
            init_source: WanImageSource::default(),
            reference_source: WanImageSource::default(),
            negative_prompt: None,
            width: None,
            height: None,
            fps: None,
            frames: None,
            seed: None,
            quant: None,
            save_clip: true,
            resume_job_id: None,
        }
    }
}

/// `GET /api/v1/wan-vace/props` — UI bootstrap.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct WanVaceProps {
    pub healthy: bool,
    pub default_frames: u32,
    pub default_fps: u32,
    pub default_resolution: String,
    pub default_quant: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_render_sec: Option<f64>,
}

/// `POST /api/v1/wan-vace/script-from-llm` — koblem asks the main LLM to expand a brief
/// (and/or an opening image) into N per-segment continuation prompts. Unlike LTX, the
/// system prompt encodes Wan2.2-VACE CONTINUATION-IDENTITY guidance: every shot must keep
/// the SAME subject/appearance/setting and only continue the motion, because VACE temporal
/// extension drifts to a new subject when per-segment prompts diverge.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct WanScriptFromLlmRequest {
    pub brief: String,
    /// How many segments to produce.
    pub n_segments: u32,
    /// Target seconds per segment (informs the per-beat action density). None → engine default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seconds_per_segment: Option<f64>,
    /// Bare base64 opening/anchor frame (no `data:` prefix). Present → the VL model sees the
    /// subject (i2v init frame or identity-anchor reference) and keeps it across every shot.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct WanScriptFromLlmResponse {
    /// One continuation prompt per segment.
    pub segments: Vec<String>,
}

/// `POST /api/v1/wan-vace/prompt-from-llm` — write ONE detailed Wan 2.2 shot prompt for a single
/// prompt field. Reuses [`LtxPromptImage`] as the shared per-shot image DTO (init frame / anchor).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct WanPromptFromLlmRequest {
    /// Optional seed idea / the field's current text to build on.
    #[serde(default)]
    pub idea: String,
    /// Wan mode (drives t2v vs i2v guidance; continuation adds the identity-continuity clause).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<WanVaceMode>,
    /// Target shot length (seconds); the prompt's motion detail is scaled to fill it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seconds: Option<f64>,
    /// Attached subject images (init frame / identity anchor) fed to the VL model.
    #[serde(default)]
    pub images: Vec<LtxPromptImage>,
    /// Neighbouring shot prompts for continuity (optional).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prev_prompt: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_prompt: Option<String>,
    /// An existing shot prompt to improve in place. With `instruction` set, the
    /// change is applied in place; without one, the prompt is reworded for a
    /// fresh take on the same scene. Same behaviour as the LTX shot drafter.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rewrite: Option<String>,
    /// Free-form directed change to apply. With `rewrite` set, revises that
    /// prompt in place; otherwise writes a fresh prompt toward the instruction.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instruction: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct WanPromptFromLlmResponse {
    pub prompt: String,
}

// ── Image generation (FLUX.2) ───────────────────────────────────────

fn default_image_model() -> String {
    "auto".to_string()
}

/// Request body for `POST /api/v1/images/generate`. Accepted as JSON, or as the
/// `request` part of a multipart body (with the image fields omitted and
/// supplied as file parts `init_image`, `mask`, repeated `reference_images`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageGenerateRequest {
    pub prompt: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub negative_prompt: Option<String>,
    /// "auto" (default) | "base" | "edit".
    #[serde(default = "default_image_model")]
    pub model: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub width: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub height: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub steps: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cfg: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sampler: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scheduler: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seed: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub batch: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub strength: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vae_tiling: Option<bool>,
    /// base64-encoded init image (img2img / edit source).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub init_image: Option<String>,
    /// base64-encoded mask (inpaint).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mask: Option<String>,
    /// base64-encoded reference images (FLUX.2 multi-ref edit).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reference_images: Option<Vec<String>>,
    /// Optionally chain background removal (matting) onto every rendered image.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remove_background: Option<BackgroundRemoval>,
}

impl Default for ImageGenerateRequest {
    fn default() -> Self {
        Self {
            prompt: String::new(),
            negative_prompt: None,
            model: default_image_model(),
            width: None,
            height: None,
            steps: None,
            cfg: None,
            sampler: None,
            scheduler: None,
            seed: None,
            batch: None,
            strength: None,
            vae_tiling: None,
            init_image: None,
            mask: None,
            reference_images: None,
            remove_background: None,
        }
    }
}

/// Background-removal step chained after the flux render. `enabled` is the
/// toggle; the rest mirror the matting `/remove` knobs 1:1.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct BackgroundRemoval {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backend: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub process_res: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sensitivity: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mask_blur: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mask_offset: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refine: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub invert: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bg_mode: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bg_color: Option<String>,
}

/// One rendered image. Both files are served from the gallery rather than
/// inlined, so a response can be ranged, streamed and used straight as an
/// `<img src>`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GeneratedImage {
    /// `/api/v1/generations/{generation_id}/file/{n}` — auth-gated, and also
    /// accepts `?token=` for elements that cannot set a header.
    pub url: String,
    pub seed: i64,
    /// Background-removed cutout, present only when the chained matting step ran
    /// and succeeded for this image.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cutout_url: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageGenerateResponse {
    pub images: Vec<GeneratedImage>,
    /// "base" | "edit" — the variant used after smart resolution.
    pub model_used: String,
    pub width: u32,
    pub height: u32,
    pub steps: u32,
    pub cfg: f32,
    pub render_ms: u64,
    pub gate_wait_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub background_removal_error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub background_removal_backend: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub background_removal_seconds: Option<f64>,
    /// The gallery row every `images[].url` points into. Always present: the
    /// renders have no other delivery, so a route that could not save them fails.
    pub generation_id: String,
}

/// Request body for `POST /api/v1/images/suggest-prompt`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SuggestPromptRequest {
    pub brief: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reference_images: Option<Vec<String>>,
    /// An existing prompt to improve in place instead of writing fresh from the
    /// brief. When present, the model revises it under the same FLUX.2 rules.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refine_source: Option<String>,
    /// Free-form change to apply to `refine_source`. Empty with a `refine_source`
    /// set → a general polish. Ignored when `refine_source` is absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refine_instruction: Option<String>,
}

/// Response body for `POST /api/v1/images/suggest-prompt`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SuggestPromptResponse {
    pub prompt: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub negative_prompt: Option<String>,
}

/// Per-variant render prefills (`base`/`edit` for FLUX.2, `t2i`/`edit` for Krea2).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ImageVariantDefaults {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_steps: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_cfg: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_sampler: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_scheduler: Option<String>,
}

/// `GET /api/v1/images/props` and `GET /api/v1/krea2/props` — UI/CLI bootstrap.
/// One struct for both engines: the Krea2-only fields are optional and absent
/// from the FLUX.2 payload (and vice-versa for `supported_modes`).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ImageProps {
    pub healthy: bool,
    /// Krea2 only: false when no engine URL is set at all (vs set-but-unreachable).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub configured: Option<bool>,
    #[serde(default)]
    pub samplers: Vec<String>,
    #[serde(default)]
    pub schedulers: Vec<String>,
    #[serde(default)]
    pub limits: Value,
    /// FLUX.2 only.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub supported_modes: Vec<String>,
    #[serde(default)]
    pub variants: Value,
    /// Krea2 only: engine-level render defaults.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub defaults: Option<Value>,
    /// Krea2 only: false when the identity-edit LoRA is not resolvable on the
    /// engine — an edit attempt then fails as a bare 400.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub edit_available: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub edit_lora: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_reference_images: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_width: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_height: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_dim: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_dim: Option<u32>,
    /// Krea2 only: bottom of the model's supported range — a hint, not a clamp.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recommended_min_dim: Option<u32>,
}

// ── Krea 2 Turbo (second image engine) ──────────────────────────────

/// Request body for `POST /api/v1/krea2/generate`. JSON, or the `request` part
/// of a multipart body with `reference_images` as file parts.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Krea2GenerateRequest {
    pub prompt: String,
    /// `"auto"` (default) | `"t2i"` | `"edit"`. `auto` resolves to `edit` iff a
    /// reference image is present. An explicit `"edit"` with no reference is a 400.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
    /// 1-2 entries, and **the order is load-bearing: scene first, person second**
    /// — that is the layout the edit LoRA was trained on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reference_images: Option<Vec<String>>,
    /// Edit mode only: longest side the reference is handed to the VL model at.
    /// LOWER = stronger edit adherence; HIGHER = stronger identity.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub grounding_px: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub negative_prompt: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub width: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub height: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub steps: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cfg: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sampler: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scheduler: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seed: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub batch: Option<u32>,
    /// Leave unset to keep the engine's own `--vae-tiling` default (ON, which is
    /// what makes a 2048² VAE decode fit); sending `false` turns it off.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vae_tiling: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remove_background: Option<BackgroundRemoval>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Krea2GenerateResponse {
    pub images: Vec<GeneratedImage>,
    /// Which mode actually ran: `"t2i"` or `"edit"`. What `auto` resolved to.
    pub mode_used: String,
    pub width: u32,
    pub height: u32,
    pub steps: u32,
    pub cfg: f32,
    pub render_ms: u64,
    pub gate_wait_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub background_removal_error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub background_removal_backend: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub background_removal_seconds: Option<f64>,
    /// The gallery row every `images[].url` points into. Always present: the
    /// renders have no other delivery, so a route that could not save them fails.
    pub generation_id: String,
}

/// `POST /api/v1/krea2/suggest-prompt`. No `reference_images`: the assist writes
/// a t2i caption, which is the wrong shape for an edit instruction.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Krea2SuggestPromptRequest {
    #[serde(default)]
    pub brief: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refine_source: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refine_instruction: Option<String>,
}

// ── Generation gallery ──────────────────────────────────────────────

/// One saved output file of a generation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GenerationOutput {
    pub filename: String,
    pub mime: String,
    pub size_bytes: i64,
    /// `video` | `audio` | `image` | `transcript`.
    pub kind: String,
}

/// One row of the generation gallery — `GET /api/v1/generations/{id}` and each
/// entry of [`GenerationListResponse`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GenerationRow {
    pub id: String,
    pub service: String,
    pub endpoint: String,
    /// Restorable knobs the route recorded, shaped per-service.
    #[serde(default)]
    pub request_meta: Value,
    #[serde(default)]
    pub outputs: Vec<GenerationOutput>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// RFC 3339.
    pub created_at: String,
}

/// `GET /api/v1/generations`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GenerationListResponse {
    #[serde(default)]
    pub items: Vec<GenerationRow>,
    #[serde(default)]
    pub total: i64,
}

/// `PATCH /api/v1/generations/{id}` body, and the reply to both that and
/// `POST /api/v1/generations/{id}/retitle`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GenerationTitle {
    pub title: String,
}

// ── Background removal (matting) ─────────────────────────────────────

/// One cut-out result from `POST /api/v1/matting/remove`. The PNG is served from
/// the gallery rather than inlined, so the response can be ranged, streamed and
/// used straight as an `<img src>`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MattingResultImage {
    /// `/api/v1/generations/{generation_id}/file/{n}` — auth-gated, and also
    /// accepts `?token=` for elements that cannot set a header.
    pub url: String,
}

/// Response body for `POST /api/v1/matting/remove`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MattingRemoveResponse {
    pub count: u32,
    pub results: Vec<MattingResultImage>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backend: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub elapsed_seconds: Option<f64>,
    /// The gallery row every `results[].url` points into. Always present: the
    /// cutouts have no other delivery, so a route that could not save them fails.
    pub generation_id: String,
}

// ── Raw chat (/api/v1/chat) ─────────────────────────────────────────

/// Request body for `POST /api/v1/chat`. The per-request `SamplingConfig` is
/// flattened onto the top-level body (e.g. `{"prompt":"…","temperature":0.7}`).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ChatRequest {
    pub prompt: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system: Option<String>,
    /// Optional image data URLs (`data:image/<mime>;base64,<payload>`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub images: Vec<String>,
    /// Optional audio data URLs (`data:audio/<mime>;base64,<payload>`) for
    /// audio-capable multimodal models (e.g. Gemma 4's audio mmproj). Each
    /// becomes an OpenAI-style `input_audio` content part on the wire.
    ///
    /// 🔴 DROPPED, not rejected, when the loaded model has no audio encoder
    /// (Beefy mode's Qwen3.8 has a vision-only mmproj). The turn still runs, the
    /// images still go, and [`ChatResponse::warnings`] says what was lost. See
    /// [`HealthResponse::llama_audio_input`] to pre-empt it in a UI.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub audio: Vec<String>,
    #[serde(default, flatten)]
    pub sampling: SamplingConfig,
}

/// Response body for `POST /api/v1/chat`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ChatResponse {
    pub response: String,
    #[serde(default)]
    pub prompt_tokens: Option<i32>,
    #[serde(default)]
    pub completion_tokens: Option<i32>,
    pub duration_ms: i64,
    /// Non-fatal things the server did to the request, in the caller's words.
    ///
    /// 🔴 The turn still happened. Today's only entry is "your audio was
    /// dropped because the loaded model has no audio encoder" — the same
    /// doctrine `agent::run::inline_attachments` already follows for a missing
    /// asset: losing the picture is better than losing the turn. A 400 here
    /// would turn a model swap into a broken chat box.
    ///
    /// Mirrors `Job::warnings`, which is where the queue puts the same idea.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
}

// ─── The global GPU job queue ────────────────────────────────────────────────
//
// One row per unit of GPU work. These types are the contract between koblem, the
// `kob` CLI and the web app, so they live here rather than in koblem's tree.
//
// The queue is the *scheduler*: `kob-gpu-gate` still owns the VRAM ledger,
// placement and eviction, but the queue decides what runs next.

/// The adapter that runs a job.
///
/// Deliberately NOT a `ServiceKind`. `ltx25` and `h3` are different kinds sharing
/// one lane; `pixal3d_rig` and `pixal3d_motion` are different kinds on one
/// service. A kind names a *unit of work*, a service names a *VRAM claim*.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobKind {
    Ltx25,
    H3,
    Music3,
    Upscale,
    Krea2,
    Flux2,
    Avatar,
    WanVace,
    Ltx2b,
    Songgen,
    Sa3,
    Pixal3dRig,
    Pixal3dMotion,
    /// LTX-2.5 re-lip: an existing clip plus new audio, re-rendered in windows.
    /// Its own kind rather than a mode of [`JobKind::Ltx25`] because it renders a
    /// window at a time over a clip that already exists, so it shares the engine
    /// and the lane but nothing of the other's payload or resume.
    LtxRelip,
}

impl JobKind {
    pub const ALL: [JobKind; 14] = [
        JobKind::Ltx25,
        JobKind::H3,
        JobKind::Music3,
        JobKind::Upscale,
        JobKind::Krea2,
        JobKind::Flux2,
        JobKind::Avatar,
        JobKind::WanVace,
        JobKind::Ltx2b,
        JobKind::Songgen,
        JobKind::Sa3,
        JobKind::Pixal3dRig,
        JobKind::Pixal3dMotion,
        JobKind::LtxRelip,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            JobKind::Ltx25 => "ltx25",
            JobKind::H3 => "h3",
            JobKind::Music3 => "music3",
            JobKind::Upscale => "upscale",
            JobKind::Krea2 => "krea2",
            JobKind::Flux2 => "flux2",
            JobKind::Avatar => "avatar",
            JobKind::WanVace => "wan_vace",
            JobKind::Ltx2b => "ltx_2b",
            JobKind::Songgen => "songgen",
            JobKind::Sa3 => "sa3",
            JobKind::Pixal3dRig => "pixal3d_rig",
            JobKind::Pixal3dMotion => "pixal3d_motion",
            JobKind::LtxRelip => "ltx_relip",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        JobKind::ALL.into_iter().find(|k| k.as_str() == s)
    }

    /// What one unit of this kind is called, for the UI's "3 of 16" line. The
    /// engines each use their own word and never `units`.
    pub fn unit_noun(self) -> &'static str {
        match self {
            JobKind::H3 => "generation",
            JobKind::Ltx25 | JobKind::Upscale | JobKind::Ltx2b | JobKind::WanVace => "segment",
            JobKind::LtxRelip => "window",
            JobKind::Music3 | JobKind::Songgen | JobKind::Sa3 => "track",
            JobKind::Pixal3dRig => "tier",
            _ => "step",
        }
    }
}

/// Where a job is in its life.
///
/// 🔴 `Collecting` is load-bearing, not decoration. Every runner drops the GPU
/// guard the moment engine work ends and BEFORE the disk/DB work — on an upscale
/// that tail is minutes, and holding the card through it would idle a GPU on
/// every harvest. A job in `Collecting` has already paid its GPU cost, which is
/// also why it is **not cancellable**: the artifact is landing, and killing it
/// there loses the only copy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobState {
    Queued,
    Running,
    Collecting,
    Done,
    Failed,
    Cancelled,
    /// Operator paused. "Finish the current job, then stop" is the overnight
    /// lever, and it needs a state at both queue and job level.
    Held,
}

impl JobState {
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Done | Self::Failed | Self::Cancelled)
    }

    /// Pending = the dispatcher will consider it on a future tick.
    pub fn is_pending(self) -> bool {
        matches!(self, Self::Queued | Self::Held)
    }

    /// Occupies a lane and a VRAM claim right now.
    pub fn is_live(self) -> bool {
        matches!(self, Self::Running | Self::Collecting)
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Running => "running",
            Self::Collecting => "collecting",
            Self::Done => "done",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
            Self::Held => "held",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "queued" => Some(Self::Queued),
            "running" => Some(Self::Running),
            "collecting" => Some(Self::Collecting),
            "done" => Some(Self::Done),
            "failed" => Some(Self::Failed),
            "cancelled" => Some(Self::Cancelled),
            "held" => Some(Self::Held),
            _ => None,
        }
    }
}

/// Why a job did not start on the last tick.
///
/// 🔴 Skip-with-a-reason is the dispatcher's MAIN LOOP, not an error branch. A
/// job that cannot run right now is never failed and never blocks the queue: it
/// is stamped, skipped, and reconsidered next tick. That skip is also exactly
/// what makes backfill work — which is why the head reservation is load-bearing
/// rather than a refinement, since it is the only thing stopping a big job from
/// being skipped forever.
///
/// One enum so every surface says the same thing, and every variant knows how it
/// clears.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "reason", rename_all = "snake_case")]
pub enum BlockedReason {
    /// Clears when an operator re-enables the service. Never fails the job —
    /// disabling flux at 22:00 must not burn the overnight batch.
    ServiceDisabled { service: String },
    /// From `gate.can_place()`. Clears when a card is re-enabled, budget frees,
    /// or `peak_mb` is edited — a peak edit that makes a job unplaceable makes it
    /// *wait and say so*.
    NoCardFits { detail: String },
    /// The comfy-api single slot. The most common reason by far.
    LaneBusy { lane: String },
    /// Transient, ≤15 s. 🔴 Never reorder around this one: `recently_evicted`
    /// holds a cooldown after an eviction and a `try_acquire` failing inside it
    /// is not "unplaceable".
    EvictionCooldown,
    /// Clears when the parent reaches `done`; a parent that fails or is cancelled
    /// cascades this job to `cancelled` instead.
    DependencyPending { parent: String },
    /// 🔴 Retried with BACKOFF, not every tick — a dead service must not be
    /// hammered once a second all night.
    EngineUnreachable { detail: String },
    /// The head job's resources are reserved for it. Without this, backfill
    /// starves the big job forever.
    HeadReserved { head: String },
    /// The operator paused the queue, or held this row.
    Paused,
    /// The box is in exclusive ("Beefy") mode: one service owns BOTH cards and
    /// nothing else may be admitted. Clears when an operator leaves the mode.
    /// Like `ServiceDisabled` this is an operator state, so the job WAITS and
    /// says so — it is never failed, and it never earns a head reservation.
    BoxLocked { service: String },
}

impl BlockedReason {
    /// A one-line "waiting: …" for the queue page and the CLI.
    pub fn summary(&self) -> String {
        match self {
            BlockedReason::ServiceDisabled { service } => format!("{service} is disabled"),
            BlockedReason::NoCardFits { detail } => detail.clone(),
            BlockedReason::LaneBusy { lane } => format!("{lane} slot busy"),
            BlockedReason::EvictionCooldown => "an eviction is settling".to_string(),
            BlockedReason::DependencyPending { parent } => format!("waiting on {parent}"),
            BlockedReason::EngineUnreachable { detail } => format!("engine unreachable: {detail}"),
            BlockedReason::HeadReserved { head } => {
                format!("holding resources for {head}")
            }
            BlockedReason::Paused => "the queue is paused".to_string(),
            BlockedReason::BoxLocked { service } => {
                format!("the box is held by Beefy LLM ({service})")
            }
        }
    }

    /// Can an operator clear this from the settings page? Drives the
    /// blocked-too-long notification: waking up to five jobs that never started
    /// because a service was left disabled is the failure this design exists to
    /// prevent, and the notification is only worth sending when there is
    /// something to do about it.
    pub fn is_operator_clearable(&self) -> bool {
        matches!(
            self,
            BlockedReason::ServiceDisabled { .. }
                | BlockedReason::Paused
                // Beefy left on overnight is exactly the failure this predicate
                // exists for: jobs that never started because a switch was left
                // flipped, and a human can clear it in one click.
                | BlockedReason::BoxLocked { .. }
        )
    }

    /// Transient states that should not be surfaced as "stuck" at all.
    pub fn is_transient(&self) -> bool {
        matches!(
            self,
            BlockedReason::EvictionCooldown
                | BlockedReason::LaneBusy { .. }
                | BlockedReason::HeadReserved { .. }
        )
    }
}

/// What a preempt of this kind costs, and whether it is possible at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Preempt {
    /// Cannot be interrupted. `pixal3d_rig` — the gate deliberately refuses to
    /// unload it because it has queued work koblem cannot see.
    None,
    /// Stops at the next unit boundary and resumes from its bank. Costs ≤ 1
    /// segment / generation.
    AtCheckpoint,
    /// Cancel and requeue whole. Seconds, for the short kinds.
    Kill,
}

/// Progress as reported by whichever engine is running this kind. Every field is
/// optional because the four comfy-api plugins and the seven sd.cpp deployments
/// each publish a different subset, and an absent field means UNKNOWN — never
/// zero. A 0/0 bar reads as finished.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct JobProgress {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phase: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub progress: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_step: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_steps: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preview_seq: Option<u64>,
    /// The engine's own queue position, when it publishes one. Informational:
    /// koblem never uses the engines' pending queues (they are 8 and 64 deep),
    /// because two queues with two orderings would mean cancel has to chase work
    /// into a queue koblem does not own.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub queue_position: Option<u32>,
}

/// A queue row as the API serves it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Job {
    pub id: String,
    pub kind: JobKind,
    pub state: JobState,
    pub band: i16,
    pub queue_rank: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_gpu: Option<String>,
    #[serde(default)]
    pub force_pin: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server_job_id: Option<String>,
    #[serde(default)]
    pub progress: JobProgress,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub depends_on: Option<String>,
    pub created_by: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blocked_reason: Option<BlockedReason>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blocked_since: Option<String>,
    /// Closed waiting episodes, oldest first and bounded. 🔴 `queued_ms` alone is
    /// unreadable — it includes waits that are the user's own choice — so the
    /// morning question "why was the box idle at 3am" is only answerable next to
    /// the reasons it waited on and how long each lasted.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub blocked_history: Vec<BlockedEpisode>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default)]
    pub warnings: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub queued_ms: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub admit_ms: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub render_ms: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub units_total: Option<i32>,
    #[serde(default)]
    pub units_done: i32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shape_key: Option<String>,
    /// A short human label for the row ("h3 · 16 generations · 1920x1088"),
    /// built server-side. Prepopulated so the page does not have to know how to
    /// read thirteen different payload shapes.
    #[serde(default)]
    pub label: String,
    pub created_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<String>,
    /// Simulated start from the dispatcher replay. `None` once the job has
    /// started, and always reported next to [`Job::estimate_samples`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub eta_start: Option<String>,
    /// Simulated finish. Published for a RUNNING job too — one long render with
    /// an empty queue is the commonest overnight state, and it is the exact
    /// number the pill wants.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub eta_finish: Option<String>,
    /// The same finish on the p90 of every job in front of it. 🔴 Published next
    /// to `eta_finish` because a confident-looking ETA built from three samples
    /// is worse than one that admits it is guessing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub eta_finish_p90: Option<String>,
    /// How many measured runs the estimate rests on. 🔴 Reported so the UI can
    /// stop a three-sample guess from reading like a measurement.
    #[serde(default)]
    pub estimate_samples: i32,
    /// 1 on a first run. A row that has been retried must never read as a first
    /// attempt.
    #[serde(default = "one")]
    pub attempt: i32,
    /// The terminal job this row was requeued from, for a manual retry.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry_of: Option<String>,
}

fn one() -> i32 {
    1
}

/// One closed waiting episode: what the job waited on, when that wait started,
/// and how long it lasted.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BlockedEpisode {
    pub reason: BlockedReason,
    pub since: String,
    pub ms: i64,
}

/// Queue-vs-immediate. The default comes from the caller's principal; an explicit
/// mode on the request always wins, because the CLI authenticates with an API key
/// and would otherwise be forced into `Now` — the opposite of what it is for.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SubmitMode {
    #[default]
    Queue,
    /// 🔴 Jump the ORDERING, not the SCHEDULER. An immediate job still gets a row
    /// and still goes through admission — bypassing the dispatcher would take
    /// VRAM the dispatcher believed it had, which is two schedulers fighting over
    /// one ledger. It does NOT preempt: it waits for the running job exactly as
    /// the gate would make it wait today, or a kobbler library scan kills a
    /// 4-hour render.
    Now,
}

/// What an enqueue answers with. 🔴 The response must always state what actually
/// happened, because it is the only non-racy signal: whether a job runs now or
/// waits is a *prediction*, and a prediction can go stale between paint and click.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnqueueResponse {
    pub job_id: String,
    pub state: JobState,
    /// 1-based position within the pending set. `None` once it is running.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub position: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub eta_start: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blocked_reason: Option<BlockedReason>,
    /// True when this enqueue matched an existing row's idempotency key and no
    /// new work was created.
    #[serde(default)]
    pub deduplicated: bool,
}

/// The whole queue, as one page render needs it.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct QueueSnapshot {
    pub jobs: Vec<Job>,
    /// Operator pause: the dispatcher finishes what is running and starts nothing.
    #[serde(default)]
    pub paused: bool,
    #[serde(default)]
    pub pending: i64,
    #[serde(default)]
    pub running: i64,
    /// Simulated finish time of the whole batch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub eta_finish: Option<String>,
    /// Jobs blocked longer than the alert threshold on a reason an operator can
    /// clear. This is the state that must be visible without opening the page.
    #[serde(default)]
    pub stuck: Vec<String>,
    /// The service exclusive ("Beefy") mode is held for, when the box is locked.
    /// `Some("llm")` is the only value that exists today.
    ///
    /// 🔴 Carried HERE rather than polled. Every page already holds this
    /// snapshot open (the topbar queue indicator does), so a whole-app banner
    /// for "the box is handed to the 27B and nothing else can run" costs zero
    /// new network traffic. It is a mutex read on the gate, not a DB or HTTP
    /// call — cheap enough for a 10 s snapshot.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub box_locked: Option<String>,
}

/// Move a job. Exactly one of the fields is honoured, checked in this order.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ReorderRequest {
    /// Place immediately after this job (same band). `null` = head of the band.
    #[serde(default)]
    pub after: Option<String>,
    /// Change band.
    #[serde(default)]
    pub band: Option<i16>,
    /// Straight to the head of band 0.
    #[serde(default)]
    pub bump: bool,
    /// Repin (or unpin, with `Some(None)`) a job that has not started. A pin is
    /// most useful before admission, but a card can be disabled after a job is
    /// already queued behind it, and then the pin is the thing that needs moving.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_gpu: Option<Option<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub force_pin: Option<bool>,
}

/// Enqueue body shared by every render route's queue path.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnqueueRequest {
    pub kind: JobKind,
    pub payload: Value,
    #[serde(default)]
    pub mode: Option<SubmitMode>,
    #[serde(default)]
    pub band: Option<i16>,
    #[serde(default)]
    pub target_gpu: Option<String>,
    #[serde(default)]
    pub force_pin: bool,
    #[serde(default)]
    pub depends_on: Option<String>,
    #[serde(default)]
    pub idempotency_key: Option<String>,
}

/// One measured `(kind, shape_key)` bucket, as the estimator serves it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobStat {
    pub kind: String,
    pub shape_key: String,
    pub samples: i32,
    pub p50_ms: i64,
    pub p90_ms: i64,
    pub unit_p50_ms: i64,
    pub updated_at: String,
}

/// A job-state transition, as the SSE stream and the NATS subject carry it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobEvent {
    pub job_id: String,
    pub kind: JobKind,
    pub state: JobState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub progress: Option<JobProgress>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blocked_reason: Option<BlockedReason>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default)]
    pub warnings: Vec<String>,
    pub at: String,
}

/// The outbound webhook payload. 🔴 Raw JSON with a pre-rendered `text`, so ntfy,
/// gotify, Discord and Slack all work with zero koblem-specific templating: each
/// of them shows *something* useful from a body carrying a plain string, and a
/// per-service template is a maintenance burden for a one-line message.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebhookPayload {
    /// `batch_complete` | `job_failed` | `job_blocked`.
    pub event: String,
    pub title: String,
    /// The whole message in one line, already assembled.
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub job_id: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
    #[serde(default)]
    pub ok: i64,
    #[serde(default)]
    pub failed: i64,
    pub at: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // ── SamplingConfig ──────────────────────────────────────────────

    #[test]
    fn deterministic_preset_only_sets_temperature() {
        let cfg = SamplingConfig {
            preset: Some(SamplingPreset::Deterministic),
            ..SamplingConfig::default()
        };
        let mut body = json!({"messages": []});
        cfg.apply_to(&mut body);
        assert_eq!(body["temperature"], json!(0.0));
        // Nothing else should be injected — keeps Deterministic byte-identical
        // to koblem's pre-refactor structured-path body.
        for k in [
            "top_p",
            "top_k",
            "min_p",
            "presence_penalty",
            "repetition_penalty",
            "max_tokens",
        ] {
            assert!(body.get(k).is_none(), "{k} leaked into body");
        }
    }

    #[test]
    fn empty_config_does_not_mutate_body() {
        let cfg = SamplingConfig::default();
        let mut body = json!({"messages": [], "temperature": 0.42});
        cfg.apply_to(&mut body);
        // No preset, no overrides — pass-through. Caller-set fields stay.
        assert_eq!(body["temperature"], json!(0.42));
        assert!(body.get("top_p").is_none());
    }

    #[test]
    fn overlay_layers_baseline_db_request() {
        let baseline = SamplingConfig {
            preset: Some(SamplingPreset::Deterministic),
            ..SamplingConfig::default()
        };
        // DB default chooses a different preset and nudges top_p.
        let db = SamplingConfig {
            preset: Some(SamplingPreset::QwenInstructGeneral),
            top_p: Some(0.88),
            ..SamplingConfig::default()
        };
        // Request override pins temperature only.
        let req = SamplingConfig {
            temperature: Some(0.3),
            ..SamplingConfig::default()
        };

        let merged = baseline.overlay(&db).overlay(&req);
        let values = merged.resolve();

        assert_eq!(values.temperature, Some(0.3), "request overrides");
        assert_eq!(values.top_p, Some(0.88), "DB top_p wins over preset");
        // Preset values from QwenInstructGeneral for the fields nobody else set.
        assert_eq!(values.top_k, Some(20));
        assert_eq!(values.min_p, Some(0.0));
        assert_eq!(values.presence_penalty, Some(1.5));
        assert_eq!(values.repetition_penalty, Some(1.0));
    }

    #[test]
    fn qwen_instruct_reasoning_values() {
        // Preset stays in sync with the `values()` match arm. If you change
        // the numbers, update the TS mirror in SamplingControls too.
        let v = SamplingPreset::QwenInstructReasoning.values();
        assert_eq!(v.temperature, Some(1.0));
        assert_eq!(v.top_p, Some(0.95));
        assert_eq!(v.top_k, Some(20));
        assert_eq!(v.min_p, Some(0.0));
        // 🔴 0.0, NOT 1.5 — thinking mode. This assertion said 1.5 and was
        // left stale when the preset was corrected; it is the preset that is
        // right. See the note on the `QwenInstructReasoning` arm.
        assert_eq!(v.presence_penalty, Some(0.0));
        assert_eq!(v.repetition_penalty, Some(1.0));
    }

    #[test]
    fn sampling_preset_wire_key_is_sampling_preset() {
        // The serde rename matters — the chat UI, settings storage, and
        // `chat_completions.rs` strip all speak `sampling_preset`.
        let cfg = SamplingConfig {
            preset: Some(SamplingPreset::QwenInstructGeneral),
            ..SamplingConfig::default()
        };
        let s = serde_json::to_string(&cfg).unwrap();
        assert!(
            s.contains("\"sampling_preset\":\"qwen_instruct_general\""),
            "{s}"
        );
        assert!(!s.contains("\"preset\""), "bare `preset` key leaked: {s}");

        // Round-trip with the documented wire key must populate the struct
        // field regardless of the field's internal name.
        let back: SamplingConfig =
            serde_json::from_value(json!({"sampling_preset": "deterministic"})).unwrap();
        assert_eq!(back.preset, Some(SamplingPreset::Deterministic));
    }

    #[test]
    fn apply_to_overwrites_preexisting_body_fields() {
        // When a preset OR explicit value resolves, it replaces whatever was
        // on the body — this is what lets the `/v1/chat/completions` proxy
        // layer DB defaults on top of caller-set fields without duplication.
        let cfg = SamplingConfig {
            preset: Some(SamplingPreset::QwenInstructGeneral),
            temperature: Some(0.9),
            ..SamplingConfig::default()
        };
        let mut body = json!({"messages": [], "temperature": 0.0, "top_p": 0.5});
        cfg.apply_to(&mut body);
        assert_eq!(body["temperature"], json!(0.9));
        assert_eq!(body["top_p"], json!(0.8)); // from preset
    }

    #[test]
    fn reasoning_effort_rides_in_chat_template_kwargs() {
        // Same object as `enable_thinking`, merged not replaced.
        let cfg = SamplingConfig {
            enable_thinking: Some(true),
            reasoning_effort: Some(ReasoningEffort::Low),
            ..SamplingConfig::default()
        };
        let mut body = json!({"messages": [], "chat_template_kwargs": {"other": 1}});
        cfg.apply_to(&mut body);
        assert_eq!(
            body["chat_template_kwargs"]["reasoning_effort"],
            json!("low")
        );
        assert_eq!(body["chat_template_kwargs"]["enable_thinking"], json!(true));
        assert_eq!(body["chat_template_kwargs"]["other"], json!(1));
    }

    #[test]
    fn reasoning_effort_is_dropped_when_thinking_is_off() {
        // An effort budget for a trace that never happens is noise on the wire.
        let cfg = SamplingConfig {
            enable_thinking: Some(false),
            reasoning_effort: Some(ReasoningEffort::Xhigh),
            ..SamplingConfig::default()
        };
        let mut body = json!({"messages": []});
        cfg.apply_to(&mut body);
        assert_eq!(
            body["chat_template_kwargs"]["enable_thinking"],
            json!(false)
        );
        assert!(
            body["chat_template_kwargs"]
                .get("reasoning_effort")
                .is_none()
        );
    }

    #[test]
    fn an_unknown_sampling_preset_tag_is_ignored_not_fatal() {
        // The llm-logs Rerun form echoes back whatever the column held, and
        // that column now also carries surface keys like `sampling_director`.
        let cfg: SamplingConfig =
            serde_json::from_value(json!({"sampling_preset": "sampling_director", "top_p": 0.4}))
                .expect("an unknown tag must not 422 the whole rerun");
        assert_eq!(cfg.preset, None);
        assert_eq!(cfg.top_p, Some(0.4));
    }

    #[test]
    fn a_none_reasoning_effort_does_not_sink_the_whole_config() {
        // 🔴 The proxy parses the caller's body straight into SamplingConfig and
        // falls back to Default on error — a strict enum here would throw away
        // temperature/top_p/... because of one unrelated OpenAI key.
        let cfg: SamplingConfig =
            serde_json::from_value(json!({"temperature": 0.25, "reasoning_effort": "none"}))
                .expect("must not fail on an unrepresentable effort");
        assert_eq!(cfg.temperature, Some(0.25));
        assert_eq!(cfg.reasoning_effort, None);
    }

    #[test]
    fn reasoning_effort_has_no_none_variant() {
        // 🔴 `"none"` makes Qwen3.8's template raise -> llama-server 500.
        // The parser must refuse it, and the wire strings must stay exact.
        assert!(ReasoningEffort::from_wire("none").is_none());
        assert!(ReasoningEffort::from_wire("off").is_none());
        assert_eq!(
            ReasoningEffort::from_wire("XHigh"),
            Some(ReasoningEffort::Xhigh)
        );
        assert_eq!(
            serde_json::to_string(&ReasoningEffort::Xhigh).unwrap(),
            "\"xhigh\""
        );
        assert_eq!(
            serde_json::to_string(&ReasoningEffort::Medium).unwrap(),
            "\"medium\""
        );
        assert_eq!(ReasoningEffort::Low.as_wire(), "low");
    }

    #[test]
    fn enable_thinking_writes_chat_template_kwargs() {
        // `Some(false)` writes the chat_template_kwargs.enable_thinking key
        // — replaces the old hardcoded `disable_thinking_auto_inject` path.
        let cfg = SamplingConfig {
            enable_thinking: Some(false),
            ..SamplingConfig::default()
        };
        let mut body = json!({"messages": []});
        cfg.apply_to(&mut body);
        assert_eq!(
            body["chat_template_kwargs"]["enable_thinking"],
            json!(false)
        );

        // Some(true) writes true.
        let cfg = SamplingConfig {
            enable_thinking: Some(true),
            ..SamplingConfig::default()
        };
        let mut body = json!({"messages": []});
        cfg.apply_to(&mut body);
        assert_eq!(body["chat_template_kwargs"]["enable_thinking"], json!(true));
    }

    #[test]
    fn enable_thinking_none_leaves_body_alone() {
        // None — pure pass-through, body unchanged.
        let cfg = SamplingConfig::default();
        let mut body = json!({"messages": []});
        cfg.apply_to(&mut body);
        assert!(body.get("chat_template_kwargs").is_none());
    }

    #[test]
    fn enable_thinking_merges_with_existing_kwargs() {
        // OpenAI-compatible callers may already pass other chat_template_kwargs;
        // we must merge, not clobber.
        let cfg = SamplingConfig {
            enable_thinking: Some(true),
            ..SamplingConfig::default()
        };
        let mut body = json!({
            "messages": [],
            "chat_template_kwargs": {"add_generation_prompt": true},
        });
        cfg.apply_to(&mut body);
        assert_eq!(
            body["chat_template_kwargs"]["add_generation_prompt"],
            json!(true)
        );
        assert_eq!(body["chat_template_kwargs"]["enable_thinking"], json!(true));
    }

    #[test]
    fn enable_thinking_overlay_request_wins() {
        let baseline = SamplingConfig {
            enable_thinking: Some(false),
            ..SamplingConfig::default()
        };
        let req = SamplingConfig {
            enable_thinking: Some(true),
            ..SamplingConfig::default()
        };
        let merged = baseline.overlay(&req);
        assert_eq!(merged.resolve().enable_thinking, Some(true));

        // None on the override falls through to baseline.
        let merged = baseline.overlay(&SamplingConfig::default());
        assert_eq!(merged.resolve().enable_thinking, Some(false));
    }

    #[test]
    fn chat_request_omits_empty_media_arrays() {
        // `images` + `audio` must NOT serialize when empty: they flatten
        // alongside the sampling fields, and a stray `"images":[]`/`"audio":[]`
        // would pollute the wire body (and any downstream request logging).
        let req = ChatRequest {
            prompt: "hi".to_string(),
            ..ChatRequest::default()
        };
        let body = serde_json::to_value(&req).unwrap();
        assert_eq!(body["prompt"], json!("hi"));
        assert!(body.get("images").is_none());
        assert!(body.get("audio").is_none());
    }

    #[test]
    fn chat_request_roundtrips_audio() {
        let req = ChatRequest {
            prompt: "what is this?".to_string(),
            audio: vec!["data:audio/wav;base64,QUJD".to_string()],
            ..ChatRequest::default()
        };
        let body = serde_json::to_value(&req).unwrap();
        assert_eq!(body["audio"][0], json!("data:audio/wav;base64,QUJD"));
        let back: ChatRequest = serde_json::from_value(body).unwrap();
        assert_eq!(back.audio, req.audio);
    }

    #[test]
    fn enable_thinking_serde_roundtrip() {
        let cfg = SamplingConfig {
            enable_thinking: Some(true),
            ..SamplingConfig::default()
        };
        let s = serde_json::to_string(&cfg).unwrap();
        assert!(s.contains("\"enable_thinking\":true"), "{s}");
        let back: SamplingConfig = serde_json::from_str(&s).unwrap();
        assert_eq!(back.enable_thinking, Some(true));

        // Default (None) is omitted from the wire — keeps payloads tiny when
        // the caller doesn't care.
        let cfg = SamplingConfig::default();
        let s = serde_json::to_string(&cfg).unwrap();
        assert!(!s.contains("enable_thinking"), "{s}");
    }

    // ── GrammarMode ─────────────────────────────────────────────────

    #[test]
    fn grammar_mode_wire_is_plain_string() {
        let off: GrammarMode = serde_json::from_value(json!("off")).unwrap();
        assert_eq!(off, GrammarMode::Off);
        let review: GrammarMode = serde_json::from_value(json!("review")).unwrap();
        assert_eq!(review, GrammarMode::Review);
        let custom: GrammarMode = serde_json::from_value(json!("my_thing")).unwrap();
        assert_eq!(custom, GrammarMode::Named("my_thing".into()));

        assert_eq!(
            serde_json::to_value(&GrammarMode::Off).unwrap(),
            json!("off")
        );
        assert_eq!(
            serde_json::to_value(&GrammarMode::Review).unwrap(),
            json!("review")
        );
        assert_eq!(
            serde_json::to_value(GrammarMode::Named("xyz".into())).unwrap(),
            json!("xyz")
        );
    }

    #[test]
    fn grammar_wire_key_is_grammar_mode() {
        // Field uses serde rename so the wire is `grammar_mode`. This keeps
        // the proxy's pass-through `grammar` (raw GBNF text) separate from
        // the koblem-level mode selector.
        let cfg = SamplingConfig {
            grammar: Some(GrammarMode::Review),
            ..SamplingConfig::default()
        };
        let s = serde_json::to_string(&cfg).unwrap();
        assert!(s.contains("\"grammar_mode\":\"review\""), "{s}");
        // Round-trip via the wire key populates the field.
        let back: SamplingConfig = serde_json::from_value(json!({"grammar_mode": "off"})).unwrap();
        assert_eq!(back.grammar, Some(GrammarMode::Off));
    }

    #[test]
    fn grammar_overlay_request_wins() {
        let baseline = SamplingConfig {
            grammar: Some(GrammarMode::Review),
            ..SamplingConfig::default()
        };
        // Request explicitly disables grammar.
        let req = SamplingConfig {
            grammar: Some(GrammarMode::Off),
            ..SamplingConfig::default()
        };
        assert_eq!(
            baseline.overlay(&req).resolve().grammar,
            Some(GrammarMode::Off)
        );
        // None override falls through to baseline.
        assert_eq!(
            baseline
                .overlay(&SamplingConfig::default())
                .resolve()
                .grammar,
            Some(GrammarMode::Review)
        );
    }

    #[test]
    fn grammar_apply_to_does_not_touch_body() {
        // apply_to never writes the `grammar` field — DB resolution lives
        // server-side. This guarantee is what lets the proxy and chat paths
        // call apply_to without needing the pool.
        let cfg = SamplingConfig {
            grammar: Some(GrammarMode::Review),
            ..SamplingConfig::default()
        };
        let mut body = json!({"messages": []});
        cfg.apply_to(&mut body);
        assert!(body.get("grammar").is_none());
    }

    #[test]
    fn vision_text_embed_roundtrip() {
        let req = VisionTextEmbedRequest {
            prompts: vec!["credits roll".into(), "intro music".into()],
        };
        let json = serde_json::to_string(&req).unwrap();
        let back: VisionTextEmbedRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(req.prompts, back.prompts);

        let resp = VisionTextEmbedResponse {
            embeddings: vec![vec![0.1, 0.2], vec![0.3, 0.4]],
            duration_ms: Some(42),
            gate_wait_ms: None,
            gpu_log_id: Some(7),
        };
        let json = serde_json::to_string(&resp).unwrap();
        let back: VisionTextEmbedResponse = serde_json::from_str(&json).unwrap();
        assert_eq!(resp.embeddings, back.embeddings);
        assert_eq!(resp.duration_ms, back.duration_ms);
        assert_eq!(resp.gpu_log_id, back.gpu_log_id);
    }

    /// The legacy `{prompts}` body is a valid `TextEmbeddingsRequest`, and the new
    /// response still reads as the old one, so callers can move at their own pace.
    #[test]
    fn text_embeddings_accepts_the_legacy_shape_both_ways() {
        let legacy = serde_json::to_value(VisionTextEmbedRequest {
            prompts: vec!["credits roll".into()],
        })
        .unwrap();
        let req: TextEmbeddingsRequest = serde_json::from_value(legacy).unwrap();
        assert_eq!(req.prompts, vec!["credits roll".to_string()]);
        assert!(req.input.is_empty() && req.task.is_none() && req.chunk.is_none());

        let req: TextEmbeddingsRequest = serde_json::from_value(json!({
            "input": ["Alien"],
            "task": "document",
            "titles": [null],
            "chunk": {"max_tokens": 512, "overlap_tokens": 32}
        }))
        .unwrap();
        assert_eq!(req.task, Some(crate::embed::EmbedTask::Document));
        assert_eq!(req.titles, vec![None]);
        assert_eq!(
            req.chunk,
            Some(ChunkOptions {
                max_tokens: Some(512),
                overlap_tokens: 32
            })
        );

        let resp = TextEmbeddingsResponse {
            embeddings: vec![vec![0.6, 0.8]],
            model: crate::embed::EG2_MODEL.into(),
            dim: 2,
            chunks: Some(vec![TextEmbeddingChunk {
                input_index: 0,
                chunk_index: 0,
                char_start: 0,
                char_end: 5,
                embedding: vec![0.6, 0.8],
            }]),
            duration_ms: Some(3),
            ..Default::default()
        };
        let old: VisionTextEmbedResponse =
            serde_json::from_value(serde_json::to_value(&resp).unwrap()).unwrap();
        assert_eq!(old.embeddings, resp.embeddings);
        assert_eq!(old.duration_ms, Some(3));
    }

    #[test]
    fn vision_classify_from_embed_roundtrip() {
        let req = VisionClassifyFromEmbedRequest {
            image_embeddings: vec![vec![0.5, -0.5], vec![1.0, 0.0]],
            prompts: vec!["a".into(), "b".into(), "c".into()],
            return_logits: Some(true),
        };
        let json = serde_json::to_string(&req).unwrap();
        let back: VisionClassifyFromEmbedRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(req.image_embeddings, back.image_embeddings);
        assert_eq!(req.prompts, back.prompts);
        assert_eq!(req.return_logits, back.return_logits);

        // return_logits should be omitted when None to keep request payloads compact.
        let req2 = VisionClassifyFromEmbedRequest {
            image_embeddings: vec![vec![0.0]],
            prompts: vec!["x".into()],
            return_logits: None,
        };
        let json = serde_json::to_string(&req2).unwrap();
        assert!(
            !json.contains("return_logits"),
            "unset return_logits leaked into wire payload: {json}"
        );

        let resp = VisionClassifyFromEmbedResponse {
            scores: vec![vec![0.1, 0.9, 0.5]],
            logits: Some(vec![vec![-2.0, 2.0, 0.0]]),
            model: Some(crate::embed::EG2_MODEL.into()),
            duration_ms: Some(13),
            gate_wait_ms: Some(0),
            gpu_log_id: None,
        };
        let json = serde_json::to_string(&resp).unwrap();
        let back: VisionClassifyFromEmbedResponse = serde_json::from_str(&json).unwrap();
        assert_eq!(resp.scores, back.scores);
        assert_eq!(resp.logits, back.logits);
        assert_eq!(resp.duration_ms, back.duration_ms);
    }

    #[test]
    fn music_mode_wire_is_snake_case_and_defaults_generate() {
        assert_eq!(
            serde_json::to_value(MusicMode::Cover).unwrap(),
            json!("cover")
        );
        assert_eq!(
            serde_json::to_value(MusicMode::Repaint).unwrap(),
            json!("repaint")
        );
        // Omitted mode deserialises to Generate (the headline path).
        let req: MusicGenerateRequest =
            serde_json::from_value(json!({"caption": "lofi beats"})).unwrap();
        assert_eq!(req.mode, MusicMode::Generate);
        // save_song defaults true so the playground keeps renders by default.
        assert!(req.save_song);
        // duration omitted → koblem auto-computes it (None on the wire).
        assert!(req.duration.is_none());
        // lyrics default to the ACE engine; no separate brief.
        assert_eq!(req.lyrics_source, MusicLyricsSource::Ace);
        assert!(req.lyrics_prompt.is_none());
    }

    #[test]
    fn music_lyrics_source_wire() {
        assert_eq!(
            serde_json::to_value(MusicLyricsSource::Llm).unwrap(),
            json!("llm")
        );
        let req: MusicGenerateRequest = serde_json::from_value(json!({
            "caption": "dreamy synthwave",
            "lyrics_source": "llm",
            "lyrics_prompt": "a late-night drive through neon city streets"
        }))
        .unwrap();
        assert_eq!(req.lyrics_source, MusicLyricsSource::Llm);
        assert_eq!(
            req.lyrics_prompt.as_deref(),
            Some("a late-night drive through neon city streets")
        );
    }

    #[test]
    fn music_generate_response_roundtrip() {
        let resp = MusicGenerateResponse {
            tracks: vec![MusicTrack {
                filename: "song-42.mp3".into(),
                url: "/api/v1/music/file/song-42.mp3".into(),
                seed: 42,
                content_type: "audio/mpeg".into(),
                bytes: 1024,
            }],
            duration_used: 90.0,
            lyrics_used: Some("[Instrumental]".into()),
            duration_ms: Some(11000),
            gate_wait_ms: Some(0),
            gpu_log_id: Some(3),
        };
        let json = serde_json::to_string(&resp).unwrap();
        let back: MusicGenerateResponse = serde_json::from_str(&json).unwrap();
        assert_eq!(back.tracks.len(), 1);
        assert_eq!(back.tracks[0].seed, 42);
        assert_eq!(back.duration_used, 90.0);
    }

    // ── Character-sheet identity conditioning ───────────────────────

    fn sheet(identity: Option<&str>) -> LtxCharacterRef {
        LtxCharacterRef {
            image_b64: "AAAA".into(),
            source_id: Some(2),
            resize_mode: None,
            name: Some("Mia".into()),
            identity: identity.map(str::to_string),
        }
    }

    #[test]
    fn ref_t2v_prompt_is_identity_without_refs() {
        // The no-character path must stay byte-identical — every existing render goes through here.
        assert_eq!(ltx_ref_t2v_prompt("a woman walks", &[]), "a woman walks");
    }

    #[test]
    fn ref_t2v_prompt_prefixes_once() {
        let refs = vec![sheet(None)];
        assert_eq!(
            ltx_ref_t2v_prompt("a woman walks", &refs),
            "ref_t2v: a woman walks"
        );
        // Idempotent, and tolerant of a hand-typed prefix in any case.
        assert_eq!(
            ltx_ref_t2v_prompt("ref_t2v: a woman walks", &refs),
            "ref_t2v: a woman walks"
        );
        assert_eq!(
            ltx_ref_t2v_prompt("Ref_T2V: a woman walks", &refs),
            "Ref_T2V: a woman walks"
        );
    }

    #[test]
    fn ref_t2v_prompt_folds_identity_once() {
        let refs = vec![sheet(Some("dark curly hair, brown eyes, round glasses"))];
        let once = ltx_ref_t2v_prompt("a woman walks", &refs);
        assert_eq!(
            once,
            "ref_t2v: a woman walks. dark curly hair, brown eyes, round glasses."
        );
        // Re-running must not stack the clause (the compile path can run more than once).
        assert_eq!(ltx_ref_t2v_prompt(&once, &refs), once);
    }

    #[test]
    fn character_ref_omits_absent_fields() {
        let bare = LtxCharacterRef {
            image_b64: "AAAA".into(),
            ..Default::default()
        };
        let v = serde_json::to_value(&bare).unwrap();
        assert_eq!(v["image_b64"], json!("AAAA"));
        for k in ["source_id", "resize_mode", "name", "identity"] {
            assert!(v.get(k).is_none(), "{k} should be omitted when unset");
        }
        // The wire spelling of the resize mode is the engine's, not Rust's.
        assert_eq!(
            serde_json::to_value(LtxRefResizeMode::NativeResolution).unwrap(),
            json!("native_resolution")
        );
        assert_eq!(
            serde_json::to_value(LtxRefResizeMode::MatchTarget).unwrap(),
            json!("match_target")
        );
    }

    #[test]
    fn memory_regime_skips_the_ref_t2v_prefix_but_keeps_identity() {
        // `echo-e50` never saw `ref_t2v:`; the appearance clause is what routes it, so that half
        // must survive.
        let refs = vec![sheet(Some("dark curly hair, brown eyes"))];
        assert_eq!(
            ltx_character_prompt("a woman walks", &refs, LtxIdentityRegime::Memory),
            "a woman walks. dark curly hair, brown eyes."
        );
    }

    #[test]
    fn the_lora_catalog_answers_for_files_it_has_never_heard_of() {
        // The whole point of the shape: an operator drops a new .gguf into the directory, adds it
        // to LTX_VIDEO_LORAS, and the picker keeps working without a koblibs change.
        let unknown = LtxLoraInfo::for_path("someone-elses-lora-v9.gguf");
        assert_eq!(unknown.label, "someone-elses-lora-v9.gguf");
        assert_eq!(unknown.default_multiplier, 1.0);
        assert!(unknown.trigger.is_none());
        assert_eq!(
            ltx_lora_default_multiplier("someone-elses-lora-v9.gguf"),
            1.0
        );
    }

    /// 🔴 An adapter with no word on the subject is USABLE. Every C++ fleet engine has no
    /// co-tenant and never sends `for_this_model`; reading those as unusable empties the LoRA
    /// picker on all of them, and an empty picker reads as a configuration problem rather than as a
    /// deserialization default. Only a co-tenanted ComfyUI (`h3` + `ltx25` sharing one
    /// `models/loras`) ever answers `false`.
    #[test]
    fn an_adapter_that_says_nothing_about_its_model_is_usable() {
        let quiet: LtxLoraInfo =
            serde_json::from_str(r#"{"path":"a.gguf","label":"a","default_multiplier":1.0}"#)
                .expect("a listing without the flag must still parse");
        assert!(quiet.for_this_model);
        assert!(LtxLoraInfo::for_path("anything.gguf").for_this_model);

        let foreign: LtxLoraInfo = serde_json::from_str(
            r#"{"path":"h3-turbo.gguf","label":"h3","default_multiplier":1.0,"for_this_model":false}"#,
        )
        .expect("an explicit refusal must parse");
        assert!(!foreign.for_this_model);
    }

    #[test]
    fn catalogued_adapters_carry_their_trained_multiplier() {
        // 🔴 The two that are NOT 1.0. Both were silently rendering at the wrong strength before
        // the catalog existed, and neither failure is visible in a render that succeeds.
        assert_eq!(ltx_lora_default_multiplier(LTX_LORA_OMNINFT), 2.0);
        assert_eq!(ltx_lora_default_multiplier(LTX_LORA_FACEID_CLOSEUP), 0.2);
        assert_eq!(ltx_lora_default_multiplier(LTX_LORA_AUDIO_REACTIVE), 1.5);
        assert_eq!(ltx_lora_default_multiplier(LTX_LORA_MSR_V2), 1.0);
        // The stem fallback: the .safetensors an adapter was converted from resolves to the same
        // profile, so listing the wrong extension does not silently lose the multiplier.
        assert_eq!(
            ltx_lora_default_multiplier("omninft-2.3-diffconv.safetensors"),
            2.0
        );
        // Every catalogued file name must be unique — two entries for one file means whichever
        // comes first wins, silently.
        for (i, p) in LTX_LORA_CATALOG.iter().enumerate() {
            assert!(
                !LTX_LORA_CATALOG[..i]
                    .iter()
                    .any(|q| q.file.eq_ignore_ascii_case(p.file)),
                "duplicate catalog entry for {}",
                p.file
            );
        }
    }

    #[test]
    fn trigger_phrases_are_appended_once_and_only_for_attached_adapters() {
        // The transition adapter fires on `zhuanchang`, appended at the END so the description
        // still leads (valiantcat's README is explicit about the placement).
        assert_eq!(
            ltx_apply_lora_triggers("a woman turns to camera.", [LTX_LORA_TRANSITION]),
            "a woman turns to camera. zhuanchang"
        );
        // No terminal punctuation ⇒ comma-joined, which is how the upstream examples read.
        assert_eq!(
            ltx_apply_lora_triggers("a low-angle wide shot of a wet road", [LTX_LORA_TRANSITION]),
            "a low-angle wide shot of a wet road, zhuanchang"
        );
        // IDEMPOTENT: an author who typed it themselves does not get it twice, and neither does a
        // re-compiled plan.
        assert_eq!(
            ltx_apply_lora_triggers("a woman turns to camera. zhuanchang", [LTX_LORA_TRANSITION]),
            "a woman turns to camera. zhuanchang"
        );
        // Stacking two trigger-carrying adapters appends both, in attachment order.
        assert_eq!(
            ltx_apply_lora_triggers("a fox runs.", [LTX_LORA_TRANSITION, LTX_LORA_PIXAR_TOON]),
            "a fox runs. zhuanchang, P1x4r"
        );
        // An adapter with no trigger must not touch the prompt at all — including not trimming it
        // into a different string, which would move bytes on every render for no reason.
        let plain = "a fox runs.";
        assert_eq!(
            ltx_apply_lora_triggers(plain, [LTX_LORA_AUDIO_REACTIVE, LTX_LORA_MSR_V2]),
            plain
        );
        assert_eq!(ltx_apply_lora_triggers(plain, []), plain);
    }

    #[test]
    fn triggers_compose_with_the_ref_t2v_prefix() {
        // Append-only is not a style choice: a Sheet-regime prompt is GATED on `ref_t2v: ` being
        // the first thing the encoder sees, so a leading trigger would break identity instead.
        let refs = vec![sheet(Some("dark curly hair"))];
        let p = ltx_character_prompt("a woman walks", &refs, LtxIdentityRegime::Sheet);
        let out = ltx_apply_lora_triggers(&p, [LTX_LORA_TRANSITION]);
        assert!(out.starts_with(LTX_REF_T2V_PREFIX), "{out}");
        assert!(out.ends_with("zhuanchang"), "{out}");
        assert!(out.contains("dark curly hair"), "{out}");
    }

    /// ⚠️ The Echo doses are named by LITERAL here, not by a constant, and that is the point: the
    /// constants were product surface and went with LTX-2.3 in 2026-08. The `echo` branch of the
    /// resolver stays, because it must remain in step with `ltxIdentityRegime` in the editor and
    /// because a document saved before the retirement can still carry the name.
    #[test]
    fn regime_is_read_off_the_model_name() {
        assert_eq!(
            LtxIdentityRegime::for_model(Some("echo-e50")),
            LtxIdentityRegime::Memory
        );
        assert_eq!(
            LtxIdentityRegime::for_model(Some("echo-full")),
            LtxIdentityRegime::Memory
        );
        // Unrecognised (and absent) must stay on the historical path.
        assert_eq!(
            LtxIdentityRegime::for_model(Some("base")),
            LtxIdentityRegime::Sheet
        );
        assert_eq!(LtxIdentityRegime::for_model(None), LtxIdentityRegime::Sheet);
        // A folded MSR checkpoint is no longer registered, but an operator can re-register one;
        // falling through to Sheet would silently send the TAGGED layout and a ref_t2v prefix to
        // weights trained with neither.
        assert_eq!(
            LtxIdentityRegime::for_model(Some("msr-v2")),
            LtxIdentityRegime::Memory
        );
    }

    #[test]
    fn an_identity_adapter_supplies_the_regime_the_model_name_cannot() {
        // THE CASE THAT BROKE when msr-v2 stopped being a checkpoint: bare `base` is Sheet, but
        // `base` + the MSR adapter is a Memory render, and only the LoRA set says so.
        assert_eq!(
            LtxIdentityRegime::for_model(Some("base")),
            LtxIdentityRegime::Sheet
        );
        assert_eq!(
            LtxIdentityRegime::for_render(Some("base"), [LTX_LORA_MSR_V2]),
            LtxIdentityRegime::Memory
        );
        // MSR on an Echo dose — a legacy name plus today's adapter — stays Memory.
        assert_eq!(
            LtxIdentityRegime::for_render(Some("echo-e50"), [LTX_LORA_MSR_V2]),
            LtxIdentityRegime::Memory
        );
        // The sheet adapter is the only way to reach Sheet with a trained response now.
        assert_eq!(
            LtxIdentityRegime::for_render(Some("base"), [LTX_LORA_FACEID_SHEET]),
            LtxIdentityRegime::Sheet
        );
        // An ordinary effect adapter changes nothing about the contract.
        assert_eq!(
            LtxIdentityRegime::for_render(Some("base"), ["ltx2.3-audio-reactive-v2.gguf"]),
            LtxIdentityRegime::Sheet
        );
        assert_eq!(
            LtxIdentityRegime::for_render(Some("echo-full"), []),
            LtxIdentityRegime::Memory
        );
    }

    #[test]
    fn identity_training_follows_the_adapter_too() {
        // Bare base holds nothing, and the UI has to be able to say so.
        assert!(!LtxIdentityRegime::identity_is_trained(Some("base"), []));
        // ...but base + either identity adapter does.
        assert!(LtxIdentityRegime::identity_is_trained(
            Some("base"),
            [LTX_LORA_MSR_V2]
        ));
        assert!(LtxIdentityRegime::identity_is_trained(
            Some("base"),
            [LTX_LORA_FACEID_SHEET]
        ));
        assert!(LtxIdentityRegime::identity_is_trained(
            Some("base"),
            [LTX_LORA_FACEID_CLOSEUP]
        ));
        // An effect adapter is not identity training.
        assert!(!LtxIdentityRegime::identity_is_trained(
            Some("base"),
            ["ltx2.3-audio-reactive-v2.gguf", LTX_LORA_LIPDUB]
        ));
        // The Echo checkpoints were trained on their own, with no adapter at all.
        assert!(LtxIdentityRegime::identity_is_trained(Some("echo-e50"), []));
        assert!(LtxIdentityRegime::identity_is_trained(
            Some("echo-full"),
            []
        ));
    }

    #[test]
    fn memory_regime_is_untagged_not_merely_unset() {
        // 0 and "absent" are DIFFERENT: absent leaves the engine on 1.0 (the tagged layout).
        assert_eq!(
            LtxIdentityRegime::Memory.default_tass_phase_scale(),
            Some(0.0)
        );
        assert_eq!(LtxIdentityRegime::Sheet.default_tass_phase_scale(), None);
        assert_eq!(
            LtxIdentityRegime::Memory.resize_mode(),
            LtxRefResizeMode::MatchTarget
        );
    }

    #[test]
    fn ref_token_share_reproduces_the_measured_points() {
        // Both GPU-measured sheet points, to within rounding: 39.5% CAPTURED the framing,
        // 11.8% was clean. If this test drifts, the cost model no longer describes the engine.
        let captured = ltx_ref_token_share(768, 448, 49, 1, LtxIdentityRegime::Sheet);
        assert!(
            (captured - 0.395).abs() < 0.005,
            "expected ~0.395, got {captured}"
        );
        let clean = ltx_ref_token_share(1280, 704, 97, 1, LtxIdentityRegime::Sheet);
        assert!(
            (clean - 0.118).abs() < 0.005,
            "expected ~0.118, got {clean}"
        );
        // A match_target reference costs exactly one latent frame, whatever the resolution.
        let memory = ltx_ref_token_share(1280, 704, 129, 1, LtxIdentityRegime::Memory);
        assert!(
            (memory - 1.0 / 18.0).abs() < 1e-9,
            "expected 1/18, got {memory}"
        );
        assert_eq!(
            ltx_ref_token_share(1280, 704, 129, 0, LtxIdentityRegime::Memory),
            0.0
        );
    }

    #[test]
    fn segment_without_character_refs_serialises_unchanged() {
        // `character_refs` must not appear on an ordinary shot — the engine keeps its untagged
        // (bit-identical) path only when the field is absent entirely.
        let seg = LtxSegment {
            prompt: "a woman walks".into(),
            ..Default::default()
        };
        let v = serde_json::to_value(&seg).unwrap();
        assert!(v.get("character_refs").is_none());
        assert!(
            serde_json::to_value(LtxVideoGenerateRequest::default())
                .unwrap()
                .get("tass_phase_scale")
                .is_none()
        );
        assert!(v.get("frame_drop").is_none());
    }

    #[test]
    fn frame_drop_is_a_string_or_a_number_on_the_wire() {
        // The untagged shape is the whole contract: `"auto"`/`"off"` from the two-state control,
        // a bare integer from the escape hatch. A tagged object here would 400 every editor save.
        let parse = |s: &str| serde_json::from_str::<LtxFrameDrop>(s).unwrap();
        assert_eq!(
            parse("\"auto\""),
            LtxFrameDrop::Mode(LtxFrameDropMode::Auto)
        );
        assert_eq!(parse("\"off\""), LtxFrameDrop::Mode(LtxFrameDropMode::Off));
        assert_eq!(parse("3"), LtxFrameDrop::Frames(3));
        assert_eq!(
            serde_json::to_string(&LtxFrameDrop::Mode(LtxFrameDropMode::Off)).unwrap(),
            "\"off\""
        );
        assert_eq!(
            serde_json::to_string(&LtxFrameDrop::Frames(9)).unwrap(),
            "9"
        );

        // And the engine encoding, which is the only thing koblem actually sends.
        assert_eq!(LtxFrameDrop::default().engine_reference_head_trim(), -1);
        assert_eq!(
            LtxFrameDrop::Mode(LtxFrameDropMode::Off).engine_reference_head_trim(),
            0
        );
        assert_eq!(LtxFrameDrop::Frames(17).engine_reference_head_trim(), 17);
        // Above the engine's ceiling is a units mistake, not a request to trim a whole shot.
        assert_eq!(
            LtxFrameDrop::Frames(48_000).engine_reference_head_trim(),
            512
        );
    }

    /// A `#[serde(default = …)]` that disagrees with the type's `Default` is a
    /// trap: `..Default::default()` then silently opts a caller out of saving (or
    /// out of the "auto" model) while an omitted wire field opts them in.
    #[test]
    fn rust_defaults_agree_with_the_wire_defaults() {
        assert!(MusicGenerateRequest::default().save_song);
        assert!(SonggenGenerateRequest::default().save_song);
        assert!(Sa3GenerateRequest::default().save);
        assert!(AvatarGenerateRequest::default().save_clip);
        assert!(LtxVideoGenerateRequest::default().save_clip);
        assert!(LtxRelipRequest::default().save_clip);
        assert!(WanVaceGenerateRequest::default().save_clip);
        assert_eq!(ImageGenerateRequest::default().model, "auto");

        // Same value whether the field is omitted on the wire or left to Default.
        let music: MusicGenerateRequest = serde_json::from_value(json!({"caption": ""})).unwrap();
        assert_eq!(music.save_song, MusicGenerateRequest::default().save_song);
        let song: SonggenGenerateRequest = serde_json::from_value(json!({})).unwrap();
        assert_eq!(song.save_song, SonggenGenerateRequest::default().save_song);
        let image: ImageGenerateRequest = serde_json::from_value(json!({"prompt": ""})).unwrap();
        assert_eq!(image.model, ImageGenerateRequest::default().model);
    }

    // ── H3 reference roles — the ONE enum, and its wire strings ─────────────────

    /// These eleven strings are the wire, and the editor sends them by name
    /// (`web/src/lib/director/h3-roles.ts`). `as_str`, the serde representation and `from_wire`
    /// must all agree: a rename anywhere makes saved timelines read their references as the kind
    /// default, which is silent and only shows up as a weaker `summary:` prefix.
    #[test]
    fn h3_role_wire_strings_are_pinned_and_round_trip() {
        let all = [
            (H3ReferenceRole::FirstFrame, "first_frame"),
            (H3ReferenceRole::LastFrame, "last_frame"),
            (H3ReferenceRole::IdentitySource, "identity_source"),
            (H3ReferenceRole::FrameAnchor, "frame_anchor"),
            (H3ReferenceRole::Storyboard, "storyboard"),
            (H3ReferenceRole::EditSource, "edit_source"),
            (H3ReferenceRole::ContinuationSource, "continuation_source"),
            (H3ReferenceRole::StructureSource, "structure_source"),
            (H3ReferenceRole::VoiceTimbre, "voice_timbre"),
            (H3ReferenceRole::AudioReuse, "audio_reuse"),
            (H3ReferenceRole::AudioContinuity, "audio_continuity"),
        ];
        for (role, wire) in all {
            assert_eq!(role.as_str(), wire);
            assert_eq!(serde_json::to_value(role).unwrap(), json!(wire));
            assert_eq!(H3ReferenceRole::from_wire(wire), Some(role));
            assert_eq!(
                serde_json::from_value::<H3ReferenceRole>(json!(wire)).unwrap(),
                role
            );
        }
    }

    /// The eleven prompt-craft roles collapse to the engine's five (`app/contract.py::RefRole`).
    /// Getting this mapping wrong is not a mislabel — `first_frame`/`last_frame` select a DIFFERENT
    /// CHECKPOINT, and `continuation_source`/`reuse` change what conditioning the shim builds.
    #[test]
    fn h3_engine_role_collapses_to_the_five_the_shim_knows() {
        assert_eq!(H3ReferenceRole::FirstFrame.engine_role(), "first_frame");
        assert_eq!(H3ReferenceRole::LastFrame.engine_role(), "last_frame");
        assert_eq!(
            H3ReferenceRole::ContinuationSource.engine_role(),
            "continuation_source"
        );
        assert_eq!(H3ReferenceRole::AudioReuse.engine_role(), "reuse");
        for role in [
            H3ReferenceRole::IdentitySource,
            H3ReferenceRole::FrameAnchor,
            H3ReferenceRole::Storyboard,
            H3ReferenceRole::EditSource,
            H3ReferenceRole::StructureSource,
            H3ReferenceRole::VoiceTimbre,
            H3ReferenceRole::AudioContinuity,
        ] {
            assert_eq!(role.engine_role(), "reference", "{role:?}");
        }
        // Only the two keyframe roles are the FL2VA checkpoint.
        assert!(H3ReferenceRole::FirstFrame.is_keyframe());
        assert!(H3ReferenceRole::LastFrame.is_keyframe());
        assert!(!H3ReferenceRole::FrameAnchor.is_keyframe());
    }

    // ── H3 constants vs. the engine ─────────────────────────────────────────

    /// Build the capabilities document the mirror table DESCRIBES, so the mechanics of
    /// [`h3_capability_drift`] are tested without pinning a snapshot — a snapshot would be a THIRD
    /// copy of every number and would go stale for exactly the reason the constants did.
    fn h3_caps_from_mirrors() -> serde_json::Value {
        let mut doc = json!({});
        for &(_, ptr, value) in H3_CAPABILITY_MIRRORS {
            let keys: Vec<&str> = ptr.trim_start_matches('/').split('/').collect();
            let mut node = &mut doc;
            for key in &keys[..keys.len() - 1] {
                node = node
                    .as_object_mut()
                    .unwrap()
                    .entry((*key).to_string())
                    .or_insert_with(|| json!({}));
            }
            let last = keys[keys.len() - 1].to_string();
            node.as_object_mut().unwrap().insert(last, json!(value));
        }
        doc
    }

    #[test]
    fn h3_capability_mirror_pointers_are_unique_and_well_formed() {
        let mut seen = std::collections::HashSet::new();
        for &(konst, ptr, _) in H3_CAPABILITY_MIRRORS {
            assert!(
                ptr.starts_with('/'),
                "{konst}: `{ptr}` is not a JSON Pointer"
            );
            assert!(seen.insert(ptr), "two constants claim to mirror `{ptr}`");
        }
    }

    #[test]
    fn h3_capability_drift_is_clean_when_the_engine_agrees() {
        let drift = h3_capability_drift(&h3_caps_from_mirrors());
        assert!(drift.is_clean(), "{drift:?}");
    }

    /// 🔴 THE REGRESSION THIS EXISTS FOR. `H3_MAX_VIDEO_REFS` sat at 2 for the nine days after
    /// ComfyUI replaced WanGP, refusing a third reference the engine accepted — and a client cap
    /// BELOW the engine's is an error nothing reports, because the request never reaches the
    /// engine to be accepted.
    #[test]
    fn h3_capability_drift_catches_a_cap_below_the_engine() {
        let mut caps = h3_caps_from_mirrors();
        caps["references"]["max_video"] = json!(H3_MAX_VIDEO_REFS + 1);
        let drift = h3_capability_drift(&caps);
        assert_eq!(drift.mismatched.len(), 1, "{drift:?}");
        assert!(
            drift.mismatched[0].contains("H3_MAX_VIDEO_REFS"),
            "{drift:?}"
        );
        assert!(drift.unpublished.is_empty(), "{drift:?}");
    }

    /// A field the engine stopped publishing must read as UNVERIFIABLE, never as agreement.
    #[test]
    fn h3_capability_drift_reports_a_field_the_engine_does_not_publish() {
        let mut caps = h3_caps_from_mirrors();
        caps["references"]
            .as_object_mut()
            .unwrap()
            .remove("max_audio");
        let drift = h3_capability_drift(&caps);
        assert!(drift.mismatched.is_empty(), "{drift:?}");
        assert_eq!(drift.unpublished.len(), 1, "{drift:?}");
        assert!(
            drift.unpublished[0].contains("H3_MAX_AUDIO_REFS"),
            "{drift:?}"
        );
    }

    #[test]
    fn h3_capability_drift_on_an_empty_document_is_all_unpublished() {
        let drift = h3_capability_drift(&json!({}));
        assert!(drift.mismatched.is_empty(), "{drift:?}");
        assert_eq!(drift.unpublished.len(), H3_CAPABILITY_MIRRORS.len());
    }

    #[test]
    fn h3_cap_prefers_the_engine_and_falls_back_to_the_constant() {
        let caps = json!({"references": {"max_video": 5}});
        assert_eq!(
            h3_cap_u64_or(Some(&caps), "/references/max_video", 3),
            5,
            "the live document is the authority"
        );
        assert_eq!(
            h3_cap_u64_or(Some(&caps), "/references/max_audio", 3),
            3,
            "an unpublished field falls back"
        );
        assert_eq!(
            h3_cap_u64_or(None, "/references/max_video", 3),
            3,
            "no document at all falls back"
        );
        // Nonsense from a broken engine is not a limit.
        let junk = json!({"references": {"max_video": -1}});
        assert_eq!(h3_cap_u64_or(Some(&junk), "/references/max_video", 3), 3);
    }

    /// 🔴 [`H3_TRIM_JOIN`] is `frames_for_latents(K)` frozen at K=7, and nothing enforces K=7.
    /// This is the thing that notices if the default moves.
    #[test]
    fn h3_trim_join_is_the_frozen_value_of_its_own_function() {
        assert_eq!(h3_trim_join(H3_CONTINUATION_K_DEFAULT), H3_TRIM_JOIN);
        assert_eq!(H3_CONTINUATION_K_DEFAULT, 7);
        assert_eq!(H3_TRIM_JOIN, 22);
        // The legal depths are 2 mod 5 and each trims a different number of frames — which is
        // exactly why freezing the number is only safe while K is fixed.
        assert_eq!(h3_trim_join(2), 5);
        assert_eq!(h3_trim_join(12), 39);
    }

    /// 🔴 THE REFERENCE-VIDEO FLOOR IS 5, AND IT WAS 48 UNTIL 2026-08-21.
    ///
    /// 48 was 2.0 s at 24 fps out of WanGP's `validate_generative_settings`. The ComfyUI node
    /// raises below 5, in its own error string, and has no other length check. The comment that
    /// set this to 48 explicitly "corrected" a previous 5 — the correction was right for WanGP and
    /// wrong again for the backend that renders now.
    #[test]
    fn h3_reference_video_floor_is_the_nodes_own_five() {
        assert_eq!(H3_MIN_REF_VIDEO_FRAMES, 5);
        // 5 is itself on the 5+17k grid, so the floor needs no snapping.
        assert_eq!(h3_effective_ref_video_frames(5, 362), 5);
    }

    /// The ceiling is the GENERATION's own length, which is why it is a function and not a
    /// constant: `h3_max_ref_video_frames`.
    #[test]
    fn a_long_reference_is_truncated_to_the_generation_then_snapped() {
        assert_eq!(h3_max_ref_video_frames(107), 107);
        // 300 frames into a 107-frame generation: truncate to 107, which is already 5 + 17*6.
        assert_eq!(h3_effective_ref_video_frames(300, 107), 107);
        // Into a 100-frame generation: truncate to 100, then snap DOWN to 90.
        assert_eq!(h3_effective_ref_video_frames(300, 100), 90);
        // Order matters — snapping first would give 90 then truncate to 90 by luck here, but
        // against an off-grid generation the two orders diverge. 96 -> 90, not 96.
        assert_eq!(h3_effective_ref_video_frames(96, 362), 90);
        // Short enough to need nothing done to it.
        assert_eq!(h3_effective_ref_video_frames(56, 362), 56);
    }

    /// Under the floor there is no grid point at all, and the engine refuses rather than
    /// delivering a zero-length reference.
    #[test]
    fn under_the_floor_nothing_on_the_grid_is_reachable() {
        for frames in 0..5u32 {
            assert_eq!(h3_effective_ref_video_frames(frames, 362), 0, "{frames}");
        }
    }

    #[test]
    fn h3_frames_for_latents_inverts_latents_for_frames_on_the_grid() {
        for frames in h3_legal_tails() {
            let t = h3_latents_for_frames(frames);
            assert_eq!(h3_frames_for_latents(t), frames, "round trip at {frames}f");
        }
    }

    /// An unknown or legacy role must leave the document readable and fall back to the KIND's
    /// default. A hard parse error here is a project that will not open.
    #[test]
    fn an_unknown_h3_role_falls_back_to_the_kind_default() {
        let parse = |v: serde_json::Value| serde_json::from_value::<H3Reference>(v).unwrap();

        // The engine's catch-all has no single meaning, so it resolves per kind.
        let img = parse(json!({"kind": "image", "role": "reference", "image": "x"}));
        assert_eq!(img.role, None);
        assert_eq!(img.effective_role(), H3ReferenceRole::IdentitySource);
        let vid = parse(json!({"kind": "video", "role": "reference", "video": "x"}));
        assert_eq!(vid.effective_role(), H3ReferenceRole::StructureSource);
        let aud = parse(json!({"kind": "audio", "role": "reference", "audio": "x"}));
        assert_eq!(aud.effective_role(), H3ReferenceRole::VoiceTimbre);

        // `reuse` only ever applied to audio, so it has exactly one meaning and maps.
        assert_eq!(
            parse(json!({"kind": "audio", "role": "reuse", "audio": "x"})).effective_role(),
            H3ReferenceRole::AudioReuse
        );
        // Anything a newer client invents, and an empty string, are read as "not stated".
        for role in [json!("time_travel"), json!(""), serde_json::Value::Null] {
            let r = parse(json!({"kind": "image", "role": role, "image": "x"}));
            assert_eq!(r.role, None);
            assert_eq!(r.effective_role(), H3ReferenceRole::IdentitySource);
        }
        // An absent role never serialises back, so a round trip does not invent one.
        let bare = parse(json!({"kind": "image", "image": "x"}));
        assert!(serde_json::to_value(&bare).unwrap().get("role").is_none());
    }

    // ── H3 soundtrack slicing ───────────────────────────────────────

    /// The whole reason `H3_MAX_FRAMES_WITH_AUDIO` exists: typing 15 s snaps UP past the audio cap,
    /// and the grid has nothing between 345 and 362 to land on instead.
    #[test]
    fn fifteen_seconds_snaps_past_what_an_audio_reference_may_be() {
        let snapped = h3_snap_frames((15.0 * H3_FPS as f64) as u32);
        assert_eq!(snapped, 362);
        assert!(h3_seconds_from_frames(362) > H3_MAX_AUDIO_REF_SECONDS);
        // The cap is the largest on-grid length that a slice can legally cover.
        assert_eq!(H3_MAX_FRAMES_WITH_AUDIO, 345);
        assert!(h3_seconds_from_frames(H3_MAX_FRAMES_WITH_AUDIO) <= H3_MAX_AUDIO_REF_SECONDS);
        // And it really is the NEXT rung down — nothing between.
        assert_eq!(H3_MAX_FRAMES_WITH_AUDIO + 17, H3_MAX_FRAMES);
    }

    /// 🔴 The load-bearing one. A slice must start where the PICTURE it scores actually sits in the
    /// finished cut, which is not where the generation lengths would put it — every anchored seam
    /// drops [`H3_TRIM_JOIN`] frames. Getting this wrong drifts a whole seam's worth per join,
    /// always the same direction.
    ///
    /// ⚠️ Every expectation below is DERIVED from `H3_TRIM_JOIN` rather than written out. It used to
    /// hardcode the arithmetic for a 1-frame seam and went stale the day the constant became 22 —
    /// silently, because nothing runs this crate's tests: koblibs has no Makefile, and `cap-lints`
    /// means a downstream `make` never reaches them either.
    #[test]
    fn slice_offsets_track_the_delivered_clock_not_the_rendered_one() {
        let fps = H3_FPS as f64;
        let trim = H3_TRIM_JOIN as f64;
        let gen_frames = 345.0;
        // Three full-length generations; the second and third continue with an anchor.
        let gens = [(345, false, true), (345, true, true), (345, true, true)];
        let plan = h3_plan_soundtrack(&gens, 60.0);

        assert_eq!(plan.slices[0].song_offset_seconds, 0.0);
        assert_eq!(plan.slices[0].delivered_at_seconds, 0.0);

        // Generation 1 is delivered after gen 0's full length...
        assert!((plan.slices[1].delivered_at_seconds - gen_frames / fps).abs() < 1e-9);
        // ...but its slice reaches BACK by the seam, because those frames reproduce gen 0's tail and
        // the model renders them even though delivery drops them.
        assert!((plan.slices[1].song_offset_seconds - (gen_frames - trim) / fps).abs() < 1e-9);

        // The third generation has lost a seam to EACH of the two joins before it.
        assert!(
            (plan.slices[2].delivered_at_seconds - (2.0 * gen_frames - trim) / fps).abs() < 1e-9
        );
        assert!(
            (plan.slices[2].song_offset_seconds - (2.0 * gen_frames - 2.0 * trim) / fps).abs()
                < 1e-9
        );

        // The cut is two seams shorter than the sum of the generations.
        assert!((plan.delivered_seconds - (3.0 * gen_frames - 2.0 * trim) / fps).abs() < 1e-9);
    }

    /// The property that makes a single continuous mux correct: a surviving frame plays over the
    /// exact song moment it was conditioned on. Checked frame by frame across a seam.
    #[test]
    fn every_surviving_frame_hears_what_it_was_conditioned_on() {
        let fps = H3_FPS as f64;
        let gens = [(56, false, true), (56, true, true), (56, true, true)];
        let plan = h3_plan_soundtrack(&gens, 30.0);
        for (i, &(frames, anchored, _)) in gens.iter().enumerate() {
            let trim = if anchored { H3_TRIM_JOIN } else { 0 };
            let s = &plan.slices[i];
            for f in trim..frames {
                // Where the song says this rendered frame is...
                let conditioned_at = s.song_offset_seconds + f as f64 / fps;
                // ...and where the frame actually lands in the muxed cut.
                let delivered_at = s.delivered_at_seconds + (f - trim) as f64 / fps;
                assert!(
                    (conditioned_at - delivered_at).abs() < 1e-9,
                    "gen {i} frame {f}: conditioned {conditioned_at} vs delivered {delivered_at}"
                );
            }
        }
    }

    /// A generation the author opted out of, one under the 2 s floor, and one past the end of the
    /// song all decline a slice — and each says which, because a silent bed is indistinguishable
    /// from a slice that failed.
    #[test]
    fn skipped_generations_say_why() {
        // Generation 2 has no soundtrack reference in scope — the author took that shot out of it.
        let plan = h3_plan_soundtrack(
            &[(345, false, true), (345, true, false), (345, true, true)],
            60.0,
        );
        assert!(plan.slices[0].skipped.is_none());
        assert!(
            plan.slices[1]
                .skipped
                .as_deref()
                .unwrap()
                .contains("no soundtrack reference")
        );
        assert_eq!(plan.slices[1].slice_seconds, 0.0);
        assert!(plan.slices[2].skipped.is_none());

        // A generation under the 2 s audio floor.
        let short = h3_plan_soundtrack(&[(22, false, true)], 60.0);
        assert!(
            short.slices[0]
                .skipped
                .as_deref()
                .unwrap()
                .contains("under the")
        );

        // A song that runs out mid-timeline.
        let ran_out = h3_plan_soundtrack(&[(345, false, true), (345, true, true)], 15.0);
        assert!(
            ran_out.slices[1]
                .skipped
                .as_deref()
                .unwrap()
                .contains("song ends")
        );
    }

    /// No slice may ever exceed what WanGP accepts, even from a document packed before the cap.
    #[test]
    fn a_slice_never_exceeds_the_engines_ceiling() {
        let plan = h3_plan_soundtrack(&[(362, false, true), (362, true, true)], 600.0);
        for s in &plan.slices {
            assert!(
                s.slice_seconds <= H3_MAX_AUDIO_REF_SECONDS,
                "slice {} is {}s",
                s.generation,
                s.slice_seconds
            );
        }
    }

    /// Every `JobKind` round-trips through as_str/parse. Guards a new kind added
    /// to the enum without a match arm — the queue keys its adapter table on this
    /// string and an unparseable row is a job that never runs.
    #[test]
    fn job_kind_str_roundtrips() {
        for k in JobKind::ALL {
            assert_eq!(JobKind::parse(k.as_str()), Some(k));
        }
        assert_eq!(JobKind::parse("nope"), None);
    }

    /// Same for `JobState`, which is written to and read back from a TEXT column.
    #[test]
    fn job_state_str_roundtrips() {
        for st in [
            JobState::Queued,
            JobState::Running,
            JobState::Collecting,
            JobState::Done,
            JobState::Failed,
            JobState::Cancelled,
            JobState::Held,
        ] {
            assert_eq!(JobState::parse(st.as_str()), Some(st));
        }
    }

    /// `collecting` is live but not terminal and not pending: the GPU has been
    /// released, the artifact is landing, and the row is past the point where a
    /// cancel could do anything but lose it.
    #[test]
    fn collecting_is_live_and_uncancellable() {
        assert!(JobState::Collecting.is_live());
        assert!(!JobState::Collecting.is_terminal());
        assert!(!JobState::Collecting.is_pending());
        assert!(JobState::Running.is_live());
        assert!(JobState::Held.is_pending());
    }

    /// A blocked reason survives the wire as a tagged enum, so every surface —
    /// row, SSE frame, CLI — reads the same value rather than parsing a string.
    #[test]
    fn blocked_reason_is_tagged_on_the_wire() {
        let r = BlockedReason::LaneBusy {
            lane: "comfy-api".into(),
        };
        let json = serde_json::to_value(&r).unwrap();
        assert_eq!(json["reason"], "lane_busy");
        assert_eq!(json["lane"], "comfy-api");
        let back: BlockedReason = serde_json::from_value(json).unwrap();
        assert_eq!(back, r);
        assert_eq!(back.summary(), "comfy-api slot busy");
    }

    /// Only reasons an operator can actually do something about are worth waking
    /// someone for; a busy lane at 03:00 is the queue working correctly.
    #[test]
    fn only_operator_clearable_reasons_are_alertable() {
        assert!(
            BlockedReason::ServiceDisabled {
                service: "flux".into()
            }
            .is_operator_clearable()
        );
        assert!(BlockedReason::Paused.is_operator_clearable());
        assert!(
            !BlockedReason::LaneBusy {
                lane: "comfy-api".into()
            }
            .is_operator_clearable()
        );
        assert!(BlockedReason::EvictionCooldown.is_transient());
    }

    /// An absent progress field means UNKNOWN and must not serialize as a zero —
    /// a 0/0 step counter reads as finished.
    #[test]
    fn empty_progress_omits_its_fields() {
        let json = serde_json::to_value(JobProgress::default()).unwrap();
        assert_eq!(json, json!({}));
    }
}
