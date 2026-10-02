//! P4.Z — the Phase-5 usability smoke test (p4-plan §P4.Z): one launch
//! driven end to end on public surfaces only — `admit → decided → begin →
//! agent.start (fake Herdr) → prompt (fake Herdr) → finish` — every
//! `Transition` committed through `store::apply`, the Jev evaluation
//! answered by a private in-file `TcpListener` fake, the catalog loaded by
//! the config adapter, the git and transcript adapters read on tempdirs.
//! No daemon, no `pub(crate)` reach-in. Daemon-only values minted inline
//! (handoff names their Phase-5 home): caller binding, run/set ids, the
//! dispatch commit, `HerdrIncarnation` (OQ-8), `begin ‖ launch_plan`.

#[cfg(test)]
pub mod support;

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt as _;
    use std::path::{Path, PathBuf};
    use std::time::Duration;

    use governor_core::config::{ConfigVersion, Policy};
    use governor_core::identity::{
        AgentKind, AgentName, CallerBinding, CallerKey, ChildIdentity, EffectKey, HerdrIncarnation,
        IdempotencyKey, JudgmentSetId, LaunchId, NativeSession, PaneId, ProjectRoot,
        RelayInstanceId, RunId, TabId, TerminalId, Timestamp,
    };
    use governor_core::lifecycle::{
        EffectKind, EffectOutcome, EffectReceipt, EffectResult, EffectState, EffectWrite, Event,
        PromptCertainty, Run, State, StateChange, Transition, VersionTriple, Versioned,
        launch_plan, reserved_run, transition,
    };
    use governor_core::routing::Question::{ChangesFiles, WeakestSufficientTier};
    use governor_core::routing::{
        Decision, JudgmentOutcome, JudgmentPurpose, JudgmentRecord, JudgmentSet, QuestionVersion,
        evaluation_questions, evaluation_verdict, placement_plan, route, validate_evaluation,
    };
    use governor_core::task::{
        Launch, LaunchOutcome, LaunchPhase, Task, admit, begin, decided, finish, new_launch,
    };
    use herdr_governor::adapters::config::load;
    use herdr_governor::adapters::git::{base_commit, worktree_evidence};
    use herdr_governor::adapters::herdr::{AgentPromptParams, AgentStartParams, TabCreateParams};
    use herdr_governor::adapters::jev::{
        ApiKey, Client as JevClient, JudgeParams, Kind, QuestionSpec, State as JevState, TaskState,
    };
    use herdr_governor::adapters::transcript::{
        Cursor, EventKind, SessionPointer, TranscriptRoots, read_window, resolve,
    };
    use herdr_governor::store::Store;
    use serde_json::{Value, json};
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
    use tokio::net::TcpListener;

    use crate::support::fake_herdr::{FakeHerdr, Topology};

    const NOW: Timestamp = Timestamp(1_790_812_800_000);
    const D: Duration = Duration::from_secs(5);
    /// A Phase-5 `RunId` is a uuid v7 the daemon mints; fixed here.
    const RUN_ID: &str = "01932a00-0000-7000-8000-000000000001";
    const SET_ID: &str = "jset:launch:l-smoke:evaluate";
    const CATALOG: &str = r#"
[policy]
tiers = ["fast", "standard", "frontier"]
provider_limit_threshold = 0.6
cooldown_secs = 3600
[[catalog.operating_points]]
id = "atlas-mini"
harness = "atlas"
args = ["--fast"]
tier = "fast"
capabilities = ["start"]
cost_class = 0
provider = "vendor-b"
"#;
    const TRANSCRIPT: &str = "{\"type\":\"message\",\"message\":{\"role\":\"assistant\",\
                              \"content\":[{\"type\":\"text\",\"text\":\"renamed\"}]}}\n";

    fn caller() -> CallerKey {
        CallerKey {
            agent_kind: AgentKind("caller-kind".into()),
            native_session: NativeSession("caller-sess".into()),
        }
    }

    fn task() -> Task {
        Task {
            objective: "Rename the helper and update its call sites".into(),
            scope: "src/tui.rs only".into(),
            done_when: vec!["just test passes".into()],
            constraints: vec!["No new dependencies".into()],
            tier: None,
            recovery_of: None,
            label: None,
            cwd: None,
        }
    }

    fn only(changes: Vec<StateChange>) -> Transition {
        Transition {
            state_changes: changes,
            events: vec![],
            effects: vec![],
        }
    }

    fn write(key: &EffectKey, state: EffectState, receipt: Option<EffectReceipt>) -> Transition {
        only(vec![StateChange::WriteEffect(EffectWrite {
            key: key.clone(),
            state,
            certainty: None,
            receipt,
        })])
    }

    /// The dispatch commit (`planned → dispatching`) — the Phase-5 dispatcher's write.
    fn dispatching(key: &EffectKey) -> Transition {
        write(key, EffectState::Dispatching, None)
    }

    fn concat(mut a: Transition, b: Transition) -> Transition {
        a.state_changes.extend(b.state_changes);
        a.events.extend(b.events);
        a.effects.extend(b.effects);
        a
    }

    fn ack(key: &EffectKey, kind: EffectKind, receipt: Option<EffectReceipt>) -> Event {
        Event::EffectResult(EffectResult {
            key: key.clone(),
            kind,
            outcome: EffectOutcome::Acknowledged,
            receipt,
        })
    }

    fn run_key(suffix: &str) -> EffectKey {
        EffectKey(format!("run:{RUN_ID}:{suffix}"))
    }

    /// Re-read the Run, apply the core's total transition function to
    /// `event`, commit the result — Phase 5's coordinator loop.
    fn step(store: &mut Store, decision: &Decision, policy: &Policy, event: Event) -> Run {
        let id = RunId(RUN_ID.into());
        let run = store.run(&id).unwrap().unwrap();
        let journal = store.journal(&id).unwrap();
        let handoffs = store.handoffs(&id).unwrap();
        let stamped = Versioned {
            requested_against: VersionTriple {
                version: run.version,
                work_generation: run.work_generation,
                evidence_generation: run.evidence_generation,
            },
            value: event,
        };
        let read = (Some(decision), journal.as_slice(), handoffs.as_slice());
        let next = transition(&run, &stamped, NOW, policy, read, "/unused/freeze");
        assert!(!next.state_changes.is_empty(), "the event must apply");
        store.apply(&next, NOW).unwrap();
        store.run(&id).unwrap().unwrap()
    }

    fn effect_state(store: &Store, key: &EffectKey) -> EffectState {
        store.effect(key).unwrap().unwrap().state
    }

    /// Catalog, credential (0600), a git repo with one commit and a pi
    /// transcript, all under one tempdir.
    fn fixtures(dir: &Path) -> (PathBuf, PathBuf, PathBuf, PathBuf) {
        let catalog = dir.join("catalog.toml");
        std::fs::write(&catalog, CATALOG).unwrap();
        let cred = dir.join("credentials");
        std::fs::write(&cred, "smoke-token\n").unwrap();
        std::fs::set_permissions(&cred, std::fs::Permissions::from_mode(0o600)).unwrap();
        let repo = dir.join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        git(&repo, &["init", "-q", "-b", "main"]);
        std::fs::write(repo.join("README"), "smoke\n").unwrap();
        git(&repo, &["add", "-A"]);
        git(&repo, &["commit", "-q", "-m", "init"]);
        let transcript = dir.join("session.jsonl");
        std::fs::write(&transcript, TRANSCRIPT).unwrap();
        (catalog, cred, repo, transcript)
    }

    fn git(root: &Path, args: &[&str]) {
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(root)
            .args(args)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@t")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@t")
            .output()
            .unwrap();
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(out.status.success(), "git {args:?}: {stderr}");
    }

    /// The private fake Jev: accept one connection, read one HTTP/1.1
    /// request, answer exactly the six launch questions, close. Returns
    /// the decoded request body.
    async fn serve_jev_once(listener: &TcpListener) -> Value {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut raw = Vec::new();
        while !raw.ends_with(b"\r\n\r\n") {
            let mut byte = [0_u8; 1];
            assert_eq!(stream.read(&mut byte).await.unwrap(), 1, "headers end");
            raw.push(byte[0]);
        }
        let head = String::from_utf8_lossy(&raw).to_ascii_lowercase();
        let length: usize = head
            .lines()
            .find_map(|l| l.strip_prefix("content-length:"))
            .map(|v| v.trim().parse().unwrap())
            .unwrap();
        let mut body = vec![0_u8; length];
        stream.read_exact(&mut body).await.unwrap();
        let reply = json!({
            "model": "jev-smoke",
            "answers": {
                "done_when_verifiable": {"type": "noul", "noul": 0.9},
                "weakest_sufficient_tier": {"type": "choice", "choice": "fast",
                    "probabilities": {"fast": 0.7, "standard": 0.2, "frontier": 0.1}},
                "changes_files": {"type": "choice", "choice": "few",
                    "probabilities": {"none": 0.1, "few": 0.8, "broad": 0.1}},
                "security_boundary": {"type": "noul", "noul": 0.1},
                "needs_external": {"type": "noul", "noul": 0.1},
                "long_running": {"type": "noul", "noul": 0.1}
            },
            "usage": {"input_tokens": 1, "output_tokens": 1}
        })
        .to_string();
        let response = format!(
            "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\
             connection: close\r\n\r\n{reply}",
            reply.len()
        );
        stream.write_all(response.as_bytes()).await.unwrap();
        stream.shutdown().await.unwrap();
        serde_json::from_slice(&body).unwrap()
    }

    fn question_specs() -> Vec<QuestionSpec> {
        evaluation_questions(&[])
            .into_iter()
            .map(|question| QuestionSpec {
                question,
                kind: if matches!(question, WeakestSufficientTier | ChangesFiles) {
                    Kind::Choice
                } else {
                    Kind::Noul { threshold: None }
                },
                instructions: format!("answer {}", question.as_str()),
                criteria: vec![("yes".into(), "it holds".into())],
            })
            .collect()
    }

    /// `admit`: the caller binding, then the `evaluating` row plus the
    /// planned `jev_evaluate`; the evaluation runs against the fake Jev and
    /// its `Judgments` receipt commits as the result write.
    async fn admit_and_evaluate(
        store: &mut Store,
        launch: &Launch,
        policy_version: &ConfigVersion,
        cred: &Path,
    ) -> JudgmentRecord {
        let bind = StateChange::BindCaller(CallerBinding {
            caller: caller(),
            relay_instance: RelayInstanceId("relay-smoke".into()),
            pane_at_bind: PaneId("w0:p0".into()),
        });
        store.apply(&only(vec![bind]), NOW).unwrap();
        let admitted = admit(launch);
        assert_eq!(admitted.effects.len(), 1, "admit plans the evaluation");
        store.apply(&admitted, NOW).unwrap();
        let eval_key = EffectKey(format!("launch:{}:evaluate", launch.id.0));
        store.apply(&dispatching(&eval_key), NOW).unwrap();

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let key = ApiKey::read_0600(cred).await.unwrap();
        let client = JevClient::new(&base).unwrap();
        let params = JudgeParams {
            model: "jev-latest".into(),
            state: JevState::Task(TaskState::from(&launch.task)),
            questions: question_specs(),
            timeout: D,
        };
        let (answer, request) =
            tokio::join!(client.judge(&key, &params), serve_jev_once(&listener));
        let judged = answer.unwrap();
        assert_eq!(request["state"]["task"]["objective"], launch.task.objective);
        assert_eq!(judged.judgments.len(), 6, "six launch questions");

        let record = JudgmentRecord {
            set: JudgmentSet {
                id: JudgmentSetId(SET_ID.into()),
                purpose: JudgmentPurpose::Launch,
                launch: Some(launch.id.clone()),
                run: None,
                versions: None,
                task_digest: launch.task_digest,
                handoff_digest: None,
                evidence_digest: None,
                model: judged.model,
                question_version: QuestionVersion("q-v1".into()),
                policy_version: policy_version.clone(),
                outcome: JudgmentOutcome::Answered,
            },
            judgments: judged.judgments,
        };
        let receipt = Some(EffectReceipt::Judgments(record.clone()));
        let commit = write(&eval_key, EffectState::Acknowledged, receipt);
        store.apply(&commit, NOW).unwrap();
        assert_eq!(effect_state(store, &eval_key), EffectState::Acknowledged);
        store
            .judgment_record(&JudgmentSetId(SET_ID.into()))
            .unwrap()
            .expect("the receipt wrote the judgment rows")
    }

    /// `decided` then `begin ‖ launch_plan`: the routed row, the reserved
    /// Run, then `launching` with the one `tab_create` effect journaled.
    fn decide_and_begin(
        store: &mut Store,
        launch: &Launch,
        decision: &Decision,
        run: &Run,
    ) -> Launch {
        store.apply(&decided(launch, decision, run), NOW).unwrap();
        let routed = store.launch(&launch.id).unwrap().unwrap();
        assert_eq!(routed.phase, LaunchPhase::Routed);
        assert_eq!(routed.decision.as_ref(), Some(decision));
        let reserved = store.run(&run.id).unwrap().unwrap();
        assert_eq!(reserved.state, State::Reserved);
        let plan = placement_plan(None, &[]);
        let planned = launch_plan(&reserved, decision, &plan, &PaneId("w0:p0".into()));
        let begun = concat(begin(&routed), planned);
        assert_eq!(begun.effects.len(), 1, "one topology effect");
        store.apply(&begun, NOW).unwrap();
        let launching = store.launch(&launch.id).unwrap().unwrap();
        assert_eq!(launching.phase, LaunchPhase::Launching);
        assert_eq!(store.run(&run.id).unwrap().unwrap().state, State::Starting);
        launching
    }

    /// The Herdr leg: `tab.create`, `agent.start`, `agent.prompt` against
    /// the fake, each dispatch committed before the wire and each result
    /// fed back through the transition function.
    async fn launch_on_herdr(
        store: &mut Store,
        fake: &FakeHerdr,
        decision: &Decision,
        policy: &Policy,
        prompt: &str,
    ) -> Run {
        let client = fake.client();
        let candidate = &decision.candidates[0];
        let tab_key = run_key("tab");
        store.apply(&dispatching(&tab_key), NOW).unwrap();
        let tab = TabCreateParams::default();
        let created = client.tab_create(&tab, D).await.unwrap().value;
        let tab_receipt = EffectReceipt::TabCreated {
            tab: TabId(created.tab.tab_id.clone()),
            pane: PaneId(created.root_pane.pane_id.clone()),
        };
        let tab_event = ack(&tab_key, EffectKind::TabCreate, Some(tab_receipt));
        let starting = step(store, decision, policy, tab_event);
        assert_eq!(starting.state, State::Starting, "start planned");

        let start_key = run_key("start:0");
        assert_eq!(effect_state(store, &start_key), EffectState::Planned);
        store.apply(&dispatching(&start_key), NOW).unwrap();
        let start = AgentStartParams {
            name: starting.child_name.clone(),
            kind: candidate.harness.0.clone(),
            pane_id: created.root_pane.pane_id.clone(),
            args: candidate.args.clone(),
            timeout_ms: Some(30_000),
        };
        let started = client.agent_start(&start, D).await.unwrap().value;
        assert_eq!(started.argv.first(), Some(&candidate.harness.0));
        let identity = ChildIdentity {
            // OQ-8: the daemon mints `HerdrIncarnation` from `ConnEpoch`.
            herdr_incarnation: HerdrIncarnation(format!("fake:{}", fake.incarnation())),
            terminal_id: TerminalId(started.agent.terminal_id.clone()),
            agent_kind: candidate.harness.clone(),
            agent_name: AgentName(starting.child_name.clone()),
            native_session: started.agent.agent_session.map(|s| NativeSession(s.value)),
            pane_id: PaneId(started.agent.pane_id.clone()),
        };
        let receipt = EffectReceipt::AgentStarted {
            identity: identity.clone(),
        };
        let start_event = ack(&start_key, EffectKind::AgentStart, Some(receipt));
        let prompting = step(store, decision, policy, start_event);
        assert_eq!(prompting.state, State::Prompting);
        assert_eq!(prompting.identity.as_ref(), Some(&identity));
        let point = prompting.operating_point.as_ref();
        assert_eq!(point, Some(&candidate.operating_point));

        let prompt_key = run_key("prompt:task");
        assert_eq!(effect_state(store, &prompt_key), EffectState::Planned);
        store.apply(&dispatching(&prompt_key), NOW).unwrap();
        let params = AgentPromptParams {
            target: prompting.child_name.clone(),
            text: prompt.to_owned(),
        };
        let prompted = client.agent_prompt(&params, D).await.unwrap().value;
        assert_eq!(prompted.agent.pane_id, started.agent.pane_id);
        let prompt_event = ack(&prompt_key, EffectKind::Prompt, None);
        let active = step(store, decision, policy, prompt_event);
        assert_eq!(active.state, State::Active);
        assert_eq!(active.prompt_certainty, Some(PromptCertainty::Acknowledged));
        active
    }

    #[tokio::test]
    async fn smoke_admit_to_finish_on_public_surfaces() {
        let dir = tempfile::tempdir().unwrap();
        let (catalog, cred, repo, transcript) = fixtures(dir.path());
        let loaded = load(&catalog).await.unwrap();
        let policy = &loaded.config.policy;
        let mut store = Store::open(&dir.path().join("store.db")).unwrap();
        let fake = FakeHerdr::start(Topology::single_shell());
        let repo_str = repo.to_string_lossy().into_owned();

        let launch = new_launch(
            LaunchId("l-smoke".into()),
            caller(),
            ProjectRoot(repo_str.clone()),
            IdempotencyKey("key-smoke".into()),
            task(),
        );
        let record = admit_and_evaluate(&mut store, &launch, &loaded.version, &cred).await;

        // Route on the committed judgment rows, exactly as Phase 5 will.
        let evaluation = validate_evaluation(&record, policy, &[]).unwrap();
        assert_eq!(evaluation_verdict(&evaluation), None, "not rejected");
        let decision = route(&launch, None, &evaluation, &loaded.config, &[], &[], &[]).unwrap();
        assert_eq!(decision.candidates.len(), 1, "the one catalog point");
        let head = base_commit(&repo).await.unwrap();
        let run_id = RunId(RUN_ID.into());
        let run = reserved_run(&launch, run_id, repo_str, Some(head), NOW, policy);
        let launching = decide_and_begin(&mut store, &launch, &decision, &run);

        let prompt = launch.task.render();
        let active = launch_on_herdr(&mut store, &fake, &decision, policy, &prompt).await;
        let methods: Vec<String> = fake.requests().into_iter().map(|(m, _)| m).collect();
        assert_eq!(methods, ["tab.create", "agent.start", "agent.prompt"]);

        // Evidence adapters on the same tempdirs: the Run's baseline is the
        // repo's HEAD, and the child's transcript reads through the pointer.
        let evidence = worktree_evidence(&repo).await.unwrap();
        assert_eq!(Some(evidence.head), active.base_commit);
        assert!(evidence.dirty.is_empty(), "clean worktree");
        let pointer = SessionPointer::new("pi", &transcript.to_string_lossy(), None);
        let source = resolve(&pointer, &TranscriptRoots::default())
            .await
            .unwrap();
        let window = read_window(&source, Cursor::START).await.unwrap();
        assert_eq!(window.events.len(), 1, "one transcript record");
        assert_eq!(window.events[0].kind, EventKind::Message);
        assert_eq!(window.events[0].text.as_deref(), Some("renamed"));

        // `finish`: `launching` → `done`/`launched`, caller notified.
        let outcome = LaunchOutcome::Launched {
            run: active.id.clone(),
            operating_point: active.operating_point.clone().unwrap(),
            requested_operating_point: None,
            tier_evidence: decision.clone(),
        };
        let done = finish(&launching, outcome.clone(), None, None, NOW, policy);
        assert_eq!(done.events.len(), 1, "one launch_answered");
        store.apply(&done, NOW).unwrap();

        let final_launch = store
            .launch_by_idempotency(&caller(), &launch.project_root, &launch.idempotency_key)
            .unwrap()
            .unwrap();
        assert_eq!(final_launch.phase, LaunchPhase::Done);
        assert_eq!(final_launch.outcome, Some(outcome));
        let final_run = store.run_by_launch(&launch.id).unwrap().unwrap();
        assert_eq!(final_run, active, "finish leaves the active Run untouched");
        for key in ["tab", "start:0", "prompt:task"] {
            let state = effect_state(&store, &run_key(key));
            assert_eq!(state, EffectState::Acknowledged, "{key}");
        }
        assert!(store.ready_effects().unwrap().is_empty(), "nothing planned");
        let unacked = store.mailbox_unacked(&caller(), None, 10).unwrap();
        let kinds: Vec<_> = unacked.iter().map(|e| e.kind.as_str()).collect();
        assert_eq!(kinds, ["launch_answered"]);
        let body = &unacked[0].body;
        assert!(body.contains("\"launched\""), "{body}");
        assert_eq!(store.unsettled_runs().unwrap().len(), 1, "one active Run");
    }
}
