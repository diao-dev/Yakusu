//! Model clients: the Anthropic Messages API and any OpenAI-compatible
//! endpoint (OpenAI, OpenRouter, DeepSeek, LM Studio, Ollama, ...).

use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::Duration;

pub const ANTHROPIC_URL: &str = "https://api.anthropic.com/v1/messages";
pub const ANTHROPIC_VERSION: &str = "2023-06-01";

/// USD per million tokens: (input, output, cache write 5m, cache read).
pub const PRICES: &[(&str, [f64; 4])] = &[
    ("claude-haiku-5-5", [0.10, 0.50, 0.125, 0.01]),
    ("claude-sonnet-5-5", [2.00, 10.00, 2.50, 0.10]),
    ("claude-opus-5-5", [4.00, 20.00, 5.00, 0.20]),
    // Gemini (ai.google.dev/gemini-api/docs/pricing, Nov 2026). Newer models:
    // enter the price in Settings.
    ("gemini-3.1-flash-lite", [0.25, 1.50, 0.25, 0.25]),
    ("gemini-3.5-flash", [1.50, 9.00, 1.50, 1.50]),
    ("gemini-3-flash-preview", [0.50, 3.00, 0.50, 0.50]),
    ("gemini-2.5-flash", [0.30, 2.50, 0.30, 0.30]),
    ("gemini-2.5-flash-lite", [0.10, 0.40, 0.10, 0.10]),
];

pub fn price_of(model: &str) -> Option<[f64; 4]> {
    PRICES.iter().find(|(m, _)| *m == model).map(|(_, p)| *p)
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Usage {
    pub input: u64,
    pub output: u64,
    pub cache_write: u64,
    pub cache_read: u64,
}

/// Cost of `u` at prices `p` (USD per million tokens).
pub fn cost_at(p: &[f64; 4], u: &Usage) -> f64 {
    (u.input as f64 * p[0] + u.output as f64 * p[1] + u.cache_write as f64 * p[2] + u.cache_read as f64 * p[3]) / 1e6
}

/// Cost at the built-in price, `None` when the price isn't known. An unknown
/// price is not a price of zero: callers that enforce a spending limit must
/// refuse rather than count it as free.
pub fn cost_usd(model: &str, u: &Usage) -> Option<f64> {
    price_of(model).map(|p| cost_at(&p, u))
}

pub struct Provider {
    pub id: &'static str,
    pub label: &'static str,
    pub base_url: &'static str,
    pub kind: &'static str,
    pub models: &'static [&'static str],
    pub key_hint: &'static str,
    pub key_url: &'static str,
}

pub const PROVIDERS: &[Provider] = &[
    Provider {
        id: "anthropic",
        label: "Claude (Anthropic)",
        base_url: ANTHROPIC_URL,
        kind: "anthropic",
        models: &["claude-haiku-5-5", "claude-sonnet-5-5", "claude-opus-5-5"],
        key_hint: "sk-ant-...",
        key_url: "https://console.anthropic.com/settings/keys",
    },
    Provider {
        id: "gemini",
        label: "Gemini (Google)",
        // Google's OpenAI-compatible endpoint.
        base_url: "https://generativelanguage.googleapis.com/v1beta/openai",
        kind: "openai",
        // The first one is used when no model has been chosen.
        models: &["gemini-3.1-flash-lite", "gemini-3.5-flash", "gemini-3.8-flash"],
        key_hint: "AIza...",
        key_url: "https://aistudio.google.com/apikey",
    },
    Provider {
        id: "openai",
        label: "OpenAI",
        base_url: "https://api.openai.com/v1",
        kind: "openai",
        models: &[],
        key_hint: "sk-...",
        key_url: "https://platform.openai.com/api-keys",
    },
    Provider {
        id: "openrouter",
        label: "OpenRouter",
        base_url: "https://openrouter.ai/api/v1",
        kind: "openai",
        models: &[],
        key_hint: "sk-or-...",
        key_url: "https://openrouter.ai/keys",
    },
    Provider {
        id: "local",
        label: "Local model (LM Studio / Ollama)",
        base_url: "http://localhost:1234/v1",
        kind: "openai",
        models: &[],
        key_hint: "not needed",
        key_url: "",
    },
];

pub fn provider(id: &str) -> &'static Provider {
    PROVIDERS.iter().find(|p| p.id == id).unwrap_or(&PROVIDERS[1])
}

pub fn providers_json() -> Value {
    let mut m = serde_json::Map::new();
    for p in PROVIDERS {
        m.insert(
            p.id.into(),
            json!({"label": p.label, "base_url": p.base_url, "kind": p.kind, "models": p.models,
                   "key_hint": p.key_hint, "key_url": p.key_url}),
        );
    }
    Value::Object(m)
}

pub fn prices_json() -> Value {
    let mut m = serde_json::Map::new();
    for (k, p) in PRICES {
        m.insert((*k).into(), json!(p));
    }
    Value::Object(m)
}

#[derive(Debug, Clone)]
pub enum LlmError {
    /// Retrying won't help (bad key, no credit, gave up after retries).
    Fatal(String),
    /// The reply hit the token limit. The caller should split the work.
    Truncated(Usage),
    /// The token limit was used up before any answer was written (a model
    /// that thinks first spent it all on thinking). The caller should ask
    /// again with more room.
    NoRoom(Usage),
    /// The provider's safety system declined to answer.
    Refused(Usage),
}

impl std::fmt::Display for LlmError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LlmError::Fatal(m) => f.write_str(m),
            LlmError::Truncated(_) | LlmError::NoRoom(_) => f.write_str("The reply was cut off by the length limit."),
            LlmError::Refused(_) => f.write_str("The model declined to answer."),
        }
    }
}

pub struct Completion {
    pub text: String,
    pub usage: Usage,
    /// `None` when the model's price isn't known.
    pub usd: Option<f64>,
}

/// Anything that can answer a prompt. The real clients talk HTTP; tests and
/// the dev server use [`fake::FakeLlm`].
pub trait Llm: Send + Sync {
    fn model(&self) -> &str;
    /// The output-token limit actually sent for a request asking for
    /// `requested` tokens of answer: models that think before answering get
    /// extra room, because thinking counts against the same limit.
    fn max_output(&self, requested: u32) -> u32 {
        requested
    }
    fn complete(&self, system: &str, user: &str, max_tokens: u32, cancel: &AtomicBool) -> Result<Completion, LlmError>;

    /// Whether requests can go through the provider's Batch API (half price,
    /// answered within 24 hours instead of right away).
    fn batch_supported(&self) -> bool {
        false
    }
    /// Most bytes of requests one batch job may carry.
    fn batch_max_bytes(&self) -> usize {
        0
    }
    /// Send a batch job; returns its id.
    fn batch_submit(&self, _reqs: &[BatchRequest], _cancel: &AtomicBool) -> Result<String, LlmError> {
        Err(LlmError::Fatal(NO_BATCH.into()))
    }
    fn batch_status(&self, _id: &str, _cancel: &AtomicBool) -> Result<BatchStatus, LlmError> {
        Err(LlmError::Fatal(NO_BATCH.into()))
    }
    /// The answers of a finished job, by request key. `keys` are the keys sent,
    /// in order (for providers that don't echo them back).
    fn batch_results(
        &self,
        _id: &str,
        _keys: &[String],
        _cancel: &AtomicBool,
    ) -> Result<Vec<(String, BatchAnswer)>, LlmError> {
        Err(LlmError::Fatal(NO_BATCH.into()))
    }
}

const NO_BATCH: &str = "This provider has no Batch API.";

/// One request in a Batch API job.
#[derive(Clone, Debug)]
pub struct BatchRequest {
    /// Up to 64 of `[A-Za-z0-9_-]` (Anthropic's rule for custom ids).
    pub key: String,
    pub system: String,
    pub user: String,
    pub max_tokens: u32,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct BatchStatus {
    /// Nothing more will happen to the job.
    pub done: bool,
    /// Requests finished so far, when the provider says.
    pub finished: usize,
    /// The whole job ended without answers (expired, cancelled, failed).
    pub failed: Option<String>,
}

/// One request's answer from a batch: like a live call's, `Fatal` when that
/// request errored or expired.
pub type BatchAnswer = Result<(String, Usage), LlmError>;

/// Builds a client for (provider, key, model, base_url).
pub type ClientFactory = Arc<dyn Fn(&str, &str, &str, Option<&str>) -> Arc<dyn Llm> + Send + Sync>;

pub fn http_factory() -> ClientFactory {
    Arc::new(|prov, key, model, base| Arc::new(Client::new(prov, key, model, base)) as Arc<dyn Llm>)
}

// ---------------------------------------------------------------- HTTP client

enum Attempt {
    Fatal(String),
    Transient(String, Option<f64>),
    BadRequest(String),
    Truncated(Usage),
    NoRoom(Usage),
    Refused(Usage),
}

struct OaParams {
    tokens_key: &'static str,
    temperature: bool,
    /// `reasoning_effort` to ask for (Gemini thinks by default, and thinking
    /// is billed as output). Dropped if the server refuses it.
    effort: Option<&'static str>,
}

/// How a Claude request keeps thinking out of the way. Translation batches
/// don't need it, and thinking is billed as output and counts against
/// `max_tokens`. Each model family turns it down differently; a request the
/// API refuses moves one step along (see [`Think::next`]) and is sent again.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Think {
    /// `thinking: {"type": "disabled"}` (Haiku).
    Disabled,
    /// `thinking: {"type": "between_tools"}`: no thinking before the answer (Sonnet).
    BetweenTools,
    /// Thinking can't be turned off (Opus): ask for low effort, leave room.
    LowEffort,
    /// Send no thinking settings at all and leave room.
    Default,
}

impl Think {
    pub fn for_model(model: &str) -> Think {
        let m = model.to_lowercase();
        if m.contains("haiku") {
            Think::Disabled
        } else if m.contains("sonnet") {
            Think::BetweenTools
        } else {
            Think::LowEffort
        }
    }

    fn next(self) -> Option<Think> {
        match self {
            Think::Disabled => Some(Think::BetweenTools),
            Think::BetweenTools => Some(Think::LowEffort),
            Think::LowEffort => Some(Think::Default),
            Think::Default => None,
        }
    }

    /// The model may think before it answers.
    pub fn thinks(self) -> bool {
        matches!(self, Think::LowEffort | Think::Default)
    }
}

/// Extra output tokens allowed when the model may think first.
fn with_thinking_room(requested: u32) -> u32 {
    requested.saturating_mul(2).saturating_add(4000).min(64_000)
}

pub struct Client {
    pub provider: String,
    kind: &'static str,
    pub base_url: String,
    api_key: String,
    model: String,
    pub max_attempts: usize,
    /// Sleep between retries; stops early when cancelled (replaceable in tests).
    pub sleep: Box<dyn Fn(f64, &AtomicBool) + Send + Sync>,
    http: reqwest::blocking::Client,
    oa: Mutex<OaParams>,
    think: Mutex<Think>,
}

impl Client {
    pub fn new(provider: &str, api_key: &str, model: &str, base_url: Option<&str>) -> Client {
        let p = self::provider(provider);
        let base = base_url.filter(|b| !b.trim().is_empty()).unwrap_or(p.base_url).trim().trim_end_matches('/');
        // OpenAI's newer models want max_completion_tokens and reject a custom
        // temperature; other OpenAI-compatible servers want the classic names.
        let is_openai = provider == "openai";
        Client {
            provider: provider.into(),
            kind: p.kind,
            base_url: base.into(),
            api_key: api_key.trim().into(),
            model: model.into(),
            max_attempts: 6,
            sleep: Box::new(|secs, cancel| {
                // Sleep in slices so Stop doesn't wait out a long back-off.
                let end = std::time::Instant::now() + Duration::from_secs_f64(secs);
                while std::time::Instant::now() < end && !cancel.load(Ordering::Relaxed) {
                    std::thread::sleep(Duration::from_millis(200));
                }
            }),
            http: reqwest::blocking::Client::builder().timeout(Duration::from_secs(180)).build().expect("http client"),
            oa: Mutex::new(OaParams {
                tokens_key: if is_openai { "max_completion_tokens" } else { "max_tokens" },
                temperature: !is_openai,
                effort: if provider == "gemini" { Some("low") } else { None },
            }),
            think: Mutex::new(Think::for_model(model)),
        }
    }

    /// The thinking setting in use (it adapts to what the API accepts).
    pub fn think(&self) -> Think {
        *self.think.lock().unwrap()
    }

    fn post(&self, url: &str, headers: &[(&str, &str)], body: &Value) -> Result<Value, Attempt> {
        let text = self.send(self.http.post(url).json(body), headers)?;
        serde_json::from_str(&text).map_err(|_| Attempt::Transient("The API sent an unreadable reply.".into(), None))
    }

    fn get(&self, url: &str, headers: &[(&str, &str)]) -> Result<String, Attempt> {
        self.send(self.http.get(url), headers)
    }

    /// Send a request; the body of a 2xx reply, or the failure sorted by what
    /// to do about it.
    fn send(&self, req: reqwest::blocking::RequestBuilder, headers: &[(&str, &str)]) -> Result<String, Attempt> {
        let mut req = req;
        for (k, v) in headers {
            req = req.header(*k, *v);
        }
        let resp = match req.send() {
            Ok(r) => r,
            Err(e) => {
                if self.provider == "local" && e.is_connect() {
                    return Err(Attempt::Fatal(format!(
                        "Can't reach the local model at {}. Is LM Studio's server running?",
                        self.base_url
                    )));
                }
                if e.is_timeout() {
                    return Err(Attempt::Transient("The request timed out.".into(), None));
                }
                return Err(Attempt::Transient(format!("Network error: {e}"), None));
            }
        };
        let status = resp.status().as_u16();
        let retry_after =
            resp.headers().get("retry-after").and_then(|v| v.to_str().ok()).and_then(|v| v.trim().parse::<f64>().ok());
        let text = resp.text().unwrap_or_default();
        if (200..300).contains(&status) {
            return Ok(text);
        }
        let detail = serde_json::from_str::<Value>(&text)
            .ok()
            .map(|v| {
                v.pointer("/error/message").and_then(Value::as_str).map(String::from).unwrap_or_else(|| v.to_string())
            })
            .unwrap_or_default();
        let low = detail.to_lowercase();
        let with = |m: String| if detail.is_empty() { m } else { format!("{m} {detail}") };
        match status {
            401 | 403 => Err(Attempt::Fatal(with(format!(
                "The API key was rejected ({status}). Check it in the Translator step."
            )))),
            404 => {
                Err(Attempt::Fatal(with(format!("Model or endpoint not found: {} at {}.", self.model, self.base_url))))
            }
            _ if status == 402 || low.contains("credit balance") || low.contains("insufficient_quota") => {
                Err(Attempt::Fatal(with(
                    "Your API account is out of credit. Add credit with your provider, then continue.".into(),
                )))
            }
            400 => Err(Attempt::BadRequest(if detail.is_empty() { "bad request".into() } else { detail })),
            _ => {
                let label = match status {
                    429 => "Rate limited".to_string(),
                    529 => "The API is overloaded".into(),
                    500 => "Server error".into(),
                    502 => "Bad gateway".into(),
                    503 => "Service unavailable".into(),
                    504 => "Gateway timeout".into(),
                    s => format!("HTTP {s}"),
                };
                Err(Attempt::Transient(with(format!("{label}.")), retry_after))
            }
        }
    }

    fn anthropic_headers(&self) -> [(&str, &str); 2] {
        [("x-api-key", self.api_key.as_str()), ("anthropic-version", ANTHROPIC_VERSION)]
    }

    /// The Messages API request body (shared by live calls and batches).
    fn anthropic_body(&self, system: &str, user: &str, max_tokens: u32, think: Think) -> Value {
        // No sampling parameters: current Claude models reject `temperature`.
        let mut body = json!({
            "model": self.model,
            "max_tokens": if think.thinks() { with_thinking_room(max_tokens) } else { max_tokens },
            // Instructions + style sheet + glossary are the same for every batch: cached.
            "system": [{"type": "text", "text": system, "cache_control": {"type": "ephemeral"}}],
            "messages": [{"role": "user", "content": user}],
        });
        match think {
            Think::Disabled => body["thinking"] = json!({"type": "disabled"}),
            Think::BetweenTools => body["thinking"] = json!({"type": "between_tools"}),
            Think::LowEffort => body["output_config"] = json!({"effort": "low"}),
            Think::Default => {}
        }
        body
    }

    fn anthropic(&self, system: &str, user: &str, max_tokens: u32) -> Result<(String, Usage), Attempt> {
        let headers = self.anthropic_headers();
        let mut data = None;
        // At most one request per thinking setting.
        for _ in 0..4 {
            let think = self.think();
            let body = self.anthropic_body(system, user, max_tokens, think);
            match self.post(&self.base_url, &headers, &body) {
                Ok(d) => {
                    data = Some(d);
                    break;
                }
                Err(Attempt::BadRequest(m)) => {
                    // This model wants thinking set another way: move one
                    // step along and send the request again.
                    let low = m.to_lowercase();
                    let about_thinking = low.contains("thinking") || low.contains("between_tools");
                    let about_effort = low.contains("effort") || low.contains("output_config");
                    let next = match think {
                        Think::LowEffort if about_effort => Some(Think::Default),
                        t if about_thinking && t != Think::LowEffort => t.next(),
                        _ => None,
                    };
                    match next {
                        Some(n) => *self.think.lock().unwrap() = n,
                        None => return Err(Attempt::Fatal(format!("The API refused the request: {m}"))),
                    }
                }
                Err(e) => return Err(e),
            }
        }
        let data = data.ok_or_else(|| Attempt::Fatal("The API refused the request parameters.".into()))?;
        if data.get("type").and_then(Value::as_str) == Some("error") {
            return Err(Attempt::Transient(data.get("error").map(|e| e.to_string()).unwrap_or_default(), None));
        }
        anthropic_read(&data)
    }

    fn openai(&self, system: &str, user: &str, max_tokens: u32) -> Result<(String, Usage), Attempt> {
        let auth = format!("Bearer {}", self.api_key);
        let headers: Vec<(&str, &str)> =
            if self.api_key.is_empty() { vec![] } else { vec![("authorization", auth.as_str())] };
        let url = format!("{}/chat/completions", self.base_url);
        let mut data = None;
        for _ in 0..4 {
            let mut body = json!({
                "model": self.model,
                "messages": [{"role": "system", "content": system}, {"role": "user", "content": user}],
            });
            {
                let oa = self.oa.lock().unwrap();
                body[oa.tokens_key] = json!(self.max_output(max_tokens));
                if oa.temperature {
                    body["temperature"] = json!(0.3);
                }
                if let Some(e) = oa.effort {
                    body["reasoning_effort"] = json!(e);
                }
            }
            match self.post(&url, &headers, &body) {
                Ok(d) => {
                    data = Some(d);
                    break;
                }
                Err(Attempt::BadRequest(m)) => {
                    // Adapt to what this server accepts, then try again.
                    let low = m.to_lowercase();
                    let mut oa = self.oa.lock().unwrap();
                    if low.contains("reasoning") && oa.effort.is_some() {
                        oa.effort = None;
                    } else if low.contains("temperature") && oa.temperature {
                        oa.temperature = false;
                    } else if low.contains("max_completion_tokens") && oa.tokens_key == "max_tokens" {
                        oa.tokens_key = "max_completion_tokens";
                    } else if low.contains("max_tokens") && oa.tokens_key == "max_completion_tokens" {
                        oa.tokens_key = "max_tokens";
                    } else {
                        return Err(Attempt::Fatal(format!("The API refused the request: {m}")));
                    }
                }
                Err(e) => return Err(e),
            }
        }
        let data = data.ok_or_else(|| Attempt::Fatal("The API refused the request parameters.".into()))?;
        let choice = data.pointer("/choices/0").cloned().ok_or_else(|| {
            let s = data.to_string();
            Attempt::Transient(format!("Unexpected response: {}", &s[..s.len().min(200)]), None)
        })?;
        let text = choice.pointer("/message/content").and_then(Value::as_str).unwrap_or("").to_string();
        let n = |k: &str| data.pointer(&format!("/usage/{k}")).and_then(Value::as_u64).unwrap_or(0);
        let usage = Usage { input: n("prompt_tokens"), output: n("completion_tokens"), cache_write: 0, cache_read: 0 };
        match choice.get("finish_reason").and_then(Value::as_str) {
            Some("content_filter") => Err(Attempt::Refused(usage)),
            // A reasoning model can spend the whole limit before it answers.
            Some("length") if text.trim().is_empty() => Err(Attempt::NoRoom(usage)),
            Some("length") => Err(Attempt::Truncated(usage)),
            _ => Ok((text, usage)),
        }
    }
}

/// Text and usage from a Messages API reply (live or from a batch).
fn anthropic_read(data: &Value) -> Result<(String, Usage), Attempt> {
    // Pick text blocks by type: a reply can start with thinking blocks.
    let text: String = data
        .get("content")
        .and_then(Value::as_array)
        .map(|blocks| {
            blocks
                .iter()
                .filter(|b| b.get("type").and_then(Value::as_str) == Some("text"))
                .filter_map(|b| b.get("text").and_then(Value::as_str))
                .collect()
        })
        .unwrap_or_default();
    let u = data.get("usage").cloned().unwrap_or(Value::Null);
    let n = |k: &str| u.get(k).and_then(Value::as_u64).unwrap_or(0);
    let usage = Usage {
        input: n("input_tokens"),
        output: n("output_tokens"),
        cache_write: n("cache_creation_input_tokens"),
        cache_read: n("cache_read_input_tokens"),
    };
    match data.get("stop_reason").and_then(Value::as_str) {
        Some("refusal") => Err(Attempt::Refused(usage)),
        Some("max_tokens" | "model_context_window_exceeded") if text.trim().is_empty() => Err(Attempt::NoRoom(usage)),
        Some("max_tokens" | "model_context_window_exceeded") => Err(Attempt::Truncated(usage)),
        _ => Ok((text, usage)),
    }
}

/// Text and usage from a Gemini `generateContent` reply (native API, as a
/// batch returns it). Thought parts are skipped; thinking is billed as output.
fn gemini_read(data: &Value) -> Result<(String, Usage), Attempt> {
    let u = data.get("usageMetadata").cloned().unwrap_or(Value::Null);
    let n = |k: &str| u.get(k).and_then(Value::as_u64).unwrap_or(0);
    let usage = Usage {
        input: n("promptTokenCount"),
        output: n("candidatesTokenCount") + n("thoughtsTokenCount"),
        cache_write: 0,
        cache_read: 0,
    };
    if data.pointer("/promptFeedback/blockReason").is_some() {
        return Err(Attempt::Refused(usage));
    }
    let Some(cand) = data.pointer("/candidates/0") else {
        return Err(Attempt::Transient("The reply had no answer in it.".into(), None));
    };
    let text: String = cand
        .pointer("/content/parts")
        .and_then(Value::as_array)
        .map(|parts| {
            parts
                .iter()
                .filter(|p| !p.get("thought").and_then(Value::as_bool).unwrap_or(false))
                .filter_map(|p| p.get("text").and_then(Value::as_str))
                .collect()
        })
        .unwrap_or_default();
    match cand.get("finishReason").and_then(Value::as_str) {
        Some("SAFETY" | "PROHIBITED_CONTENT" | "BLOCKLIST" | "SPII" | "RECITATION") => Err(Attempt::Refused(usage)),
        Some("MAX_TOKENS") if text.trim().is_empty() => Err(Attempt::NoRoom(usage)),
        Some("MAX_TOKENS") => Err(Attempt::Truncated(usage)),
        _ => Ok((text, usage)),
    }
}

impl Llm for Client {
    fn model(&self) -> &str {
        &self.model
    }

    fn max_output(&self, requested: u32) -> u32 {
        // Gemini's thinking counts against the same limit as the answer.
        if (self.kind == "anthropic" && self.think().thinks()) || self.provider == "gemini" {
            with_thinking_room(requested)
        } else {
            requested
        }
    }

    fn complete(&self, system: &str, user: &str, max_tokens: u32, cancel: &AtomicBool) -> Result<Completion, LlmError> {
        let (text, usage) = self.retrying(cancel, || {
            if self.kind == "anthropic" {
                self.anthropic(system, user, max_tokens)
            } else {
                self.openai(system, user, max_tokens)
            }
        })?;
        Ok(Completion { usd: cost_usd(&self.model, &usage), text, usage })
    }

    fn batch_supported(&self) -> bool {
        self.kind == "anthropic" || self.gemini_native().is_some()
    }

    fn batch_max_bytes(&self) -> usize {
        // Anthropic takes up to 256 MB per batch; Gemini up to 20 MB inline.
        if self.kind == "anthropic" {
            100 << 20
        } else {
            15 << 20
        }
    }

    fn batch_submit(&self, reqs: &[BatchRequest], cancel: &AtomicBool) -> Result<String, LlmError> {
        if self.kind == "anthropic" {
            let think = self.think();
            let requests: Vec<Value> = reqs
                .iter()
                .map(|r| {
                    json!({"custom_id": r.key,
                           "params": self.anthropic_body(&r.system, &r.user, r.max_tokens, think)})
                })
                .collect();
            let body = json!({ "requests": requests });
            let url = format!("{}/batches", self.base_url);
            let data = self.retrying(cancel, || self.post(&url, &self.anthropic_headers(), &body))?;
            return data
                .get("id")
                .and_then(Value::as_str)
                .map(String::from)
                .ok_or_else(|| LlmError::Fatal(format!("The batch wasn't accepted: {}", short(&data.to_string()))));
        }
        let base = self.gemini_native().ok_or_else(|| LlmError::Fatal(NO_BATCH.into()))?;
        let requests: Vec<Value> = reqs
            .iter()
            .map(|r| {
                json!({"request": self.gemini_body(&r.system, &r.user, r.max_tokens),
                       "metadata": {"key": r.key}})
            })
            .collect();
        let body = json!({"batch": {"display_name": "yakusu",
                                    "input_config": {"requests": {"requests": requests}}}});
        let url = format!("{base}/models/{}:batchGenerateContent", self.model);
        let key = self.api_key.clone();
        let data = self.retrying(cancel, || self.post(&url, &[("x-goog-api-key", key.as_str())], &body))?;
        data.get("name")
            .and_then(Value::as_str)
            .map(String::from)
            .ok_or_else(|| LlmError::Fatal(format!("The batch wasn't accepted: {}", short(&data.to_string()))))
    }

    fn batch_status(&self, id: &str, cancel: &AtomicBool) -> Result<BatchStatus, LlmError> {
        let data = self.batch_get(id, cancel)?;
        if self.kind == "anthropic" {
            let c = |k: &str| data.pointer(&format!("/request_counts/{k}")).and_then(Value::as_u64).unwrap_or(0);
            let finished = c("succeeded") + c("errored") + c("canceled") + c("expired");
            let done = data.get("processing_status").and_then(Value::as_str) == Some("ended");
            return Ok(BatchStatus { done, finished: finished as usize, failed: None });
        }
        let state = data
            .pointer("/metadata/state")
            .or_else(|| data.get("state"))
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let stats = data.pointer("/metadata/batchStats").or_else(|| data.get("batchStats"));
        let num = |k: &str| {
            stats
                .and_then(|s| s.get(k))
                .and_then(|v| v.as_u64().or_else(|| v.as_str().and_then(|s| s.parse().ok())))
                .unwrap_or(0)
        };
        let finished = (num("successfulRequestCount") + num("failedRequestCount")) as usize;
        let ended = ["SUCCEEDED", "FAILED", "CANCELLED", "EXPIRED"].iter().any(|e| state.ends_with(e));
        let done = ended || data.get("done").and_then(Value::as_bool).unwrap_or(false);
        let failed = (done && !state.ends_with("SUCCEEDED") && !state.is_empty())
            .then(|| format!("Gemini ended the batch ({}).", state.to_lowercase().replace('_', " ")));
        Ok(BatchStatus { done, finished, failed })
    }

    fn batch_results(
        &self,
        id: &str,
        keys: &[String],
        cancel: &AtomicBool,
    ) -> Result<Vec<(String, BatchAnswer)>, LlmError> {
        let data = self.batch_get(id, cancel)?;
        if self.kind == "anthropic" {
            let url = data
                .get("results_url")
                .and_then(Value::as_str)
                .ok_or_else(|| LlmError::Fatal("The batch has no results to download yet.".into()))?;
            let text = self.retrying(cancel, || self.get(url, &self.anthropic_headers()))?;
            let mut out = Vec::new();
            for line in text.lines().filter(|l| !l.trim().is_empty()) {
                let Ok(v) = serde_json::from_str::<Value>(line) else { continue };
                let Some(key) = v.get("custom_id").and_then(Value::as_str) else { continue };
                let r = &v["result"];
                let ans = match r.get("type").and_then(Value::as_str) {
                    Some("succeeded") => anthropic_read(&r["message"]).map_err(attempt_error),
                    Some("errored") => Err(LlmError::Fatal(
                        r.pointer("/error/error/message")
                            .and_then(Value::as_str)
                            .map(String::from)
                            .unwrap_or_else(|| short(&r.to_string())),
                    )),
                    other => Err(LlmError::Fatal(format!("The request was {}.", other.unwrap_or("lost")))),
                };
                out.push((key.to_string(), ans));
            }
            return Ok(out);
        }
        let items = find_inlined(&data).ok_or_else(|| LlmError::Fatal("The batch has no answers in it.".into()))?;
        Ok(items
            .iter()
            .enumerate()
            .filter_map(|(k, it)| {
                let key = it
                    .pointer("/metadata/key")
                    .and_then(Value::as_str)
                    .map(String::from)
                    .or_else(|| keys.get(k).cloned())?;
                let ans = match (it.get("response"), it.get("error")) {
                    (Some(r), _) => gemini_read(r).map_err(attempt_error),
                    (None, Some(e)) => Err(LlmError::Fatal(
                        e.get("message")
                            .and_then(Value::as_str)
                            .map(String::from)
                            .unwrap_or_else(|| short(&e.to_string())),
                    )),
                    _ => Err(LlmError::Fatal("No answer for this request.".into())),
                };
                Some((key, ans))
            })
            .collect())
    }
}

impl Client {
    /// Run `f` until it succeeds or fails for good, backing off on transient errors.
    fn retrying<T>(&self, cancel: &AtomicBool, f: impl Fn() -> Result<T, Attempt>) -> Result<T, LlmError> {
        let mut delay = 2.0f64;
        let mut last = String::new();
        for _ in 0..self.max_attempts {
            match f() {
                Ok(v) => return Ok(v),
                Err(Attempt::Transient(m, retry_after)) => {
                    last = m;
                    let wait = retry_after.unwrap_or(delay).clamp(0.5, 120.0);
                    (self.sleep)(wait, cancel);
                    if cancel.load(Ordering::Relaxed) {
                        return Err(LlmError::Fatal("Stopped.".into()));
                    }
                    delay = (delay * 2.0).min(60.0);
                }
                Err(e) => return Err(attempt_error(e)),
            }
        }
        Err(LlmError::Fatal(format!("Gave up after {} tries: {last}", self.max_attempts)))
    }

    /// Gemini's native API root, when this client talks to Gemini through
    /// its OpenAI-compatible address (batches only exist natively).
    fn gemini_native(&self) -> Option<String> {
        (self.provider == "gemini").then(|| self.base_url.strip_suffix("/openai").map(String::from)).flatten()
    }

    /// A Gemini `generateContent` request with the same settings live calls use.
    fn gemini_body(&self, system: &str, user: &str, max_tokens: u32) -> Value {
        let mut cfg = json!({"maxOutputTokens": self.max_output(max_tokens)});
        let oa = self.oa.lock().unwrap();
        if oa.temperature {
            cfg["temperature"] = json!(0.3);
        }
        if oa.effort.is_some() {
            // Gemini 2.x takes a thinking budget; newer models a thinking level.
            cfg["thinkingConfig"] = if self.model.contains("gemini-2") {
                json!({"thinkingBudget": 1024})
            } else {
                json!({"thinkingLevel": "low"})
            };
        }
        json!({
            "systemInstruction": {"parts": [{"text": system}]},
            "contents": [{"role": "user", "parts": [{"text": user}]}],
            "generationConfig": cfg,
        })
    }

    fn batch_get(&self, id: &str, cancel: &AtomicBool) -> Result<Value, LlmError> {
        let (url, headers): (String, Vec<(&str, &str)>) = if self.kind == "anthropic" {
            (format!("{}/batches/{id}", self.base_url), self.anthropic_headers().to_vec())
        } else {
            let base = self.gemini_native().ok_or_else(|| LlmError::Fatal(NO_BATCH.into()))?;
            (format!("{base}/{id}"), vec![("x-goog-api-key", self.api_key.as_str())])
        };
        let text = self.retrying(cancel, || self.get(&url, &headers))?;
        serde_json::from_str(&text).map_err(|_| LlmError::Fatal("The API sent an unreadable batch status.".into()))
    }
}

fn attempt_error(a: Attempt) -> LlmError {
    match a {
        Attempt::Truncated(u) => LlmError::Truncated(u),
        Attempt::NoRoom(u) => LlmError::NoRoom(u),
        Attempt::Refused(u) => LlmError::Refused(u),
        Attempt::Fatal(m) | Attempt::BadRequest(m) | Attempt::Transient(m, _) => LlmError::Fatal(m),
    }
}

fn short(s: &str) -> String {
    s.chars().take(300).collect()
}

/// The list of answers in a finished Gemini batch. Google's docs show it
/// under `response` and under `dest`, as `inlinedResponses` (sometimes
/// wrapped in an object of the same name).
fn find_inlined(v: &Value) -> Option<&Vec<Value>> {
    match v {
        Value::Object(m) => {
            if let Some(x) = m.get("inlinedResponses") {
                return match x {
                    Value::Array(a) => Some(a),
                    other => find_inlined(other),
                };
            }
            m.values().find_map(find_inlined)
        }
        Value::Array(a) => a.iter().find_map(find_inlined),
        _ => None,
    }
}

/// The model IDs a provider offers, for the model picker. For a local
/// server this is whatever is loaded in LM Studio / Ollama.
pub fn list_models(provider_id: &str, api_key: &str, base_url: Option<&str>) -> Result<Vec<String>, String> {
    let p = provider(provider_id);
    let base = base_url.filter(|b| !b.trim().is_empty()).unwrap_or(p.base_url).trim().trim_end_matches('/');
    let http = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(15))
        .build()
        .map_err(|e| format!("Couldn't start the network client: {e}"))?;
    let key = api_key.trim();
    let req = if p.kind == "anthropic" {
        let url = format!("{}/models?limit=100", base.trim_end_matches("/messages"));
        http.get(url).header("x-api-key", key).header("anthropic-version", ANTHROPIC_VERSION)
    } else {
        let req = http.get(format!("{base}/models"));
        if key.is_empty() {
            req
        } else {
            req.header("authorization", format!("Bearer {key}"))
        }
    };
    let resp = req.send().map_err(|e| {
        if provider_id == "local" {
            format!(
                "Can't reach the local model at {base}. Start the server in LM Studio (Developer tab), then try again."
            )
        } else {
            format!("Couldn't reach {}: {e}", p.label)
        }
    })?;
    let status = resp.status().as_u16();
    if status == 401 || status == 403 {
        return Err(format!("The API key was rejected ({status})."));
    }
    let v: Value = resp.json().map_err(|_| format!("{} sent an unreadable model list.", p.label))?;
    let mut ids: Vec<String> = v
        .get("data")
        .and_then(Value::as_array)
        // Gemini lists "models/gemini-..." but takes the bare name.
        .map(|a| {
            a.iter()
                .filter_map(|m| m.get("id").and_then(Value::as_str))
                .map(|id| id.strip_prefix("models/").unwrap_or(id).to_string())
                .collect()
        })
        .unwrap_or_default();
    ids.dedup();
    Ok(ids)
}

static THINK_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?s)<think>.*?</think>").unwrap());

/// Pull the JSON array out of a model reply (tolerates code fences and chatter).
pub fn parse_json_array(text: &str) -> Result<Vec<Value>, String> {
    let stripped = THINK_RE.replace_all(text, "");
    let mut t: &str = stripped.trim();
    if t.starts_with("```") {
        if let Some(nl) = t.find('\n') {
            t = &t[nl + 1..];
        }
        if let Some(end) = t.rfind("```") {
            t = &t[..end];
        }
    }
    let (a, b) = (t.find('['), t.rfind(']'));
    match (a, b) {
        (Some(a), Some(b)) if b > a => serde_json::from_str::<Vec<Value>>(&t[a..=b]).map_err(|e| e.to_string()),
        _ => Err("no JSON array in reply".into()),
    }
}

pub mod fake {
    //! A deterministic stand-in model. It "translates" with stock English
    //! words and keeps every placeholder, so the whole pipeline can run
    //! without a network or an API key.

    use super::*;
    use std::collections::HashMap;

    static PH: LazyLock<Regex> = LazyLock::new(|| Regex::new("⟦\\d+⟧").unwrap());

    pub fn translate_item(jp: &str) -> String {
        let codes: Vec<&str> = PH.find_iter(jp).map(|m| m.as_str()).collect();
        let words: Vec<&str> =
            "the cherry blossoms were falling quietly outside the window this morning".split(' ').collect();
        let n = (jp.chars().count() / 3).clamp(3, words.len());
        let mut body = words[..n].join(" ");
        body.replace_range(0..1, "T");
        if codes.is_empty() {
            format!("{body}.")
        } else {
            format!("{body} {} and then more.", codes.join(" "))
        }
    }

    pub type Hook = Box<dyn Fn(&str, &str) -> Option<Result<String, LlmError>> + Send + Sync>;

    pub struct FakeLlm {
        pub model: String,
        pub calls: Arc<Mutex<Vec<(String, String)>>>,
        /// Called first; `Some` overrides the normal reply.
        pub hook: Option<Arc<Hook>>,
        pub delay: Duration,
    }

    impl FakeLlm {
        pub fn new(model: &str) -> FakeLlm {
            FakeLlm { model: model.into(), calls: Default::default(), hook: None, delay: Duration::ZERO }
        }
    }

    fn json_lines(s: &str) -> Vec<Value> {
        s.lines().filter(|l| l.trim_start().starts_with('{')).filter_map(|l| serde_json::from_str(l).ok()).collect()
    }

    pub fn reply(system: &str, user: &str) -> String {
        if user == "Say OK." {
            return "OK".into();
        }
        if system.starts_with("You are the lead translator") {
            return json!({"summary": "A gentle school romance.", "style_sheet": "- Past tense narration.",
                "names": [{"jp": "桜", "en": "Sakura"}, {"jp": "大雅", "en": "Taiga"}], "terms": []})
            .to_string();
        }
        if system.starts_with("You give English names") {
            let arr: Vec<Value> = user
                .lines()
                .filter(|n| !n.trim().is_empty())
                .map(|n| {
                    let en = match n {
                        "桜" => "Sakura",
                        "大雅" => "Taiga",
                        _ => "Someone",
                    };
                    json!({"jp": n, "en": en})
                })
                .collect();
            return Value::Array(arr).to_string();
        }
        if let Some(pos) = user.find("TRANSLATE") {
            let items = json_lines(&user[pos..]);
            let arr: Vec<Value> = items
                .iter()
                .map(|it| json!({"i": it["i"], "en": translate_item(it["jp"].as_str().unwrap_or(""))}))
                .collect();
            return Value::Array(arr).to_string();
        }
        let items = json_lines(user);
        if system.starts_with("You are a literary subtext critic") {
            // Flags the first line of each batch with a looser rewrite.
            return match items.first() {
                Some(it) => {
                    let en = it["en"].as_str().unwrap_or("");
                    let better = en.replacen("The cherry blossoms", "Blossoms", 1);
                    json!([{"i": it["i"], "note": "Reads too literally.", "en": better}]).to_string()
                }
                None => "[]".into(),
            };
        }
        if system.starts_with("You fit English") {
            let arr: Vec<Value> = items
                .iter()
                .map(|it| {
                    let en = it["en"].as_str().unwrap_or("");
                    let head: Vec<&str> = en.split_whitespace().take(4).collect();
                    let codes: Vec<&str> = PH.find_iter(en).map(|m| m.as_str()).collect();
                    json!({"i": it["i"], "en": format!("{} {}", head.join(" "), codes.join(" "))})
                })
                .collect();
            return Value::Array(arr).to_string();
        }
        // Review: change the first line of each batch.
        match items.first() {
            Some(it) => {
                json!([{"i": it["i"], "en": it["en"].as_str().unwrap_or("").replace("quietly", "softly")}]).to_string()
            }
            None => "[]".into(),
        }
    }

    impl Llm for FakeLlm {
        fn model(&self) -> &str {
            &self.model
        }
        fn complete(&self, system: &str, user: &str, _max: u32, _cancel: &AtomicBool) -> Result<Completion, LlmError> {
            self.calls.lock().unwrap().push((system.chars().take(40).collect(), user.chars().take(60).collect()));
            if !self.delay.is_zero() {
                std::thread::sleep(self.delay);
            }
            let (text, usage) = self.answer(system, user)?;
            Ok(Completion { usd: cost_usd(&self.model, &usage), text, usage })
        }

        fn batch_supported(&self) -> bool {
            true
        }
        fn batch_max_bytes(&self) -> usize {
            1 << 20
        }
        fn batch_submit(&self, reqs: &[BatchRequest], _cancel: &AtomicBool) -> Result<String, LlmError> {
            let id = format!("fakebatch_{}", FAKE_JOB_N.fetch_add(1, Ordering::SeqCst));
            self.calls.lock().unwrap().push(("[submit]".into(), id.clone()));
            // A model named "...slow..." keeps its batches waiting (tests set them free).
            let checks = if self.model.contains("slow") { usize::MAX } else { 1 };
            FAKE_JOBS.lock().unwrap().insert(id.clone(), (reqs.to_vec(), checks));
            Ok(id)
        }
        fn batch_status(&self, id: &str, _cancel: &AtomicBool) -> Result<BatchStatus, LlmError> {
            let mut jobs = FAKE_JOBS.lock().unwrap();
            let (reqs, left) = jobs.get_mut(id).ok_or_else(|| LlmError::Fatal("no such batch".into()))?;
            if *left > 0 {
                *left -= 1;
                return Ok(BatchStatus { done: false, finished: 0, failed: None });
            }
            Ok(BatchStatus { done: true, finished: reqs.len(), failed: None })
        }
        fn batch_results(
            &self,
            id: &str,
            _keys: &[String],
            _cancel: &AtomicBool,
        ) -> Result<Vec<(String, BatchAnswer)>, LlmError> {
            let reqs = FAKE_JOBS.lock().unwrap().get(id).map(|j| j.0.clone()).unwrap_or_default();
            Ok(reqs
                .iter()
                .map(|r| {
                    self.calls.lock().unwrap().push((
                        format!("[batch] {}", r.system.chars().take(32).collect::<String>()),
                        r.user.chars().take(60).collect(),
                    ));
                    (r.key.clone(), self.answer(&r.system, &r.user))
                })
                .collect())
        }
    }

    /// Batch jobs sent to fake models: id -> (requests, status checks left
    /// before it's done). Shared by every fake client, like a real provider.
    pub static FAKE_JOBS: LazyLock<Mutex<HashMap<String, (Vec<BatchRequest>, usize)>>> =
        LazyLock::new(|| Mutex::new(HashMap::new()));
    static FAKE_JOB_N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

    impl FakeLlm {
        fn answer(&self, system: &str, user: &str) -> Result<(String, Usage), LlmError> {
            let usage = Usage { input: user.len() as u64 / 2, output: 200, ..Default::default() };
            if let Some(h) = &self.hook {
                if let Some(r) = h(system, user) {
                    return r.map(|text| (text, usage));
                }
            }
            Ok((reply(system, user), usage))
        }
    }

    /// A factory for [`FakeLlm`]s that share one call log and hook.
    pub fn factory(
        calls: Arc<Mutex<Vec<(String, String)>>>,
        hook: Option<Arc<Hook>>,
        delay: Duration,
    ) -> ClientFactory {
        Arc::new(move |_p, _k, model, _b| {
            Arc::new(FakeLlm { model: model.into(), calls: calls.clone(), hook: hook.clone(), delay }) as Arc<dyn Llm>
        })
    }
}
