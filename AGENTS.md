# AGENTS.md

SuperMiniGames: a server-authoritative 2-player match service in Rust.

Read only what you need. This file holds agent rules only; product rules live in the spec.

## Documents
- `spec/SPEC.md`: requirements and rules. **The spec wins** over every other file.
- `spec/PLAN.md`: architecture, schema, API shapes, stack, folder layout, milestones.
- `spec/TASK.md`: master task list, task owners, parallel groups, human gates.
- `spec/TRACEABILITY.md`: generated. Never edit by hand.

## Workflow
1. Plan from `spec/TASK.md`. Work one task at a time (or one parallel group) and stop at its "Done when". Read only the spec sections that task lists.
2. If the spec is wrong, unclear or conflicts with the plan: **stop and report it**. Do not work around it and do not edit the spec unless asked.
3. Cite requirement IDs in test names and a `// REQ: <ID>` comment (for example `t_scr_6_draw_splits_points`).
4. Finish with a short summary: what changed, which IDs it covers, and whether Verus, fuzzing and benchmarks ran or were skipped, and why (VER-3).
5. Ask before adding a dependency that is not in the crate table (spec 1.1).

## Subagents (pi-subagents)
Delegation is authorized for the tasks in `spec/TASK.md`. The parent session is the orchestrator.
- Loop per task: `scout` (only if the area is unfamiliar) -> `worker` -> a fresh `reviewer` -> `worker` fixes. Use `oracle` before risky decisions (spec conflicts, memory or TLS trade-offs). Use `researcher` then `evidence-auditor` for external facts such as game rules (T5.0).
- Give each subagent one task card (format in `spec/TASK.md`). It edits only its `Owns` paths and reports back. It never edits `spec/TASK.md`, the spec or the plan.
- Run tasks in parallel only if they share a parallel label in `spec/TASK.md`. Otherwise run them in order, or use worktree isolation.
- Subagents follow every rule in this file, including Commands below.
- Reviewers are read-only and fresh (not the worker). They check the cited spec IDs, tests and simplicity.
- Keep runs small. Stay well under pi-subagents' default limit of 64 children per run.
- Only the orchestrator edits `spec/TASK.md` and runs the Verus, fuzz and benchmark triggers.

## Commands
- Do **not** run `cargo check`, `cargo clippy --fix` or `cargo test`. Editor hooks run them; read their output and fix what they report (VER-2).
- Verus: not set up until task T8.2; before that `logic` is covered by `proptest`. After T8.2, run `cargo verus verify -p superminigames-logic` only when you change `logic`, invariants, state-machine rules, timer or disqualification decisions, or scoring (VER-12).
- `cargo fuzz run <target> -- -max_total_time=60` only for an untrusted-input path, and only a target from the list in VER-21.
- Criterion benchmarks only after the project is feature-complete (VER-30).
- After any change to `spec/SPEC.md`: `python3 spec/traceability.py` (CI runs it with `--check`).

## Proof failures (VER-17)
Fix the code if it is wrong. If the formal spec is wrong, propose a change and wait for a human. Never weaken or delete `requires`/`ensures`, and never add `assume`, `admit`, `external_body` or `external` without an `APPROVED-BY:` marker.

## Code rules (CI enforces most of these)
- Respect crate boundaries (spec 1.1). Game crates never depend on each other, `engine`, `server`, `protocol` or `tokio` (WSP-9).
- `#![forbid(unsafe_code)]` in every crate.
- In `engine`, `core`, `logic` and game crates: no `HashMap`/`HashSet`, `thread_rng`, `OsRng`, `Uuid::new_v4`, `SystemTime::now` or threads. All randomness comes from the `core` RNG helpers (CI-2).
- No floating point in scoring or ranking; integer scale 12 (CI-3).
- No `unwrap()` or `expect()` outside tests; typed errors with `thiserror` (FLR-8).
- No `Mutex` or `RwLock` in `engine` (CI-6). Keep `panic = "unwind"` in the release profile.
- `logic` stays plain: primitive types, synchronous, no `serde`, no `tokio` (VER-11).
- No secrets in the repository or logs.

## Stop and ask first
- A MUST that cannot be met, or two requirements that disagree.
- Any `HUMAN` task in `spec/TASK.md` (hooks, golden files, game rules, TLS and allocator choice, UI review, capacity targets).
- Any game rule you are unsure of. Do **not** implement xiangqi, shogi or doushouqi until task T5.1 (human rule review) is done.
- A new dependency, a new crate, or a change to the protocol.
