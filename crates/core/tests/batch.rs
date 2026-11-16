//! The Batch API path of the pipeline, with the stand-in model's batches:
//! translate and review through batches, half-price costs, and picking a
//! batch back up after Stop (or closing the app) without paying twice.

mod common;
use common::*;
use std::sync::atomic::Ordering;
use std::time::Duration;
use yakusu_core::llm::{self, fake::FAKE_JOBS};
use yakusu_core::Error;

fn live_calls(calls: &Calls) -> Vec<String> {
    calls
        .lock()
        .unwrap()
        .iter()
        // The style sheet and speaker names are single small calls, always live.
        // (and so is the one-word check before a batch is sent).
        .filter(|(s, u)| {
            !s.starts_with('[')
                && !s.starts_with("You are the lead translator")
                && !s.starts_with("You give English")
                && u != "Say OK."
        })
        .map(|(s, u)| format!("{s} | {u}"))
        .collect()
}

#[test]
fn translate_and_review_go_through_batches_at_half_price() {
    let tmp = tempfile::tempdir().unwrap();
    let env = env(tmp.path());
    // Long enough that the review (30 lines a request) is worth a batch too.
    let mut second = String::from("*a\n");
    for i in 0..100 {
        second += &format!("【桜】「今日は{i}回目の朝だね。また会おう」[p]\n");
    }
    let game = make_game(&tmp.path().join("Batch Game"), &second);
    let (factory, calls) = fake();
    let mut s = settings("recommended", 3, 2);
    s.batch_api = true;
    let mut pipe = pipeline(&env, &game, s.clone(), factory);
    pipe.batch_poll = Duration::from_millis(5);
    pipe.extract().unwrap();
    // The estimate is halved.
    let est_batch = pipe.estimate();
    assert_eq!(est_batch["batch"], true);
    let mut s_live = s.clone();
    s_live.batch_api = false;
    let est_live = yakusu_core::pipeline::Pipeline::new(env.clone(), pipe.p.clone(), s_live, fake().0).estimate();
    let half = est_live["total"][0].as_f64().unwrap() / 2.0;
    assert!((est_batch["total"][0].as_f64().unwrap() - half).abs() < 1e-3, "{est_batch} vs {est_live}");

    pipe.draft_style().unwrap();
    pipe.translate(None, false).unwrap();
    pipe.review(None).unwrap();
    let live = pipe.live_lines();
    pipe.p.with_results(|res| {
        assert!(live.iter().all(|l| res.get(&l.id).is_some_and(|r| r.good())));
    });
    assert!(pipe.p.with_results(|r| r.values().any(|r| r.stage == "review")));
    // Every translate and review answer came from a batch: nothing went live.
    assert_eq!(live_calls(&calls), Vec::<String>::new());
    let submits = calls.lock().unwrap().iter().filter(|(s, _)| s == "[submit]").count();
    assert_eq!(submits, 2, "one job for translation, one for review");
    // Batch answers are billed at half the model's price (all but the one
    // small live check made before the batch was sent).
    let costs = std::fs::read_to_string(pipe.p.path("costs.jsonl")).unwrap();
    let price = llm::price_of("claude-haiku-5-5").unwrap();
    let (mut half, mut full) = (0, 0);
    for line in costs.lines() {
        let v: serde_json::Value = serde_json::from_str(line).unwrap();
        if v["stage"] == "translate" {
            let u: llm::Usage = serde_json::from_value(v["usage"].clone()).unwrap();
            let usd = v["usd"].as_f64().unwrap();
            if (usd - llm::cost_at(&price, &u) / 2.0).abs() < 1e-12 {
                half += 1;
            } else {
                assert!((usd - llm::cost_at(&price, &u)).abs() < 1e-12);
                full += 1;
            }
        }
    }
    assert!(half >= 30, "{half} half-price entries");
    assert_eq!(full, 1);
    // Nothing is left waiting or saved once the stages are done.
    assert_eq!(pipe.batch_waiting(), 0);
    assert!(!pipe.p.path("batch-answers.jsonl").exists());
    assert_eq!(pipe.build().unwrap().applied, live.len());
}

#[test]
fn stop_while_waiting_then_collect_without_paying_twice() {
    let tmp = tempfile::tempdir().unwrap();
    let env = env(tmp.path());
    let game = make_game(&tmp.path().join("Slow Game"), SECOND_KS);
    let mut s = settings("custom", 2, 2);
    s.batch_api = true;
    s.review = false;
    s.translate_model.insert("anthropic".into(), "claude-slow".into());
    let (factory, calls) = fake();
    let mut pipe = pipeline(&env, &game, s.clone(), factory);
    pipe.batch_poll = Duration::from_millis(5);
    pipe.extract().unwrap();
    let pipe = std::sync::Arc::new(pipe);
    let runner = {
        let pipe = pipe.clone();
        std::thread::spawn(move || pipe.translate(None, false))
    };
    // Wait until the job is sent and saved, then press Stop.
    let t0 = std::time::Instant::now();
    while pipe.batch_waiting() == 0 {
        assert!(t0.elapsed() < Duration::from_secs(10), "the batch was never sent");
        std::thread::sleep(Duration::from_millis(5));
    }
    pipe.cancel.store(true, Ordering::SeqCst);
    assert!(matches!(runner.join().unwrap(), Err(Error::Cancelled)));
    let waiting = pipe.batch_waiting();
    assert!(waiting >= 3);
    assert_eq!(pipe.p.results_len(), 0);
    assert_eq!(live_calls(&calls), Vec::<String>::new());
    let state: serde_json::Value = serde_json::from_slice(&std::fs::read(pipe.p.path("batch.json")).unwrap()).unwrap();
    let id = state["jobs"][0]["id"].as_str().unwrap().to_string();

    // The provider finishes the job while Yakusu is closed.
    FAKE_JOBS.lock().unwrap().get_mut(&id).unwrap().1 = 0;

    // A new run (as after reopening the app) collects it instead of sending
    // again, even with the Batch API since turned off: it's paid for.
    let (factory2, calls2) = fake();
    s.batch_api = false;
    let mut pipe2 = pipeline(&env, &game, s, factory2);
    pipe2.batch_poll = Duration::from_millis(5);
    pipe2.translate(None, false).unwrap();
    assert!(!calls2.lock().unwrap().iter().any(|(s, _)| s == "[submit]"), "the batch was sent again");
    assert_eq!(live_calls(&calls2), Vec::<String>::new());
    let live = pipe2.live_lines();
    pipe2.p.with_results(|res| assert!(live.iter().all(|l| res.get(&l.id).is_some_and(|r| r.good()))));
    assert_eq!(pipe2.batch_waiting(), 0);
}

#[test]
fn small_jobs_and_tests_stay_live() {
    let tmp = tempfile::tempdir().unwrap();
    let env = env(tmp.path());
    let game = make_game(&tmp.path().join("Small Game"), SECOND_KS);
    let (factory, calls) = fake();
    let mut s = settings("quick", 40, 2);
    s.batch_api = true;
    let pipe = pipeline(&env, &game, s, factory);
    pipe.extract().unwrap();
    // A test run of chosen lines never waits for a batch.
    let ids: Vec<String> = pipe.live_lines().iter().take(3).map(|l| l.id.clone()).collect();
    pipe.translate(Some(&ids), false).unwrap();
    // One request (every line fits in one) isn't worth a batch either.
    pipe.translate(None, false).unwrap();
    assert!(!calls.lock().unwrap().iter().any(|(s, _)| s == "[submit]"));
    assert!(pipe.p.with_results(|r| r.len()) >= pipe.live_lines().len());
}
