//! The Batch API: half-price model calls, answered within 24 hours.
//!
//! A stage (translate or review) first sends the requests it is about to make
//! as one batch job, waits for the answers and keeps them by request key. The
//! stage then runs as usual; each call takes its answer from what came back
//! instead of asking live. Splitting unusable replies, retrying broken lines
//! and the rest work unchanged: only those follow-up calls go out live.
//!
//! The job id is saved in the project before waiting, and answers are saved
//! as they're collected, so closing Yakusu (or pressing Stop) loses nothing:
//! the next run collects the job instead of paying for it again.

use super::*;
use crate::project::append_lines;
use serde::Deserialize;

/// Fewer requests than this aren't worth waiting for a batch: they go live.
const MIN_BATCH: usize = 3;

/// What a batch answered for one request.
pub(super) enum Prefetched {
    Text(String),
    Truncated,
    NoRoom,
    Refused,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Job {
    id: String,
    provider: String,
    model: String,
    stage: String,
    /// Request keys, in the order sent.
    keys: Vec<String>,
    submitted: f64,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct State {
    jobs: Vec<Job>,
}

/// One saved answer (a line of `batch-answers.jsonl`).
#[derive(Debug, Serialize, Deserialize)]
struct Saved {
    key: String,
    stage: String,
    /// text | truncated | noroom | refused
    kind: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    text: String,
}

fn provider_name(id: &str) -> &'static str {
    match id {
        "anthropic" => "Anthropic",
        "gemini" => "Google",
        _ => "the provider",
    }
}

impl Pipeline {
    /// Whether this run sends its bulk requests through the Batch API.
    pub(super) fn batch_on(&self, client: &dyn Llm) -> bool {
        self.s.batch_api && self.s.provider != "local" && client.batch_supported()
    }

    /// Whether the Batch API would be used with these settings (for estimates).
    pub fn batch_possible(&self) -> bool {
        if !self.s.batch_api {
            return false;
        }
        let base = self.s.base_urls.get(&self.s.provider).map(|b| b.trim().trim_end_matches('/')).unwrap_or("");
        match self.s.provider.as_str() {
            "anthropic" => true,
            "gemini" => base.is_empty() || base.ends_with("/openai"),
            _ => false,
        }
    }

    /// Requests sent through the Batch API whose answers haven't been collected.
    pub fn batch_waiting(&self) -> usize {
        self.load_state().jobs.iter().map(|j| j.keys.len()).sum()
    }

    fn state_path(&self) -> PathBuf {
        self.p.path("batch.json")
    }

    fn answers_path(&self) -> PathBuf {
        self.p.path("batch-answers.jsonl")
    }

    fn load_state(&self) -> State {
        std::fs::read(self.state_path()).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
    }

    fn save_state(&self, st: &State) -> Result<()> {
        crate::config::write_json_atomic(&self.state_path(), &json!(st))?;
        Ok(())
    }

    fn saved_answers(&self) -> Vec<Saved> {
        std::fs::read_to_string(self.answers_path())
            .unwrap_or_default()
            .lines()
            .filter_map(|l| serde_json::from_str(l).ok())
            .collect()
    }

    /// Answer `reqs` through the Batch API before the stage's calls are made.
    /// Collects jobs an earlier run sent first, so nothing is paid for twice.
    /// With the Batch API off, only collects jobs still out (they're paid for).
    pub(super) fn prefetch(&self, client: &dyn Llm, stage: &str, reqs: Vec<llm::BatchRequest>) -> Result<()> {
        if !client.batch_supported() {
            return Ok(());
        }
        let model = client.model().to_string();
        self.load_answers(stage);
        self.collect(client, stage)?;
        if !self.batch_on(client) {
            return Ok(());
        }
        let need: Vec<llm::BatchRequest> = {
            let have = self.prefetched.lock().unwrap();
            reqs.into_iter().filter(|r| !have.contains_key(&r.key)).collect()
        };
        if need.is_empty() {
            return Ok(());
        }
        if need.len() < MIN_BATCH {
            self.log(format!("Only {} request(s) left, so they're sent right away instead of as a batch.", need.len()));
            return Ok(());
        }
        // One tiny live call first: it catches a bad key, and the client
        // settles on request settings the API accepts (thinking, effort)
        // before they're copied into every request of the batch.
        match self.call(client, stage, &need[0].system, "Say OK.", 64) {
            Ok(_) | Err(Error::Truncated | Error::Refused) => {}
            Err(e) => return Err(e),
        }
        let need = self.fit_budget(client, need)?;
        let total = need.len();
        let mut st = self.load_state();
        let max_bytes = client.batch_max_bytes().max(1);
        let mut start = 0;
        while start < need.len() {
            // Split into jobs the provider accepts.
            let mut end = start;
            let mut bytes = 0;
            while end < need.len() {
                let b = need[end].system.len() + need[end].user.len() + 600;
                if end > start && (bytes + b > max_bytes || end - start >= 10_000) {
                    break;
                }
                bytes += b;
                end += 1;
            }
            let part = &need[start..end];
            self.prog(stage, 0, total, format!("Sending {} requests to the Batch API", fmt_n(total)));
            let id = client.batch_submit(part, &self.cancel).map_err(|e| self.batch_error(e))?;
            st.jobs.push(Job {
                id,
                provider: self.s.provider.clone(),
                model: model.clone(),
                stage: stage.into(),
                keys: part.iter().map(|r| r.key.clone()).collect(),
                submitted: now(),
            });
            self.save_state(&st)?;
            start = end;
        }
        self.log(format!(
            "Sent {} requests through {}'s Batch API (half price). Answers usually take minutes to an hour, at most \
             24 hours. You can close Yakusu meanwhile; it collects them when you run it again.",
            fmt_n(total),
            provider_name(&self.s.provider)
        ));
        self.collect(client, stage)
    }

    /// Wait for this provider's jobs and keep their answers.
    fn collect(&self, client: &dyn Llm, stage: &str) -> Result<()> {
        let mine = |j: &Job| j.provider == self.s.provider;
        let st = self.load_state();
        if st.jobs.iter().any(|j| !mine(j)) {
            self.log("A batch sent with another provider is still waiting; switch back to it to collect it.");
        }
        let mut pending: Vec<Job> = st.jobs.iter().filter(|j| mine(j)).cloned().collect();
        if pending.is_empty() {
            return Ok(());
        }
        let started = pending.iter().map(|j| j.submitted).fold(f64::MAX, f64::min);
        let total: usize = pending.iter().map(|j| j.keys.len()).sum();
        let mut waits = 0u32;
        loop {
            let mut finished = 0;
            let mut still = Vec::new();
            for job in pending {
                if self.cancel.load(Ordering::SeqCst) {
                    return Err(self.stopped_waiting());
                }
                let status = client.batch_status(&job.id, &self.cancel).map_err(|e| self.batch_error(e))?;
                if !status.done {
                    finished += status.finished;
                    still.push(job);
                    continue;
                }
                finished += job.keys.len();
                match status.failed {
                    Some(why) => self.log(format!("{why} Its requests will be sent again.")),
                    None => self.take_answers(client, &job)?,
                }
                let mut st = self.load_state();
                st.jobs.retain(|j| j.id != job.id);
                self.save_state(&st)?;
            }
            if still.is_empty() {
                self.load_answers(stage);
                return Ok(());
            }
            pending = still;
            let mins = ((now() - started) / 60.0).max(0.0) as u64;
            self.prog(
                stage,
                finished.min(total),
                total,
                format!(
                    "Waiting for the Batch API: {} of {} answered ({mins} min so far). You can close Yakusu; it \
                     collects the answers next time.",
                    fmt_n(finished.min(total)),
                    fmt_n(total)
                ),
            );
            // Check often at first, then once a minute.
            let wait = if waits < 8 { self.batch_poll } else { self.batch_poll * 2 };
            waits += 1;
            let end = std::time::Instant::now() + wait;
            while std::time::Instant::now() < end {
                if self.cancel.load(Ordering::SeqCst) {
                    return Err(self.stopped_waiting());
                }
                std::thread::sleep(Duration::from_millis(200).min(wait));
            }
        }
    }

    /// Download a finished job's answers, pay for them and save them.
    fn take_answers(&self, client: &dyn Llm, job: &Job) -> Result<()> {
        let got = client.batch_results(&job.id, &job.keys, &self.cancel).map_err(|e| self.batch_error(e))?;
        // Batch requests cost half the normal price.
        let cost = |u: &llm::Usage| self.price(&job.model).map(|p| llm::cost_at(&p, u) * 0.5).unwrap_or(0.0);
        let mut lines = String::new();
        let mut errors = 0;
        let mut first_error = String::new();
        for (key, ans) in &got {
            let (kind, text, usage) = match ans {
                Ok((t, u)) => ("text", t.clone(), Some(u)),
                Err(LlmError::Truncated(u)) => ("truncated", String::new(), Some(u)),
                Err(LlmError::NoRoom(u)) => ("noroom", String::new(), Some(u)),
                Err(LlmError::Refused(u)) => ("refused", String::new(), Some(u)),
                Err(LlmError::Fatal(m)) => {
                    errors += 1;
                    if first_error.is_empty() {
                        first_error = m.clone();
                    }
                    continue;
                }
            };
            if let Some(u) = usage {
                self.p.log_cost(&job.stage, &job.model, u, cost(u))?;
            }
            let saved = Saved { key: key.clone(), stage: job.stage.clone(), kind: kind.into(), text };
            lines += &serde_json::to_string(&saved).unwrap_or_default();
            lines.push('\n');
        }
        append_lines(&self.answers_path(), lines.as_bytes())?;
        let missing = job.keys.len().saturating_sub(got.len()) + errors;
        if missing > 0 && missing * 2 > job.keys.len() {
            return Err(Error::Llm(format!(
                "The Batch API couldn't answer most requests ({}). Turn off \"Batch API\" in Settings to send them \
                 one at a time.",
                if first_error.is_empty() { "no answers came back".to_string() } else { first_error }
            )));
        }
        if missing > 0 {
            self.log(format!("{} batch request(s) failed; they'll be sent right away instead.", fmt_n(missing)));
        }
        Ok(())
    }

    /// Put the saved answers for `stage` where the stage's calls look.
    fn load_answers(&self, stage: &str) {
        let mut pre = self.prefetched.lock().unwrap();
        for s in self.saved_answers().into_iter().filter(|s| s.stage == stage) {
            let v = match s.kind.as_str() {
                "text" => Prefetched::Text(s.text),
                "truncated" => Prefetched::Truncated,
                "noroom" => Prefetched::NoRoom,
                "refused" => Prefetched::Refused,
                _ => continue,
            };
            pre.insert(s.key, v);
        }
    }

    /// The stage finished: forget its answers (they're all used or stale).
    pub(super) fn batch_done(&self, stage: &str) {
        self.prefetched.lock().unwrap().clear();
        let rest: Vec<Saved> = self.saved_answers().into_iter().filter(|s| s.stage != stage).collect();
        let path = self.answers_path();
        if rest.is_empty() {
            let _ = std::fs::remove_file(&path);
        } else {
            let text: String = rest.iter().map(|s| serde_json::to_string(s).unwrap_or_default() + "\n").collect();
            let _ = std::fs::write(&path, text);
        }
    }

    /// Keep a batch under the spending limit: requests that don't fit are
    /// left out (they'd stop at the limit anyway).
    fn fit_budget(&self, client: &dyn Llm, reqs: Vec<llm::BatchRequest>) -> Result<Vec<llm::BatchRequest>> {
        let cap = self.s.budget_usd;
        if cap <= 0.0 {
            return Ok(reqs);
        }
        let room = cap - self.p.spent_total() - *self.reserved.lock().unwrap();
        let model = client.model();
        let mut sum = 0.0;
        let mut keep = Vec::new();
        let n = reqs.len();
        for r in reqs {
            let worst = self.worst_cost(model, &r.system, &r.user, client.max_output(r.max_tokens))? * 0.5;
            if sum + worst > room {
                break;
            }
            sum += worst;
            keep.push(r);
        }
        if keep.is_empty() {
            return Err(Error::Budget(format!(
                "Stopped at your ${cap:.2} spending limit (spent ${:.4}). Raise it in Settings to continue. Progress \
                 is kept.",
                self.p.spent_total()
            )));
        }
        if keep.len() < n {
            self.log(format!(
                "Only {} of {} requests fit under your spending limit; the batch carries those.",
                fmt_n(keep.len()),
                fmt_n(n)
            ));
        }
        Ok(keep)
    }

    fn batch_error(&self, e: LlmError) -> Error {
        match e {
            LlmError::Fatal(_) if self.cancel.load(Ordering::SeqCst) => self.stopped_waiting(),
            other => Error::Llm(format!("Batch API: {other}")),
        }
    }

    fn stopped_waiting(&self) -> Error {
        if !self.load_state().jobs.is_empty() {
            self.log(format!(
                "Stopped waiting. The batch keeps running at {}; run the translation again to collect it (it isn't \
                 paid for twice).",
                provider_name(&self.s.provider)
            ));
        }
        Error::Cancelled
    }
}
