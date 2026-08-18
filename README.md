# ni

A grid-battle arena where bot processes fight over gRPC. They demand shrubbery.

Ni exists to make a blog series honest. The
[**Wires & Boundaries**](workbooks/wires-and-boundaries.md) trace argues that
if you strip the network out of a service boundary, you find out which costs
belonged to the network and which belonged to the boundary — and that the
expensive part was never the wire. Every claim in that series is supposed to
have something runnable behind it: real deadlines, real cancellation, real
ambiguous retries, real Unix sockets, real numbers.

So Ni is a game, and the game is a pretext. Two bot processes each expose the
same gRPC service; an engine calls them once per turn with a per-turn deadline
and resolves whatever they propose. Everything interesting is at the seam.

```
                 ┌───────────────────────────────────────┐
                 │  ni-engine   (gRPC client, authority) │
                 │  spawns bots, owns state, resolves    │
                 └───────┬───────────────────────┬───────┘
       GetOrders(view)   │                       │   GetOrders(view)
       + grpc-timeout    │                       │   + traceparent
                         ▼                       ▼
                 ┌───────────────┐       ┌───────────────────────┐
                 │ ni-bot        │       │ roger-the-shrubber    │
                 │ (gRPC server) │       │ (gRPC server, hostile)│
                 └───────────────┘       └───────────────────────┘
                    tcp://127.0.0.1:0  or  unix:///…/a.sock
```

- Documentation index: [`workbooks/`](workbooks) — design, plan, and a
  step-by-step workbook per milestone.
- Contract: [`proto/ni/v1/ni.proto`](proto/ni/v1/ni.proto).

## Contents

- [The contract](#the-contract)
- [The game](#the-game)
- [Repository layout](#repository-layout)
- [Building](#building)
- [Running things](#running-things)
  - [Play a match](#play-a-match)
  - [Choosing a transport](#choosing-a-transport)
  - [Breaking things on purpose](#breaking-things-on-purpose)
  - [The match log](#the-match-log)
  - [Traces and the dashboard](#traces-and-the-dashboard)
  - [Running a bot by hand](#running-a-bot-by-hand)
  - [Environment variables](#environment-variables)
- [Running the benchmark](#running-the-benchmark)
- [Testing](#testing)
- [Design decisions worth knowing](#design-decisions-worth-knowing)
- [Documentation](#documentation)
- [Status](#status)

## The contract

`ni.v1.BotService` — five unary RPCs, no streaming in v1, deliberately.

| RPC | Direction | What it is for |
|---|---|---|
| `Identify` | engine → bot | Handshake: name, version, `protocol_version`. The engine refuses a bot it cannot serve |
| `NewMatch` | engine → bot | Match id, seat, board layout, rules. Re-sent mid-match to restore a bot that lost its state |
| `GetOrders` | engine → bot | The hot path: a `BattlefieldView` in, a list of `KnightOrder`s out, with the turn number echoed both ways |
| `MatchEnded` | engine → bot | Result and reason. Best effort |
| `SubmitReplay` | engine → bot | The whole match log, mostly so there is a large payload to measure |

Two things about this shape are load-bearing:

**The engine is the gRPC *client*.** Bots are servers. That inversion makes a
bot a *plugin* rather than a participant: it is spawned, it is asked, it is shut
down. The engine is the sole authority on rules and state — bots propose orders,
the engine disposes, and nothing a bot returns is trusted.

**`SHRUBBERY_REQUIRED`.** A bot asked about a match it holds no state for
answers `FAILED_PRECONDITION` with the message `SHRUBBERY_REQUIRED`, meaning
*you have not brought me a shrubbery — send me a `NewMatch` first*. The engine
re-sends `NewMatch` with the current position and retries the same `GetOrders`.
That retry is safe only because the turn number is echoed, which is the whole
idempotency argument in one field.

## The game

A 10×10 board, four knights a side, shrubbery clustered in the middle and
mirrored so neither side gets a terrain advantage. Chapters alternate turns.

```
     0  1  2  3  4  5  6  7  8  9
 0   ·  ·  ·  ·  ·  ·  ·  ·  ·  ·
 1   · A1  ·  ·  ·  ·  ·  · B1  ·
 2   ·  ·  ·  #  #  #  #  ·  ·  ·
 3   · A2  ·  ·  ·  ·  ·  · B2  ·
 4   ·  ·  ·  ·  #  #  ·  ·  ·  ·
 5   ·  ·  ·  ·  #  #  ·  ·  ·  ·
 6   · A3  ·  ·  ·  ·  ·  · B3  ·
 7   ·  ·  ·  #  #  #  #  ·  ·  ·
 8   · A4  ·  ·  ·  ·  ·  · B4  ·
 9   ·  ·  ·  ·  ·  ·  ·  ·  ·  ·
```

| Rule | Standard value |
|---|---|
| Knight HP | 10 |
| Move range | 3 (Manhattan; no pathfinding — any in-bounds, unoccupied tile in range) |
| Attack range | 2, **and** line of sight |
| Attack damage | 4 |
| Cover | −2 damage when the target stands in shrubbery |
| Turn cap | 100 turns, then decided on surviving total HP |

Line of sight is Bresenham over tiles; any shrubbery tile on the line blocks it.
Orders are applied in list order, each validated against the state *as mutated
so far*, stopping at the first illegal one — so a bad order wastes a turn and is
never a crash. A match ends by elimination, at the turn cap, or by forfeit
(timeout strikes, an unreachable bot, or an unrecoverable protocol violation).

None of this is random. Same inputs, same match, byte for byte — which is what
makes golden tests and transport comparisons possible.

## Repository layout

```
proto/ni/v1/ni.proto        the contract — engine is the client, bots are servers
buf.yaml                    buf lint + breaking-change config
crates/
  ni-proto                  generated types and stubs (tonic-build, vendored protoc)
  ni-game                   pure, deterministic rules engine — no gRPC, no async, no I/O
  ni-engine                 match engine, runner CLI, text renderer, transports, match log
  ni-bot                    reference bot + the server scaffolding every bot reuses
  roger-the-shrubber        a deliberately hostile bot: sleeps, crashes, forgets, lies
  ni-telemetry              tracing subscriber, OTLP exporters, W3C context in/out
  ni-bench                  measurement harness: one contract, two transports, four sizes
deploy/telemetry            Grafana + Tempo + Loki + OTel collector, one container
workbooks/                  design docs and the step-by-step workbooks
```

The dependency rule that keeps the rest honest: **`ni-game` knows nothing about
gRPC, async, or I/O.** It is `Board`, `Knight`, `MatchState`, `apply_orders` and
nothing else. `ni-engine` translates between `ni.v1` wire types and game types
in one module (`convert.rs`), and that translation *is* the service boundary the
blog trace is about. `ni-game` has not changed since M1.

| Crate | Binary | Key idea |
|---|---|---|
| `ni-game` | — | Pure rules. Fully unit-testable with no runtime |
| `ni-engine` | `ni-engine` | Authority. Every failure decision lives in `policy.rs`; every transport decision in `transport.rs` |
| `ni-bot` | `ni-bot` | Reference strategy, plus `serve()` — the listener half both bots share |
| `roger-the-shrubber` | `roger-the-shrubber` | Every way a bot can misbehave, behind flags |
| `ni-telemetry` | — | Context, not game. No game type appears in it |
| `ni-bench` | `ni-bench` | Measurement only. Reuses the engine's real spawn/dial code |

## Building

Rust stable (developed against 1.94) and nothing else. **No system `protoc`
needed** — `crates/ni-proto/build.rs` uses `protoc-bin-vendored`.

```sh
cargo build --workspace              # debug
cargo build --workspace --release    # for anything you intend to measure
cargo test --workspace
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
```

Proto linting and breaking-change detection use [buf](https://buf.build):

```sh
buf lint
buf breaking --against '.git#branch=main'
```

CI ([`.github/workflows/ci.yml`](.github/workflows/ci.yml)) runs both jobs:
`buf lint` on every push plus `buf breaking` on pull requests, and
fmt / clippy (warnings denied) / build / test for Rust. `buf breaking` in CI is
literally blog post 2 — "contracts with teeth" — running on every PR.

Linux and macOS. The Unix-socket transport uses peer credentials
(`SO_PEERCRED`), which Windows' `AF_UNIX` does not provide, so Windows would
need a different identity story.

## Running things

Everything below assumes `cargo build --workspace` has run, and is executed
from the repository root.

### Play a match

```sh
./target/debug/ni-engine run \
  --bot-a target/debug/ni-bot \
  --bot-b target/debug/ni-bot
```

The engine spawns both bots, hands each a listen target on argv, waits for a
readiness line, dials, `Identify`s, announces the match, and then plays it —
rendering the board after every turn and appending a JSON line per turn to
`ni-match.jsonl`.

```
turn 33: chapter A acted
     0  1  2  3  4  5  6  7  8  9
 0   ·  ·  ·  ·  ·  ·  ·  ·  ·  ·
 1   ·  ·  ·  ·  ·  ·  ·  ·  ·  ·
 2   ·  ·  ·  #  #  #  #  ·  ·  ·
 3   ·  ·  · A4  ·  ·  ·  ·  ·  ·
 4   ·  ·  ·  ·  #  #  ·  ·  ·  ·
 5   ·  ·  ·  ·  #  #  ·  ·  ·  ·
 6   ·  ·  ·  ·  ·  ·  ·  ·  ·  ·
 7   ·  ·  ·  #  #  #  #  ·  ·  ·
 8   ·  ·  ·  ·  ·  ·  ·  ·  ·  ·
 9   ·  ·  ·  ·  ·  ·  ·  ·  ·  ·
hp: A4=2
order A4: applied, damage=2
result: chapter A wins by elimination
```

The board goes to **stdout**; diagnostics go to **stderr**. That split is
deliberate — you can pipe one without the other:

```sh
# just the game
./target/debug/ni-engine run --bot-a target/debug/ni-bot --bot-b target/debug/ni-bot 2>/dev/null

# just the commentary
./target/debug/ni-engine run --bot-a target/debug/ni-bot --bot-b target/debug/ni-bot 1>/dev/null
```

Useful flags:

| Flag | Default | What it does |
|---|---|---|
| `--bot-a`, `--bot-b` | required | Paths to the two bot binaries |
| `--bot-a-arg`, `--bot-b-arg` | — | Extra argv for a bot; repeatable. Use `--bot-b-arg=--flag` for values starting with `-` |
| `--delay <ms>` | `0ms` | Pause between turns, for watching a match live |
| `--turn-deadline <ms>` | `500ms` | Per-turn thinking budget, enforced as the gRPC deadline |
| `--strike-limit <n>` | `3` | Consecutive missed deadlines before the match is forfeited |
| `--match-log <path>` | `ni-match.jsonl` | JSONL output |
| `--no-match-log` | off | Write no file. The replay is still built and sent |
| `--match-id <id>` | `m5-demo` | Names the match in logs, traces and socket paths |
| `--transport <tcp\|unix>` | `tcp` | Socket family |
| `--socket-dir <path>` | `$XDG_RUNTIME_DIR/ni` | Where Unix sockets live; ignored for TCP |
| `--quiet` | off | No board, no per-turn render — just the result |

Watch one at human speed:

```sh
./target/debug/ni-engine run \
  --bot-a target/debug/ni-bot --bot-b target/debug/ni-bot \
  --delay 250ms
```

### Choosing a transport

The same match, over loopback TCP or over a Unix domain socket:

```sh
./target/debug/ni-engine run --bot-a target/debug/ni-bot --bot-b target/debug/ni-bot \
  --transport unix
```

Nothing about the game changes — that is the point of milestone M5. What changes
is the socket, and what the bot can *know* about its caller:

```sh
for t in tcp unix; do
  ./target/debug/ni-engine run --bot-a target/debug/ni-bot --bot-b target/debug/ni-bot \
    --transport $t --match-id peer-$t --no-match-log --quiet 2>&1 | grep -m1 identified
done
```

```
INFO rpc{…/Identify…}: identified protocol=1 peer=tcp 127.0.0.1:52290
INFO rpc{…/Identify…}: identified protocol=1 peer=unix pid=32650 uid=0 gid=0
```

Over TCP a bot learns an address and an ephemeral port, which identifies nothing.
Over a Unix socket the kernel hands it the caller's pid, uid and gid — and the
pid there is the *engine's*, supplied by the kernel rather than claimed by the
peer. That is why containerd, the Kubernetes CRI and the kubelet device-plugin
API all speak gRPC over Unix sockets.

Sockets live in a per-match directory, `0700`, that disappears when the match
does:

```sh
./target/debug/ni-engine run --bot-a target/debug/ni-bot --bot-b target/debug/ni-bot \
  --transport unix --socket-dir /tmp/ni-sockets \
  --match-id watch-me --delay 300ms --quiet &
sleep 1 && ls -la /tmp/ni-sockets/watch-me
```

```
drwx------ 2 you you 4096 …  .
srw------- 1 you you    0 …  a.sock
srw------- 1 you you    0 …  b.sock
```

The bots unlink their own sockets on `SIGTERM`; the engine owns the directory.
Both layers are needed, because a `SIGKILL`ed bot runs no destructor. Socket
paths are length-checked against the 108-byte `sun_path` limit *before* anything
tries to bind, so an over-long `--socket-dir` fails with a message that says so.

For the whole argument — what a Unix socket is, what it offers over loopback,
and what it costs — see
[`workbooks/m5-step-by-step.md`](workbooks/m5-step-by-step.md).

### Breaking things on purpose

`roger-the-shrubber` is a bot built entirely out of failure modes. Every flag
below produces a defined engine outcome, never a hang and never a panic.

| Flag | What Roger does | What the engine does |
|---|---|---|
| `--sleep-ms <ms>` | Thinks this long before every `GetOrders` | Forfeits the turn on the deadline, records a strike; the strike limit forfeits the match |
| `--sleep-on-turn <n>` | Restricts the sleep to one turn | Costs one turn, not the match — one good turn clears the strikes |
| `--crash-on-turn <n>` | Exits uncleanly mid-call | Records a `Crash` forfeit with the exit code, and exits cleanly itself |
| `--forget-on-turn <n>` | Answers `SHRUBBERY_REQUIRED` once | Re-sends `NewMatch`, retries the same turn, plays on |
| `--illegal-orders` | Orders a knight off the board | Applies until the first illegal order, logs the reason, wastes the turn |
| `--steal-knights` | Orders the *opponent's* knights | Same: illegal, and just as boring |
| `--wrong-turn` | Echoes the wrong turn number | Discards the answer as a protocol violation |

```sh
# a bot that is always too slow: three strikes and out
./target/debug/ni-engine run \
  --bot-a target/debug/ni-bot \
  --bot-b target/debug/roger-the-shrubber \
  --bot-b-arg --sleep-ms --bot-b-arg 400 \
  --turn-deadline 100ms --quiet

# a bot that dies mid-call
./target/debug/ni-engine run \
  --bot-a target/debug/ni-bot \
  --bot-b target/debug/roger-the-shrubber \
  --bot-b-arg --crash-on-turn --bot-b-arg 4 --quiet

# a bot that forgets the match, and recovers
./target/debug/ni-engine run \
  --bot-a target/debug/ni-bot \
  --bot-b target/debug/roger-the-shrubber \
  --bot-b-arg --forget-on-turn --bot-b-arg 4 --quiet
```

Add `--transport unix` to any of them: the statuses, the reasons and the
outcomes are identical. A deadline over a Unix socket is still a deadline, and a
server dying mid-call is still ambiguous.

What tonic actually returns, observed rather than assumed (tonic 0.13, both
transports):

| Situation | Status the engine receives |
|---|---|
| The per-turn deadline elapses | `Cancelled: Timeout expired` — *not* `DEADLINE_EXCEEDED`, because the client's timer fired first |
| Bot exits during a call | `Unknown: transport error` |
| Call to an already-dead bot | `Unavailable` |
| `SHRUBBERY_REQUIRED` | `FailedPrecondition`, as sent |

The interesting one is the first: the spec's status for a missed deadline is
what a *server* returns when it enforces `grpc-timeout` itself. From the
caller's seat the call was cancelled, and the reason happens to be time. That
gap is blog post 4.

### The match log

One JSON object per line: an opening line, one per turn, and a closing line.

```sh
./target/debug/ni-engine run --bot-a target/debug/ni-bot --bot-b target/debug/ni-bot \
  --transport unix --match-log /tmp/ni.jsonl --quiet
head -1 /tmp/ni.jsonl | jq
```

```json
{
  "kind": "match_started",
  "ts_ms": 1787050870928,
  "match_id": "readme-demo",
  "transport": "unix",
  "turn_deadline_ms": 500,
  "strike_limit": 3,
  "board_width": 10,
  "board_height": 10
}
```

```json
{
  "kind": "turn",
  "ts_ms": 1787050870938,
  "match_id": "readme-demo",
  "turn": 3,
  "acting": "A",
  "status": "ok",
  "latency_us": 1449,
  "deadline_exceeded": false,
  "attempts": 1,
  "strikes": 0,
  "orders": [
    { "unit_id": "A1", "attack_target": "B1", "result": "applied", "damage": 4 }
  ]
}
```

`status` is one of `ok`, `timeout`, `unreachable`, `protocol`. `attempts` is 2
when the turn needed a `SHRUBBERY_REQUIRED` recovery. `trace_id` and `span_id`
appear on every line *if* a collector is configured (see below) and are omitted
otherwise, so the file is one view of the same event the dashboard shows.

Recipes that work:

```sh
# every turn that missed its deadline
jq -c 'select(.kind == "turn" and .status == "timeout")
       | {turn, acting, latency_us, strikes}' /tmp/ni.jsonl

# median GetOrders latency of the turns that worked
jq -s '[.[] | select(.status == "ok") | .latency_us] | sort | .[length/2|floor]' /tmp/ni.jsonl

# how it ended
jq -c 'select(.kind == "match_ended")' /tmp/ni.jsonl

# which transport and clock this match ran under
jq -c 'select(.kind == "match_started") | {transport, turn_deadline_ms, strike_limit}' /tmp/ni.jsonl
```

The same records are assembled into a `ni.v1.Replay` and sent to both bots via
`SubmitReplay` when the match ends, whether or not a log file was written.

### Traces and the dashboard

Ni emits one span per gRPC call, nested `match` → `turn` → `grpc`, and
propagates W3C trace context to the bots in gRPC metadata — so a bot's handler
span appears as a child of the engine's call span, across a process boundary.

With no collector configured, everything still works: human-readable logs on
stderr, no exporters, no trace ids. To see the traces, start the stack and point
Ni at it:

```sh
docker compose -f deploy/telemetry/docker-compose.yml up -d
open http://localhost:3000     # Grafana

OTEL_EXPORTER_OTLP_ENDPOINT=http://localhost:4318 \
  ./target/debug/ni-engine run \
    --bot-a target/debug/ni-bot \
    --bot-b target/debug/roger-the-shrubber \
    --bot-b-arg --sleep-ms --bot-b-arg 400 \
    --turn-deadline 100ms --quiet
```

Then search Tempo for `ni.match_id = "m5-demo"`. A ~34-turn match is about 120
spans across three services (`ni-engine`, `reference-bot`,
`roger-the-shrubber`). Spans carry `ni.transport`, `ni.turn`, `ni.latency_us`,
`ni.replay_bytes` and the OpenTelemetry `rpc.*` conventions, so a slow turn can
be read from both sides of the call at once.

Bots inherit `OTEL_EXPORTER_OTLP_ENDPOINT` from the engine that spawned them —
one variable configures the whole match. Full walkthrough, plus a 100-line OTLP
receiver if you would rather not run Docker, in
[`workbooks/m4-step-by-step.md`](workbooks/m4-step-by-step.md).

### Running a bot by hand

A bot is an ordinary gRPC server; the engine has no special access to it.

```sh
./target/debug/ni-bot --listen tcp://127.0.0.1:50051
./target/debug/ni-bot --listen unix:///tmp/ni-a.sock
```

It prints a readiness line to stdout and nothing else:

```
LISTENING tcp://127.0.0.1:50051
```

That line is the contract between engine and bot at start-up: it carries the
scheme as well as the address, so the engine dials what the bot actually bound
rather than what it was asked to. `:0` asks the kernel for a port and the bot
reports the one it got.

Anything that speaks gRPC can call it — a bot of your own in another language,
or `grpcurl`. Note that server reflection is *not* enabled, so `grpcurl` needs
the contract handed to it:

```sh
grpcurl -plaintext -proto proto/ni/v1/ni.proto \
  -unix -d '{"protocol_version": 1}' \
  /tmp/ni-a.sock ni.v1.BotService/Identify
```

`Identify` and `NewMatch` work cold; `GetOrders` for an unknown match answers
`SHRUBBERY_REQUIRED`, exactly as the contract says it should.

### Environment variables

| Variable | Effect |
|---|---|
| `RUST_LOG` | Standard `tracing` filter. Default `info`. `RUST_LOG=warn` quietens a match; `RUST_LOG=debug` is louder. Inherited by spawned bots |
| `OTEL_EXPORTER_OTLP_ENDPOINT` | OTLP/HTTP base URL, e.g. `http://localhost:4318`. Unset means no exporters — stderr logging only |
| `XDG_RUNTIME_DIR` | Base for Unix sockets (`$XDG_RUNTIME_DIR/ni`). Falls back to `/tmp/ni-<uid>` when unset. Overridden by `--socket-dir` |

## Running the benchmark

`ni-bench` is the measurement harness behind blog post 6: **one service
definition, two transports, four payload sizes**. It deliberately measures the
*real* RPCs rather than a synthetic echo method, so every byte it moves is a byte
the real contract would carry.

| Case | RPC | What it measures |
|---|---|---|
| `Identify` | `Identify` | The floor — a 2-byte request. What a call costs when the payload is nothing |
| `GetOrders` | `GetOrders` | The hot path at its real size (271 bytes) |
| `SubmitReplay 1KiB` | `SubmitReplay` | Small payload (1,044 bytes) |
| `SubmitReplay 1MiB` | `SubmitReplay` | Large payload (1,048,604 bytes) |

Every number is a full client-observed round trip — encode, write, read, decode,
on both sides — measured sequentially, one call at a time. Payload sizes are
`encoded_len()` of the actual message, measured rather than claimed.

### Quickstart

**Build in release mode.** A debug build measures `Vec` bounds checks and
prost's unoptimised decode loop, which is a real thing to know about debug
builds and not what this harness is for.

```sh
cargo build --workspace --release

./target/release/ni-bench --bot target/release/ni-bot --iterations 5000 --warmup 500
```

```
trans  case                     bytes      n   p50 us   p90 us   p99 us   max us    >10ms   calls/s
-----------------------------------------------------------------------------------------------------
tcp    Identify                     2   5000      158      203      275     1248        0      6329
tcp    GetOrders                  271   5000      181      236      339    41873        2      5525
tcp    SubmitReplay 1KiB         1044   5000      228      305      450    44027       15      4386
tcp    SubmitReplay 1MiB      1048604    200    27721    53374    57100    68951      200        36
unix   Identify                     2   5000      168      212      266     2348        0      5952
unix   GetOrders                  271   5000      180      222      294     1008        0      5556
unix   SubmitReplay 1KiB         1044   5000      202      260      347     1075        0      4950
unix   SubmitReplay 1MiB      1048604    200    26561    29283    38129    38650      200        38

case                    tcp p50   unix p50      delta unix MiB/s
----------------------------------------------------------------
Identify                    158        168       6.3%        0.0
GetOrders                   181        180      -0.6%        1.4
SubmitReplay 1KiB           228        202     -11.4%        4.9
SubmitReplay 1MiB         27721      26561      -4.2%       37.7
```

*(4-vCPU Intel Xeon @ 2.10 GHz VM, kernel 6.18, Ubuntu 24.04, rustc 1.94,
release. Your machine will differ — that is the point of running it yourself.)*

### Flags

| Flag | Default | Notes |
|---|---|---|
| `--bot <path>` | `target/debug/ni-bot` | The bot binary to measure against. Point it at the release build |
| `--iterations <n>` | `2000` | Measured calls per case. The 1 MiB case caps itself at 200 |
| `--warmup <n>` | `200` | Unmeasured calls first, excluding connection setup and the HTTP/2 settings exchange |
| `--transport <tcp\|unix\|both>` | `both` | One transport skips the comparison table |
| `--socket-dir <path>` | `$XDG_RUNTIME_DIR/ni` | Where the bench socket lives |
| `--json` | off | One JSON object per row instead of the tables |

### Reading the output

- **`bytes`** — measured protobuf length of the request.
- **`n`** — samples actually taken. The 1 MiB row self-limits, so it will say
  `200` where the others say `5000`. Never compare an `n` you have not read.
- **`p50` / `p90` / `p99` / `max`** — nearest-rank percentiles: every value is an
  observed call, not an interpolation between two of them.
- **`>10ms`** — how many calls exceeded 10 ms. Meaningless on the 1 MiB rows
  (whose median is 27 ms) and the most interesting column on every other row: a
  percentile tells you where the tail sits, this tells you how many calls are in
  it.
- **`calls/s`** — `1 / p50`, i.e. sequential round-trips per second for one
  caller. This is a **latency** harness; it is not a throughput measurement and
  quoting it as "requests per second" would be dishonest.
- **`delta`** — how much of the median moved between transports. Read
  [the next section](#methodology-and-pitfalls) before believing any single value.

### Methodology and pitfalls

Learned the hard way while building this:

1. **Run it at least three times before believing anything.** On the machine
   above, the sign of the median difference flips between runs: across eight
   runs the `Identify` p50 was 156–180 µs over TCP and 166–207 µs over UDS — two
   overlapping ranges. One table is an anecdote.
2. **Release build, always.** See above.
3. **Watch the observer effect.** The bots log an INFO event per handler call.
   At 5,000 iterations that is 5,000 formatted lines written synchronously
   *inside* the timed region, and the first version of this harness measured
   `tracing` as much as it measured the transport. `ni-bench` therefore sets
   `RUST_LOG=warn` unless you have already set it — so if you run it with
   `RUST_LOG=info` for debugging, do not quote the numbers.
4. **Report the machine, the payload, the iteration count and the build
   profile.** "UDS is faster than loopback" is unfalsifiable. The table above
   plus one sentence of context is not.
5. **Quiesce the box.** These are microsecond measurements on a shared kernel;
   a compile in another terminal is visible in the p99.

### What it found

Summarised from [`workbooks/m5-step-by-step.md`](workbooks/m5-step-by-step.md),
Checkpoint 7 — the full argument, with the transcripts, lives there:

- **The medians do not move.** Swapping loopback TCP for a Unix socket changed
  the median gRPC call by less than the run-to-run variance. An empty call costs
  ~160 µs here; removing the entire TCP/IP stack from underneath it does not
  meaningfully change that. Whatever those microseconds are, they are not the
  network.
- **The tail does move, repeatably.** TCP produced 16–17 calls over 10 ms per
  10,000 small calls where UDS produced 0–1, with the TCP maximum landing between
  40.8 ms and 44.4 ms in every one of eight runs. `/proc/net/netstat` confirms
  the mechanism — `DelayedACKs +50` during a TCP run, and not one TCP counter
  moving during a UDS run, because there is no TCP:

  ```sh
  # snapshot TcpExt around a run and diff it
  grep -A1 '^TcpExt' /proc/net/netstat
  ```

- **Serialization is the largest single line item on a big payload.** Of a ~27 ms
  1 MiB round trip, ~3.8 ms is the client encoding and ~7.9 ms the server
  decoding — ~43% is protobuf, and the socket family accounts for about 4%, well
  inside the noise. Decode costs twice encode because decode *allocates*: that
  replay is ~9,500 turn records holding ~38,000 orders with two short strings
  each.

Which is the blog trace's thesis, arriving as data: serialization stays, the
failure model stays, and the hop — the part everyone assumes is expensive — is
the part that turns out not to be.

### The two examples

```sh
# what protobuf costs with no socket involved at all
cargo run --release -p ni-bench --example boundary_cost
```

```
     1044 bytes  encode   4.437µs  decode   6.605µs  clone   1.602µs
  1048604 bytes  encode 3.789318ms  decode 7.863298ms  clone 3.872080ms
```

Subtract these from the harness's numbers and what remains is everything the
transport, HTTP/2 and the scheduler contribute together.

```sh
# what "nobody is there" looks like on each transport
cargo run --release -p ni-bench --example dial_errors
```

```
unix:///tmp/ni-missing.sock
    could not connect to …: transport error: No such file or directory (os error 2)
unix:///tmp/ni-stale.sock
    could not connect to …: transport error: Connection refused (os error 111)
tcp://127.0.0.1:1
    could not connect to …: transport error: tcp connect error: Connection refused (os error 111)
```

A missing path (`ENOENT`) and a stale socket (`ECONNREFUSED`) are different
diagnoses — "the directory was never mounted" versus "the service died" — and
TCP collapses both into one closed port.

### Accumulating results

`--json` exists so the numbers outlive the afternoon:

```sh
./target/release/ni-bench --bot target/release/ni-bot --iterations 5000 --json >> bench.jsonl

jq -c '{transport, case, p50_us, p99_us, over_10ms}' bench.jsonl
```

One object per row, append-only, one file per machine — so a laptop, a server and
a container are comparable six months later.

## Testing

```sh
cargo build --workspace     # integration tests spawn the real binaries
cargo test --workspace
```

That order matters: `cargo test` builds test targets and libraries, not the
sibling binaries the integration tests execute. Without the build first they fail
with `… is missing — run cargo build --workspace first`, which is deliberate.

| Suite | Where | What it covers |
|---|---|---|
| Rules | `crates/ni-game/src/*` | Movement, line of sight, cover, partial application, victory — plus golden full-match tests asserting byte-identical final state |
| Policy | `crates/ni-engine/src/policy.rs` | What each gRPC status *means*: turn forfeit vs match forfeit, strikes, recovery. No I/O |
| Transport | `crates/ni-engine/src/transport.rs` | Flag parsing, listen targets, socket-directory permissions and cleanup, path-length refusal |
| Adapters, log, render | `crates/ni-engine/src/*` | Wire↔game conversion, JSONL shape, board rendering |
| Bot | `crates/ni-bot/src/*` | Strategy, lifecycle, `SHRUBBERY_REQUIRED`, stale-socket handling |
| Failure modes | `crates/ni-engine/tests/failure_modes.rs` | Real processes vs `roger-the-shrubber`: timeouts, strikes, crashes, recovery, illegal orders, broken turn echoes, and the log they produce |
| Transport parity | `crates/ni-engine/tests/transports.rs` | The same deterministic match, the same timeout, the same crash, over both socket families; socket cleanup at both layers |
| Harness | `crates/ni-bench/src/lib.rs` | Percentile arithmetic and payload construction — because a benchmark that reports the wrong number is worse than no benchmark |

The failure-mode and parity suites are the ones worth understanding: they assert
that *whatever a bot does*, the engine returns a result rather than hanging or
panicking, and that swapping the transport changes no outcome.

## Design decisions worth knowing

Longer arguments in [`workbooks/implementation-plan.md`](workbooks/implementation-plan.md)
and the per-milestone workbooks; the short versions:

- **The engine is the authority.** Bots propose, the engine disposes. Nothing a
  bot returns is trusted — unknown unit ids, out-of-range moves, another
  chapter's knights and outright garbage all map to defined outcomes.
- **Partial application is a feature.** Orders apply until the first illegal one,
  then stop. That preserves the "did any of it happen?" ambiguity post 4 needs.
- **A missed deadline forfeits the turn, not the match.** A configurable
  consecutive-strike limit (default 3) forfeits the match, so a dead bot cannot
  stall a tournament.
- **`GetOrders` echoes the turn number**, which makes it idempotent by
  construction — and is the only reason recovering from `SHRUBBERY_REQUIRED` with
  a retry is safe.
- **`DEADLINE_EXCEEDED` is never retried.** The engine cannot know whether the
  bot decided; retrying would invent a decision. `UNAVAILABLE` gets bounded
  retries, `SHRUBBERY_REQUIRED` gets a `NewMatch` and one retry.
- **`BattlefieldView` is a projection from day one**, even though it currently
  contains the whole board. Fog of war later becomes a filtering change rather
  than a breaking one.
- **Trace context travels in gRPC metadata, never in a proto message.** Context
  is per-call transport plumbing; putting it in the contract would make every bot
  author implement it.
- **The transport is recorded, never branched on.** `RunOptions.transport`
  appears four times in the match loop, every one of them writing it to a span or
  a log line. The first `if transport == Unix` in that file would mean something
  had leaked.
- **Telemetry can never fail a match.** No collector, no problem. A log write
  error warns once and drops the writer. The log is evidence, not gameplay.
- **Fixed symmetric layout, no randomness anywhere.** Reproducible matches are
  what make golden tests and transport comparisons possible.

## Documentation

| Document | What it is |
|---|---|
| [`workbooks/wires-and-boundaries.md`](workbooks/wires-and-boundaries.md) | The 7-post blog trace Ni exists to serve |
| [`workbooks/ni-design.md`](workbooks/ni-design.md) | Game and architecture design, with the open questions each milestone settled |
| [`workbooks/implementation-plan.md`](workbooks/implementation-plan.md) | How it gets built, milestone by milestone, and why |
| [`workbooks/m2-core-loop-tutorial.md`](workbooks/m2-core-loop-tutorial.md) | Narrative walkthrough of the core loop |
| [`workbooks/m2-step-by-step.md`](workbooks/m2-step-by-step.md) | **M2** — spawn bots, play a match over loopback TCP, render a board |
| [`workbooks/m3-step-by-step.md`](workbooks/m3-step-by-step.md) | **M3** — deadlines, strikes, recovery, and a bot built to misbehave |
| [`workbooks/m4-step-by-step.md`](workbooks/m4-step-by-step.md) | **M4** — spans, W3C trace context, the JSONL log, and a Grafana stack |
| [`workbooks/m5-step-by-step.md`](workbooks/m5-step-by-step.md) | **M5** — Unix sockets vs loopback, what a UDS actually is, and the measurement harness |

The workbooks are written for someone learning Rust as they go: which file to
open, what to put in it, what the Rust means, why the design is that way, and
what to run before continuing. Every code block in them was compiled, formatted,
clippy-cleaned and run.

## Status

| Milestone | Blog post | State |
|---|---|---|
| M0 — scaffolding, proto, CI | — | done |
| M1 — rules engine | — | done |
| M2 — core loop over loopback TCP | 1 | done |
| M3 — deadlines, strikes, failure policy | 3, 4 | done |
| M4 — spans, trace context, match log, dashboard | 3 | done |
| M5 — Unix sockets and the measurement harness | 5, 6 | done |
| M6 — daemon mode, socket discovery, health checks, tournaments, a Python bot | 2, 7 | next |

M6 is where the peer credentials M5 logs become an authorization check, because
that is the milestone where the engine stops spawning every bot itself.
