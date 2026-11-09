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
}

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
    fn thinks(self) -> bool {
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
            }),
            think: Mutex::new(Think::for_model(model)),
        }
    }

    /// The thinking setting in use (it adapts to what the API accepts).
    pub fn think(&self) -> Think {
        *self.think.lock().unwrap()
    }

    fn post(&self, url: &str, headers: &[(&str, &str)], body: &Value) -> Result<Value, Attempt> {
        let mut req = self.http.post(url).json(body);
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
            return serde_json::from_str(&text)
                .map_err(|_| Attempt::Transient("The API sent an unreadable reply.".into(), None));
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

    fn anthropic(&self, system: &str, user: &str, max_tokens: u32) -> Result<(String, Usage), Attempt> {
        let headers = [("x-api-key", self.api_key.as_str()), ("anthropic-version", ANTHROPIC_VERSION)];
        let mut data = None;
        // At most one request per thinking setting.
        for _ in 0..4 {
            let think = self.think();
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
            Some("max_tokens" | "model_context_window_exceeded") if text.trim().is_empty() => {
                Err(Attempt::NoRoom(usage))
            }
            Some("max_tokens" | "model_context_window_exceeded") => Err(Attempt::Truncated(usage)),
            _ => Ok((text, usage)),
        }
    }

    fn openai(&self, system: &str, user: &str, max_tokens: u32) -> Result<(String, Usage), Attempt> {
        let auth = format!("Bearer {}", self.api_key);
        let headers: Vec<(&str, &str)> =
            if self.api_key.is_empty() { vec![] } else { vec![("authorization", auth.as_str())] };
        let url = format!("{}/chat/completions", self.base_url);
        let mut data = None;
        for _ in 0..3 {
            let mut body = json!({
                "model": self.model,
                "messages": [{"role": "system", "content": system}, {"role": "user", "content": user}],
            });
            {
                let oa = self.oa.lock().unwrap();
                body[oa.tokens_key] = json!(max_tokens);
                if oa.temperature {
                    body["temperature"] = json!(0.3);
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
                    if low.contains("temperature") && oa.temperature {
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

impl Llm for Client {
    fn model(&self) -> &str {
        &self.model
    }

    fn max_output(&self, requested: u32) -> u32 {
        if self.kind == "anthropic" && self.think().thinks() {
            with_thinking_room(requested)
        } else {
            requested
        }
    }

    fn complete(&self, system: &str, user: &str, max_tokens: u32, cancel: &AtomicBool) -> Result<Completion, LlmError> {
        let mut delay = 2.0f64;
        let mut last = String::new();
        for _ in 0..self.max_attempts {
            let r = if self.kind == "anthropic" {
                self.anthropic(system, user, max_tokens)
            } else {
                self.openai(system, user, max_tokens)
            };
            match r {
                Ok((text, usage)) => return Ok(Completion { usd: cost_usd(&self.model, &usage), text, usage }),
                Err(Attempt::Truncated(u)) => return Err(LlmError::Truncated(u)),
                Err(Attempt::NoRoom(u)) => return Err(LlmError::NoRoom(u)),
                Err(Attempt::Refused(u)) => return Err(LlmError::Refused(u)),
                Err(Attempt::Fatal(m)) | Err(Attempt::BadRequest(m)) => return Err(LlmError::Fatal(m)),
                Err(Attempt::Transient(m, retry_after)) => {
                    last = m;
                    let wait = retry_after.unwrap_or(delay).clamp(0.5, 120.0);
                    (self.sleep)(wait, cancel);
                    if cancel.load(Ordering::Relaxed) {
                        return Err(LlmError::Fatal("Stopped.".into()));
                    }
                    delay = (delay * 2.0).min(60.0);
                }
            }
        }
        Err(LlmError::Fatal(format!("Gave up after {} tries: {last}", self.max_attempts)))
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
            format!("Can't reach the local model at {base}. Start the server in LM Studio (Developer tab), then try again.")
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
        .map(|a| a.iter().filter_map(|m| m.get("id").and_then(Value::as_str)).map(String::from).collect())
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
            if let Some(h) = &self.hook {
                if let Some(r) = h(system, user) {
                    return r.map(|text| {
                        let usage = Usage { input: user.len() as u64 / 2, output: 200, ..Default::default() };
                        Completion { usd: cost_usd(&self.model, &usage), text, usage }
                    });
                }
            }
            let usage = Usage { input: user.len() as u64 / 2, output: 200, ..Default::default() };
            Ok(Completion { text: reply(system, user), usd: cost_usd(&self.model, &usage), usage })
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
