# Ni — Implementation Plan

## Context

Ni starts from two documents: `ni-design.md` (the game/architecture outline) and `wires-and-boundaries.md` (the 7-post gRPC blog trace Ni exists to serve). The goal is to implement the grid-battle arena — engine, bots, runner — so that each blog post has real, runnable material behind it: deadlines, cancellation, contract evolution, ambiguous retries, and gRPC over Unix sockets.

**Decisions settled:**
- **Language:** Rust (tonic + prost + tokio). A second bot in Python comes later to make the contract argument real.
- **UI:** plain-text board rendered to stdout each turn.
- **Partial validity (design open Q1):** apply orders sequentially until the first illegal one, then stop. This preserves the partial-application ambiguity post 4 needs.
- **Timeout (design open Q2):** bot forfeits the *turn*, not the match; a configurable consecutive-strike limit (default 3) forfeits the match to guard against dead bots.

**Decisions resolved from the design doc's own leanings:**
- **Retryability (Q3):** `GetOrders` echoes the turn number in request and response, making it idempotent-by-construction. Engine policy: `DEADLINE_EXCEEDED` → forfeit turn, no retry (the ambiguity is the material); transient `UNAVAILABLE` → bounded retry; `FAILED_PRECONDITION`/`SHRUBBERY_REQUIRED` → re-send `NewMatch`, replay, retry.
- **Bot state (Q4):** spawn-per-match first; the view is self-contained so the reference bot is stateless per call.
- **View scope (Q5):** `BattlefieldView` is designed as a per-chapter *projection* from day one, even while it happens to be complete — fog of war later becomes a filtering change, not a breaking change.
- **Shrubbery placement (Q6):** fixed symmetric layouts shipped in `NewMatch` config, for reproducible/comparable matches.

---

## Setup

**Cargo workspace**, single repo:

```
Cargo.toml                  # [workspace]
proto/ni/v1/ni.proto        # the contract, package ni.v1
buf.yaml                    # buf for lint + breaking-change detection (post 2 material)
crates/
  ni-proto/                 # build.rs + tonic-build; generated types + client/server stubs
  ni-game/                  # pure rules engine — no async, no gRPC, no I/O
  ni-engine/                # binary `ni-engine`: engine + thin runner CLI + text renderer
  ni-bot/                   # binary `ni-bot`: reference bot + small bot-server scaffolding lib
.github/workflows/ci.yml    # buf lint, buf breaking --against main, fmt, clippy, cargo test
```

Key crates: `tonic`, `prost`, `tokio`, `clap` (CLI), `serde`/`serde_json` (match log), `thiserror`/`anyhow`, `tracing` + `tracing-subscriber` (OTel wiring added in the observability milestone). `protoc-bin-vendored` in the build script so no system `protoc` is needed.

Why buf even in a Rust project: it's language-agnostic, and `buf breaking` in CI is literally post 2 ("contracts with teeth") running on every PR.

Naming conventions from the design doc hold: binaries `ni-engine` / `ni-bot`, proto package `ni.v1`, sockets `/run/ni/*.sock` (dev fallback: `$XDG_RUNTIME_DIR/ni/` or a `--socket-dir` flag, since `/run` needs root).

---

## Protocol (`proto/ni/v1/ni.proto`)

Service `Bot` — the **engine is the gRPC client**, bots are servers (plugin direction, per design doc). Five unary RPCs:

| RPC | Notes |
|---|---|
| `Identify` | name, version, `protocol_version`; engine refuses too-old bots with `FAILED_PRECONDITION` |
| `NewMatch` | match ID, chapter assignment, board layout (dims + shrubbery tiles), rules config (HP, ranges, damage, turn cap, per-turn deadline) |
| `GetOrders` | `{match_id, turn, BattlefieldView}` in → `{turn, repeated KnightOrder}` out. Turn echoed both ways. |
| `MatchEnded` | result + reason enum (elimination, turn-cap HP decision, forfeit-timeout, forfeit-crash, forfeit-illegal…) |
| `SubmitReplay` | full match log; exists to give post 6 a large payload |

- `KnightOrder { unit_id, optional move_to, optional attack_target }` — the list-of-orders property.
- `BattlefieldView` — board dims, shrubbery tiles, visible knights (id, chapter, pos, hp, stats), whose turn, turn number, remaining time budget.
- `SHRUBBERY_REQUIRED`: `Status::failed_precondition` with message `SHRUBBERY_REQUIRED` (bot side raises it when it gets `GetOrders`/`MatchEnded` for an unknown match).
- **No streaming in v1** — deliberate, per design doc.

---

## Game core (`ni-game`)

Pure, deterministic, synchronous — fully unit-testable without any gRPC:

- Types: `Board` (10×10 + shrubbery set), `Knight { id, chapter, pos, hp, move_range, attack_range, damage }`, `MatchState`, `Order`, `OrderOutcome`.
- Movement: Manhattan distance ≤ move_range, destination in-bounds and unoccupied.
- Attack: target within attack_range **and** line of sight (Bresenham over tiles; any shrubbery tile on the line blocks). Target standing in shrubbery takes reduced damage (cover). Attacker inside shrubbery attacks out normally.
- `apply_orders(state, chapter, orders) -> (new_state, Vec<OrderOutcome>)` — validates each order *against the state as mutated so far*, applies until the first illegal order, stops there, records the reason. No randomness anywhere.
- Victory: enemy chapter eliminated; draw at turn cap (default 100) decided on surviving total HP.
- All numbers come from a `Rules` struct so `NewMatch` config is the single source.

Tests: table-driven unit tests for LOS/cover/movement edges, plus golden full-match tests (two scripted order sequences → assert exact final state), guaranteeing determinism.

---

## Engine + game loop (`ni-engine`)

Runner is thin and lives in the same binary: `ni-engine run --bot-a ./ni-bot --bot-b ./ni-bot [--transport tcp|unix] [--delay 200ms] [--quiet]`.

Loop, spawn-per-match model:

1. Spawn both bot processes, passing each a listen target on argv (`--listen unix:///…/a.sock` or `--listen tcp://127.0.0.1:0`; for TCP the bot prints its bound port on stdout).
2. Connect with dial-retry/backoff until each bot answers; `Identify` both, check `protocol_version`.
3. `NewMatch` to both (chapter assignment, layout, rules).
4. Per turn: build the acting chapter's `BattlefieldView` (projection function — the fog-of-war seam), call `GetOrders` with the per-turn deadline set via `Request::set_timeout` (which puts `grpc-timeout` on the wire — that this is a *propagated deadline*, not just a client timer, is post 3's point). Then:
   - Orders returned → check echoed turn number matches, `apply_orders`, log per-order outcomes.
   - Illegal orders → applied-until-first-illegal is a defined outcome, never a crash.
   - `DEADLINE_EXCEEDED` (or client-side timeout status — *observe and document what tonic actually returns*, that's post 4 material) → turn forfeited, strike counter++; 3 consecutive strikes → match forfeit.
   - `FAILED_PRECONDITION` / `SHRUBBERY_REQUIRED` → re-send `NewMatch` with current state replay, retry the same `GetOrders` (safe: turn echo).
   - Transport failure (`UNAVAILABLE`, connection refused, process exit) → match forfeit recorded, engine exits cleanly.
5. Render the board after each turn; append a JSONL match-log entry (turn, orders, outcomes, per-call latency).
6. On end: `MatchEnded` to both (best-effort), accept `SubmitReplay`, print result, kill/reap bot processes.

**The engine is the sole authority**: bots propose, engine disposes. Nothing a bot returns is trusted — unit IDs it doesn't own, out-of-range moves, garbage, all map to defined outcomes.

Transports: loopback TCP first; UDS second via tonic's `connect_with_connector`(client, `tokio::net::UnixStream`) and `serve_with_incoming`(server, `UnixListenerStream`). Same service definition, swap is a flag — which is itself post 5's point.

---

## Reference bot (`ni-bot`)

Small tonic server + trivial strategy (move toward nearest visible enemy, attack if legal — enough for interesting matches). Holds a `HashMap<MatchId, MatchInfo>`; unknown match ⇒ `SHRUBBERY_REQUIRED`. The server scaffolding (arg parsing, listener setup for both transports, service wiring) is a small lib within the crate so `roger-the-shrubber` reuses it later.

---

## UI

Text renderer in `ni-engine`, stdout, one frame per turn:

```
turn 12  ··· chapter A to act ··· deadline 500ms
  0 1 2 3 4 5 6 7 8 9
0 · · · # # · · · · ·      A1 hp 7   B1 hp 10
1 · A1· # # · · b2· ·      A2 hp 10  B2 hp 4
...
```
Knights as `A1..A4` / `b1..b4`, shrubbery as `#`, HP sidebar, order/outcome lines beneath (`A2 moves (3,4)→(5,4), attacks B1: hit 3 (cover)`). `--delay` to watch live, `--quiet` for tournaments/benchmarks. Replay stepping through the JSONL log is a later nice-to-have.

---

## Milestones (mapped to blog posts)

1. **M0 — Scaffolding:** workspace, proto v1, buf lint/breaking, generated stubs compiling, CI.
2. **M1 — Rules engine:** `ni-game` complete with unit + golden tests.
3. **M2 — Core loop** *(post 1)*: spawn-per-match engine vs two reference bots over loopback TCP, text rendering, match result printed.
4. **M3 — Time control & failure** *(posts 3, 4)*: per-turn deadlines, turn-forfeit + strikes, `SHRUBBERY_REQUIRED` recovery, turn-number echo, crash/garbage handling; start `roger-the-shrubber` (sleeps, crashes, nonsense orders, others' knights).
5. **M4 — Observability** *(post 3)*: `tracing` span per `GetOrders`, W3C traceparent in gRPC metadata engine→bot, JSONL match log with timings, `SubmitReplay` wired.
6. **M5 — UDS + measurement harness** *(posts 5, 6)*: unix transport behind the flag; bench binary measuring empty-call / 1KB / 1MB over UDS vs loopback TCP (blog note: build this early — post 1's numbers come free).
7. **M6 — Later, gated on the work:** daemon mode + `/run/ni` glob discovery + gRPC health checking, tournament round-robin, Python bot *(post 2 evolution experiments: add a unit type / cover value, run old bots against new engine)*.

Contract-evolution experiments (post 2) don't need code up front — they're `buf breaking` plus deliberate proto edits on a branch once M2 works.

---

## Verification

- `cargo fmt --check && cargo clippy && cargo test` across the workspace (CI).
- `ni-game`: golden deterministic match tests — same inputs, byte-identical final state.
- Integration test (in `ni-engine/tests/`): build and spawn real `ni-bot` binaries, run a full match over TCP and over UDS, assert a decisive, deterministic result and a well-formed match log.
- Failure-mode integration tests using `roger-the-shrubber` flags (`--sleep-forever`, `--crash-on-turn N`, `--illegal-orders`): assert the engine records the right forfeit reason and never panics.
- Manual smoke: `cargo run -p ni-engine -- run --bot-a target/debug/ni-bot --bot-b target/debug/ni-bot --delay 200ms` and watch the board.
