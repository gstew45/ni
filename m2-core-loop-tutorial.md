# M2 tutorial companion — the first complete gRPC match

> New to Rust or looking for exact file-by-file instructions? Start with
> [`m2-step-by-step.md`](m2-step-by-step.md). Use this document as the
> architectural and conceptual reference beside that workbook.

M1 gave Ni a deterministic game with no networking. M2 puts a process
boundary around that game:

```text
ni-engine process                         ni-bot process

authoritative MatchState
        |
        | MatchState -> BattlefieldView
        |
        +------ GetOrders(view) -----------> choose an order
        |                                    from proto data
        |<----- GetOrdersResponse ----------
        |
        | KnightOrder -> ni_game::Order
        v
apply_orders()
render board
```

By the end of this milestone, `ni-engine` will start two `ni-bot` processes,
connect to each over loopback TCP, run a match, print every turn, report the
result, and clean up both child processes.

This is a companion rather than a finished implementation. It explains the
pieces, gives representative code, and proposes checkpoints at which to stop
and verify your understanding. Write and test one vertical slice at a time;
do not try to produce the entire match runner in one pass.

## What you should learn

M2 is about five ideas:

1. A `.proto` file produces both sides of a typed RPC boundary.
2. The bot is the gRPC **server**, even though the engine is in charge.
3. Generated wire types and domain types have different jobs.
4. An RPC that looks like a method call is still a fallible call to another
   process.
5. gRPC handles the service protocol, not process startup, readiness, game
   rules, or cleanup.

Those ideas are also the useful raw material for blog post 1.

## Keep the milestone narrow

M2 includes:

- one bot process per seat, per match;
- loopback TCP;
- `Identify`, `NewMatch`, `GetOrders`, and `MatchEnded`;
- explicit conversion between `ni-proto` and `ni-game`;
- a simple deterministic reference strategy;
- the authoritative match loop;
- text rendering;
- a result on stdout;
- child-process cleanup.

M2 does **not** include:

- a deadline on `GetOrders`;
- timeout strikes or retries;
- recovery from `SHRUBBERY_REQUIRED`;
- crash-to-forfeit behavior;
- JSONL logs or replay delivery;
- tracing;
- Unix domain sockets;
- daemon mode.

The v1 proto already has fields and RPCs needed by some of those later
milestones. That does not mean they all need behavior now. For example,
`SubmitReplay` can return an empty success response in M2, while the engine
does not call it. Implementing every failure policy now would hide the first
gRPC lesson under M3 work.

One consequence is deliberate: if a bot hangs during M2, the engine can hang
with it. M3 will make that behavior bounded and observable.

## 1. Understand what M0 generated for you

The contract is `proto/ni/v1/ni.proto`. `crates/ni-proto/build.rs` compiles it,
and this line includes the generated Rust:

```rust
tonic::include_proto!("ni.v1");
```

Do not edit the generated file under `target/`. It is build output.

Two generated modules matter most:

```rust
use ni_proto::ni::v1::{
    bot_service_client::BotServiceClient,
    bot_service_server::{BotService, BotServiceServer},
};
```

- `BotServiceClient` is used by `ni-engine`.
- `BotService` is the trait implemented by `ni-bot`.
- `BotServiceServer` adapts that implementation to tonic's HTTP/2 server.

The direction can initially feel backwards. The bot is a server because it
offers a capability: "given a battlefield view, propose orders." The engine
loads that capability, calls it, and remains authoritative. This is the same
shape as a plugin system even though the transport is TCP in M2.

### What generated message fields look like

Read the generated Rust once. Several protobuf-to-Rust mappings will explain
later compiler errors:

| Proto | Generated Rust |
|---|---|
| `string` | `String` |
| `uint32` | `u32` |
| `repeated Knight` | `Vec<Knight>` |
| message field such as `Position position` | `Option<Position>` |
| enum field such as `Chapter chapter` | `i32` |
| `optional string attack_target` | `Option<String>` |

Nested messages use `Option` because protobuf must distinguish an absent
message from a present message whose scalar fields are all zero.

Enum fields are stored as `i32` because a peer may send a numeric value that
this version of the code does not know. Convert them with `Chapter::try_from`
instead of assuming every integer is valid.

This is an important limitation to mention in the blog: generated types
prevent many mistakes, but they do not make untrusted input valid. A required
application concept can still arrive as `None`, a string can still be empty,
and an enum number can still be unknown.

### The three tonic wrapper types

Every generated service method uses:

```rust
async fn method(
    &self,
    request: tonic::Request<Input>,
) -> Result<tonic::Response<Output>, tonic::Status>;
```

- `Request<T>` is the message plus RPC metadata.
- `Response<T>` is the response message plus response metadata.
- `Status` is the gRPC failure sent to the caller.

Call `request.into_inner()` when you only need the message. Later milestones
will also inspect metadata and deadlines, which is why tonic does not pass
the message by itself.

## 2. Add only the runtime support M2 needs

The existing workspace already has tonic, prost, and Tokio. M2 will also need:

- `clap` for the two command-line interfaces;
- `anyhow` for errors in binaries and orchestration code;
- `tokio-stream` to give an already-bound `TcpListener` to tonic;
- Tokio's `io-util` feature to read a child's readiness line;
- Tokio's `sync` feature for the bot's match map.

A suitable workspace-level shape is:

```toml
[workspace.dependencies]
anyhow = "1"
clap = { version = "4", features = ["derive"] }
tokio-stream = { version = "0.1", features = ["net"] }

tokio = { version = "1", features = [
    "macros",
    "rt-multi-thread",
    "net",
    "process",
    "time",
    "io-util",
    "sync",
] }
```

Then opt into the relevant workspace dependencies from `ni-engine` and
`ni-bot`.

`tokio-stream` is not being added for gRPC streaming. `GetOrders` remains a
unary RPC. `TcpListenerStream` merely adapts a stream of accepted TCP
connections to the interface tonic's server accepts.

### Error types: `Status` versus `anyhow::Error`

Use each error type at the boundary where it belongs:

- A bot RPC returns `Status` because the error crosses gRPC.
- `main`, process spawning, address parsing, and the match runner can return
  `anyhow::Result` because those errors stay inside the engine CLI.
- Illegal game orders are neither of those. They remain normal
  `ni_game::OrderOutcome` values.

Do not turn an illegal move into `Status::invalid_argument`. The RPC request
was successfully received and decoded; the engine's rules decided that an
order was illegal. That distinction keeps the engine authoritative.

## 3. Build the bot server first

Start with a server that can answer only `Identify` correctly. This is the
smallest end-to-end gRPC slice.

A useful eventual structure is:

```text
crates/ni-bot/src/
  lib.rs       service implementation and pure strategy
  main.rs      CLI, TCP listener, tonic server
```

It is fine to begin with everything in `main.rs` and extract `lib.rs` once
the first call works. A library makes the strategy and service easier to
unit-test without starting a process.

### Implement the generated trait

The generated trait requires all five methods, even if M2 does not use all
five. Begin with a service value:

```rust
use std::collections::HashMap;

use ni_proto::ni::v1::Chapter;
use tokio::sync::RwLock;

#[derive(Clone)]
struct MatchInfo {
    chapter: Chapter,
}

#[derive(Default)]
pub struct ReferenceBot {
    matches: RwLock<HashMap<String, MatchInfo>>,
}
```

Why keep a map in a spawn-per-match bot? A process will normally have only
one entry during M2, but the contract is explicitly keyed by `match_id`.
Respecting that lifecycle now makes the later move to a multi-match daemon a
change in process lifetime rather than a redesign of the service.

The first method can be almost mechanical:

```rust
#[tonic::async_trait]
impl BotService for ReferenceBot {
    async fn identify(
        &self,
        _request: Request<IdentifyRequest>,
    ) -> Result<Response<IdentifyResponse>, Status> {
        Ok(Response::new(IdentifyResponse {
            name: "reference-bot".to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            protocol_version: PROTOCOL_VERSION,
        }))
    }

    // Implement the other required methods next.
}
```

`#[tonic::async_trait]` makes async trait methods work for the Rust version
and generated trait used by this project. The generated `BotService` trait is
`Send + Sync + 'static`, which is why mutable service state belongs behind an
async lock rather than in a plain mutable `HashMap`.

### Bind the TCP listener yourself

The implementation plan says the engine passes `tcp://127.0.0.1:0`. Port zero
asks the operating system to choose an unused port. The bot must discover
that port and report it to its parent.

If tonic binds the address internally with `Server::serve`, you do not get a
convenient opportunity to inspect the chosen address. Bind first, then pass
the listener to tonic:

```rust
use std::io::Write as _;

use tokio::net::TcpListener;
use tokio_stream::wrappers::TcpListenerStream;
use tonic::transport::Server;

let bind_address = listen
    .strip_prefix("tcp://")
    .context("M2 listen target must begin with tcp://")?;

let listener = TcpListener::bind(bind_address).await?;
let local_address = listener.local_addr()?;

println!("LISTENING tcp://{local_address}");
std::io::stdout().flush()?;

Server::builder()
    .add_service(BotServiceServer::new(ReferenceBot::default()))
    .serve_with_incoming(TcpListenerStream::new(listener))
    .await?;
```

Use stdout for this one machine-readable readiness line and stderr for bot
logs. The engine will pipe stdout, so flushing the line is part of the small
parent/child launch protocol.

This line means "the socket has been bound," not "the service is healthy."
That is sufficient for spawn-per-match. Proper gRPC health checking belongs
with daemon mode in M6.

### Checkpoint 1: call `Identify` without the engine

Run the bot on a fixed port while developing:

```sh
cargo run -p ni-bot -- --listen tcp://127.0.0.1:50051
```

If `grpcurl` is installed, the server does not need reflection because you
can provide the source proto:

```sh
grpcurl -plaintext \
  -import-path ./proto \
  -proto ni/v1/ni.proto \
  -d '{"protocolVersion": 1}' \
  127.0.0.1:50051 \
  ni.v1.BotService/Identify
```

At this point you have already exercised:

```text
JSON typed at grpcurl
  -> protobuf request
  -> HTTP/2 gRPC call
  -> generated tonic router
  -> your Identify method
  -> generated protobuf response
  -> JSON printed by grpcurl
```

That is a useful blog-post screenshot because the `.proto` file, rather than
a handwritten HTTP route, identifies the service and method.

## 4. Implement the bot's match lifecycle

`NewMatch` establishes which chapter this bot controls. Validate the
application-required fields at the RPC boundary:

```rust
let NewMatchRequest {
    match_id,
    chapter,
    board,
    rules,
    ..
} = request.into_inner();

if match_id.is_empty() {
    return Err(Status::invalid_argument("match_id is required"));
}

let chapter = Chapter::try_from(chapter)
    .map_err(|_| Status::invalid_argument("unknown chapter"))?;

if chapter == Chapter::Unspecified {
    return Err(Status::invalid_argument("chapter is required"));
}

let _board = board.ok_or_else(|| Status::invalid_argument("board is required"))?;
let _rules = rules.ok_or_else(|| Status::invalid_argument("rules are required"))?;

self.matches
    .write()
    .await
    .insert(match_id, MatchInfo { chapter });

Ok(Response::new(NewMatchResponse {}))
```

The underscore names show that M2 validates those messages but the
conservative strategy does not need to retain them. Keeping the full board
and rules in `MatchInfo` is also reasonable if you want to compare later
views against the announced configuration.

In `GetOrders`:

1. Look up `request.match_id`.
2. Reject a missing `view`.
3. Check that the request turn and view turn agree.
4. Check that `view.to_act` is this bot's chapter.
5. Pass the view and chapter to a pure strategy function.
6. Echo the request turn in `GetOrdersResponse`.

An unknown match can already return the future recovery signal:

```rust
let info = self
    .matches
    .read()
    .await
    .get(&request.match_id)
    .cloned()
    .ok_or_else(|| {
        Status::failed_precondition(ni_proto::SHRUBBERY_REQUIRED)
    })?;
```

The M2 engine may treat that status as a fatal runner error. M3 will teach it
to resend `NewMatch` and retry.

`MatchEnded` should remove the map entry and acknowledge a known match. If
`remove` returns `None`, answer with the same
`FAILED_PRECONDITION`/`SHRUBBERY_REQUIRED` status as `GetOrders`; the
contract defines both calls that way. `SubmitReplay` can simply return
`SubmitReplayResponse {}` in M2. Do not return `UNIMPLEMENTED` if you can
cheaply honor the declared RPC with an empty acknowledgement; it makes
version behavior less surprising.

## 5. Give the reference bot a deliberately boring strategy

The first strategy should optimize for these properties, in this order:

1. legal;
2. deterministic;
3. eventually makes progress;
4. easy to explain;
5. tactically clever.

Tactical quality comes last because this project is teaching boundaries, not
search algorithms.

Do not make `ni-bot` depend on `ni-game` to validate its own choices. A bot
should be implementable from the protobuf contract alone; the future Python
bot must have the same information and responsibilities. Sharing the rules
crate would make the process boundary look more type-safe than it really is.

### A safe first algorithm

For each turn:

1. Find the closest living ally/enemy pair.
2. Consider every in-bounds, unoccupied tile the ally can reach.
3. Pick the tile with the smallest Manhattan distance to that enemy.
4. Attack only when the resulting position is adjacent to the enemy.
5. Return one `KnightOrder`.

Returning only one order is conservative, but it avoids two early sources of
collective illegality:

- two friendly knights choosing the same destination;
- an earlier attack killing the target of a later attack.

Adjacent attacks always have line of sight because there is no intermediate
tile on which shrubbery could block the shot. The strategy may use less than
the configured attack range, but it remains easy to reason about.

The heart of it can look like this:

```rust
use std::collections::HashSet;

use ni_proto::ni::v1::{
    BattlefieldView, Chapter, Knight, KnightOrder, Position,
};

fn coords(knight: &Knight) -> Option<(u32, u32)> {
    knight.position.as_ref().map(|p| (p.x, p.y))
}

fn distance(a: (u32, u32), b: (u32, u32)) -> u32 {
    a.0.abs_diff(b.0) + a.1.abs_diff(b.1)
}

fn choose_order(
    view: &BattlefieldView,
    chapter: Chapter,
) -> Option<KnightOrder> {
    let opponent = match chapter {
        Chapter::A => Chapter::B,
        Chapter::B => Chapter::A,
        Chapter::Unspecified => return None,
    };

    let mut best_pair: Option<(&Knight, &Knight)> = None;

    for ally in view.knights.iter().filter(|knight| {
        knight.hp > 0 && Chapter::try_from(knight.chapter).ok() == Some(chapter)
    }) {
        let Some(ally_pos) = coords(ally) else {
            continue;
        };

        for enemy in view.knights.iter().filter(|knight| {
            knight.hp > 0
                && Chapter::try_from(knight.chapter).ok() == Some(opponent)
        }) {
            let Some(enemy_pos) = coords(enemy) else {
                continue;
            };

            let candidate_key = (
                distance(ally_pos, enemy_pos),
                ally.unit_id.as_str(),
                enemy.unit_id.as_str(),
            );

            let is_better = match best_pair {
                None => true,
                Some((best_ally, best_enemy)) => {
                    let best_key = (
                        distance(
                            coords(best_ally).expect("selected ally has a position"),
                            coords(best_enemy).expect("selected enemy has a position"),
                        ),
                        best_ally.unit_id.as_str(),
                        best_enemy.unit_id.as_str(),
                    );
                    candidate_key < best_key
                }
            };

            if is_better {
                best_pair = Some((ally, enemy));
            }
        }
    }

    let (unit, target) = best_pair?;
    let start = coords(unit)?;
    let target_pos = coords(target)?;

    let occupied: HashSet<_> = view
        .knights
        .iter()
        .filter(|knight| knight.hp > 0)
        .filter_map(coords)
        .collect();

    let mut destination = start;
    let mut best_key = (distance(start, target_pos), 0, start.1, start.0);

    for y in 0..view.height {
        for x in 0..view.width {
            let candidate = (x, y);
            let movement = distance(start, candidate);

            if movement > unit.move_range {
                continue;
            }
            if candidate != start && occupied.contains(&candidate) {
                continue;
            }

            let key = (distance(candidate, target_pos), movement, y, x);
            if key < best_key {
                destination = candidate;
                best_key = key;
            }
        }
    }

    let target_distance = distance(destination, target_pos);
    let attack_target = (target_distance <= 1
        && target_distance <= unit.attack_range)
        .then(|| target.unit_id.clone());

    let move_to = (destination != start).then_some(Position {
        x: destination.0,
        y: destination.1,
    });

    Some(KnightOrder {
        unit_id: unit.unit_id.clone(),
        move_to,
        attack_target,
    })
}
```

The string IDs are part of tie-breaking. Without a stable tie-break,
implementation details such as collection order can change the match.

Returning `None` when there are no valid ally/enemy pairs becomes an empty
order list. That is a legal pass. The authoritative engine, not the bot,
decides whether the match has already ended.

### Checkpoint 2: test strategy without gRPC

Write small unit tests that construct `BattlefieldView` values directly:

- an adjacent ally attacks;
- a distant ally moves no farther than `move_range`;
- the selected destination is not occupied;
- the same view always yields the same order;
- no living enemies yields no order.

These tests are not "mocking gRPC." They are testing a pure decision that
does not need a transport. Transport behavior gets its own end-to-end check.

## 6. Spawn a bot and establish a client connection

Now move to `ni-engine`, but do not write the game loop yet. Its first goal
is:

```text
spawn bot -> read address -> connect -> Identify -> stop bot
```

A useful eventual structure is:

```text
crates/ni-engine/src/
  lib.rs           shared engine modules
  main.rs          clap and top-level cleanup
  process.rs       spawn, readiness line, client, shutdown
  convert.rs       ni-game <-> ni-proto
  match_runner.rs  authoritative loop
  render.rs        MatchState -> String
```

Starting with fewer files is fine. The important separation is conceptual:
process supervision, wire conversion, game orchestration, and presentation
are different jobs.

### The readiness line is not gRPC

gRPC does not start the server process or tell the parent which ephemeral
port it selected. Use Tokio's process API:

```rust
use std::process::Stdio;

use anyhow::{Context, Result};
use ni_proto::ni::v1::bot_service_client::BotServiceClient;
use tokio::{
    io::{AsyncBufReadExt, BufReader},
    process::{Child, Command},
};
use tonic::transport::Channel;

pub struct BotProcess {
    pub client: BotServiceClient<Channel>,
    child: Child,
}

impl BotProcess {
    pub async fn spawn(path: &str, label: &str) -> Result<Self> {
        let mut child = Command::new(path)
            .arg("--listen")
            .arg("tcp://127.0.0.1:0")
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .kill_on_drop(true)
            .spawn()
            .with_context(|| format!("failed to spawn {label} from {path}"))?;

        let setup: Result<_> = async {
            let stdout =
                child.stdout.take().context("bot stdout was not piped")?;
            let mut lines = BufReader::new(stdout).lines();

            let ready = lines
                .next_line()
                .await?
                .context("bot exited before its readiness line")?;

            let address = ready
                .strip_prefix("LISTENING tcp://")
                .context("invalid bot readiness line")?;

            let client =
                BotServiceClient::connect(format!("http://{address}"))
                    .await
                    .with_context(|| format!("failed to connect to {label}"))?;

            Ok((client, lines))
        }
        .await;

        let (client, mut lines) = match setup {
            Ok(ready) => ready,
            Err(error) => {
                let _ = child.kill().await;
                let _ = child.wait().await;
                return Err(error);
            }
        };

        let label = label.to_string();
        tokio::spawn(async move {
            while let Ok(Some(line)) = lines.next_line().await {
                eprintln!("[{label}] {line}");
            }
        });

        Ok(Self { client, child })
    }

    pub async fn shutdown(&mut self) -> Result<()> {
        if self.child.try_wait()?.is_none() {
            self.child.kill().await?;
        }
        let _ = self.child.wait().await;
        Ok(())
    }
}
```

There are two address schemes here:

- `tcp://127.0.0.1:0` is Ni's process-launch argument.
- `http://127.0.0.1:54321` is the URI tonic uses to create a plaintext
  channel.

This does not mean the API became REST. gRPC still frames protobuf messages
over HTTP/2; `http://` configures the transport endpoint and indicates that
TLS is not in use.

The background task drains any later stdout lines so the child's pipe cannot
fill. Prefer to keep all human logs on stderr and reserve stdout for
parent/child control messages.

`kill_on_drop(true)` is a safety net. Still call `shutdown` so the process is
explicitly killed and reaped. At the top level, arrange cleanup even when the
match returns an error. Also clean up the first bot if starting the second
one fails:

```rust
let mut bot_a = BotProcess::spawn(bot_a_path, "bot A").await?;
let mut bot_b = match BotProcess::spawn(bot_b_path, "bot B").await {
    Ok(bot) => bot,
    Err(error) => {
        let _ = bot_a.shutdown().await;
        return Err(error);
    }
};

let result = run_match(&mut bot_a, &mut bot_b, options).await;
let _ = bot_a.shutdown().await;
let _ = bot_b.shutdown().await;
result
```

This is already evidence for an important lesson: generated stubs solve the
RPC protocol, while the application still owns process lifetime.

### Handshake before starting a match

Call `Identify` on each client:

```rust
let identity = bot
    .client
    .identify(IdentifyRequest {
        protocol_version: PROTOCOL_VERSION,
    })
    .await?
    .into_inner();

anyhow::ensure!(
    identity.protocol_version == PROTOCOL_VERSION,
    "bot {} speaks protocol {}, engine requires {}",
    identity.name,
    identity.protocol_version,
    PROTOCOL_VERSION,
);
```

The generated client takes `&mut self`, so expect the engine to hold mutable
bot handles. A tonic client is cheap to clone because clones share the
underlying channel, but M2 does not need concurrent calls to the same bot.

### Checkpoint 3: spawn and identify two processes

Before creating `MatchState`, make the engine:

1. parse `ni-engine run --bot-a ... --bot-b ...`;
2. spawn both paths;
3. print both identities;
4. shut both children down;
5. exit successfully.

This isolates process startup and gRPC connection failures from game-loop
failures.

## 7. Make the wire/domain conversion explicit

`ni-game` deliberately does not depend on `ni-proto`. Preserve that
direction:

```text
ni-game  <-- ni-engine -->  ni-proto
                       \
                        gRPC channel
```

The engine is the adapter because it knows both the authoritative domain and
the external contract.

Avoid giving both sets of types the same unqualified names:

```rust
use ni_game::{
    Chapter as GameChapter,
    MatchState,
    Order as GameOrder,
    Position as GamePosition,
};
use ni_proto::ni::v1::{
    Chapter as ProtoChapter,
    KnightOrder as ProtoOrder,
    Position as ProtoPosition,
};
```

You cannot implement `From<ProtoPosition> for GamePosition` inside
`ni-engine`: both the trait and the two types are defined in other crates, so
Rust's orphan rule rejects the implementation. Named conversion functions
are clearer here anyway.

### Convert authoritative state into a view

The projection should be a function even though M2 reveals the whole board:

```rust
pub fn battlefield_view(state: &MatchState) -> BattlefieldView
```

It should copy:

- board width and height;
- shrubbery positions;
- every knight, including dead knights with `hp == 0`;
- the chapter to act;
- the current turn;
- zero for the unused whole-match time budget.

Representative mappings:

```rust
fn chapter_to_proto(chapter: GameChapter) -> i32 {
    match chapter {
        GameChapter::A => ProtoChapter::A as i32,
        GameChapter::B => ProtoChapter::B as i32,
    }
}

fn position_to_proto(position: GamePosition) -> ProtoPosition {
    ProtoPosition {
        x: position.x,
        y: position.y,
    }
}
```

When constructing a proto knight, remember that `position` is an
`Option<ProtoPosition>`:

```rust
ProtoKnight {
    unit_id: knight.id.clone(),
    chapter: chapter_to_proto(knight.chapter),
    position: Some(position_to_proto(knight.pos)),
    hp: knight.hp,
    move_range: knight.move_range,
    attack_range: knight.attack_range,
    attack_damage: knight.damage,
}
```

`Board::shrubbery` is a `HashSet`, whose iteration order is not stable. Sort
the positions before putting them on the wire:

```rust
let mut shrubbery: Vec<_> =
    state.board.shrubbery.iter().copied().collect();
shrubbery.sort();

let shrubbery = shrubbery
    .into_iter()
    .map(position_to_proto)
    .collect();
```

Protobuf does not assign meaning to repeated-field order unless the
application does, but stable output improves reproducibility, tests, logs,
and future byte-level measurements.

Keeping `battlefield_view` as a named projection is the fog-of-war seam.
Later, it can take the receiving chapter and filter visibility without
changing `MatchState` or the proto shape.

### Convert proposed orders back into game orders

This direction is intentionally boring:

```rust
fn order_from_proto(order: ProtoOrder) -> GameOrder {
    GameOrder {
        unit_id: order.unit_id,
        move_to: order.move_to.map(|position| GamePosition {
            x: position.x,
            y: position.y,
        }),
        attack_target: order.attack_target,
    }
}
```

Do not reject off-board positions here. They decoded correctly and map
perfectly to a game position; `apply_orders` should produce
`DestinationOob`. Conversion answers "can this wire value be represented?"
The rules engine answers "is this order legal now?"

### Build `NewMatchRequest`

The request announces stable match configuration:

- a match ID;
- the receiving bot's chapter;
- board dimensions and sorted shrubbery;
- every game rule;
- turn 0 for a fresh match.

The proto reserves a nonzero `NewMatchRequest.turn` for M3 recovery. On
recovery, the engine will resend `NewMatch` with the current turn before
retrying `GetOrders`.

The proto `Rules` also has `turn_deadline_ms` and `timeout_strike_limit`.
Choose the intended defaults, such as 500 and 3, but document that M2 does
not enforce them yet. Alternatively set both to zero until M3. Whichever
choice you make, do not claim to the reader or blog audience that a deadline
exists on the wire until the engine actually calls `Request::set_timeout`.

### Checkpoint 4: test conversion, not implementation details

Useful conversion tests:

- chapters map in both seats;
- every knight appears with the expected ID, position, stats, and HP;
- shrubbery is sorted;
- dead knights remain in the view with zero HP;
- optional move and attack fields survive order conversion;
- invalid game orders are converted and later rejected by `apply_orders`,
  rather than rejected by the adapter.

## 8. Run one turn before running a match

Create `standard_match(Rules::standard())`, call `NewMatch` on both bots, and
then run exactly Chapter A's first turn:

```rust
let turn = state.turn;
let acting = state.to_act;

let response = bot_a
    .client
    .get_orders(GetOrdersRequest {
        match_id: match_id.clone(),
        turn,
        view: Some(battlefield_view(&state)),
    })
    .await?
    .into_inner();

anyhow::ensure!(
    response.turn == turn,
    "bot echoed turn {}, expected {}",
    response.turn,
    turn,
);

let orders: Vec<_> = response
    .orders
    .into_iter()
    .map(order_from_proto)
    .collect();

let (next_state, outcomes) =
    ni_game::apply_orders(state, acting, &orders);
state = next_state;

render_turn(&state, acting, turn, &outcomes);

if ni_game::match_status(&state) == ni_game::MatchStatus::InProgress {
    ni_game::end_turn(&mut state);
}

let status_after_turn = ni_game::match_status(&state);
```

Print the orders, outcomes, state, and `status_after_turn`. The status check
before `end_turn` preserves elimination as the reason; the check afterward
can detect the turn cap. Do not add a loop until this path is correct.

This is the central boundary crossing:

```text
domain state
  -> explicit projection
  -> generated request
  -> protobuf encoding
  -> gRPC transport
  -> generated server dispatch
  -> bot strategy
  -> generated response
  -> protobuf decoding
  -> explicit conversion
  -> authoritative validation
```

Neither generated code nor the bot mutates `MatchState`.

## 9. Turn the one-turn slice into the core loop

The loop has two kinds of terminal check:

1. after applying orders, to catch elimination;
2. after `end_turn`, to catch the turn cap.

That ordering already exists in M1's rules and should not be duplicated in
the bot.

The shape is:

```rust
let final_status = loop {
    let acting = state.to_act;
    let turn = state.turn;
    let view = battlefield_view(&state);

    let request = GetOrdersRequest {
        match_id: match_id.clone(),
        turn,
        view: Some(view),
    };

    let response = match acting {
        GameChapter::A => bot_a.client.get_orders(request).await?,
        GameChapter::B => bot_b.client.get_orders(request).await?,
    }
    .into_inner();

    anyhow::ensure!(
        response.turn == turn,
        "response turn {} does not match request turn {}",
        response.turn,
        turn,
    );

    let orders = response
        .orders
        .into_iter()
        .map(order_from_proto)
        .collect::<Vec<_>>();

    let (next, outcomes) =
        ni_game::apply_orders(state, acting, &orders);
    state = next;

    render_turn(&state, acting, turn, &outcomes);

    match ni_game::match_status(&state) {
        ni_game::MatchStatus::InProgress => {}
        finished => break finished,
    }

    ni_game::end_turn(&mut state);

    match ni_game::match_status(&state) {
        ni_game::MatchStatus::InProgress => {}
        finished => break finished,
    }

    if !delay.is_zero() {
        tokio::time::sleep(delay).await;
    }
};
```

This is representative code; adapt ownership to your final `run_match`
signature. In particular, moving `state` into `apply_orders` and receiving
the next state back is expected because that is M1's API.

M2 can propagate a failed `GetOrders` with `?` and abort the runner after
cleanup. Do not silently label every transport error a forfeit yet. M3 will
distinguish timeout, protocol, precondition, unavailable server, and process
exit.

### Tell both bots how the match ended

Convert `MatchStatus` into one `MatchEndedRequest` per bot. Outcomes are from
the receiving bot's perspective:

| Final result | Message to A | Message to B |
|---|---|---|
| A wins | `WIN` | `LOSS` |
| B wins | `LOSS` | `WIN` |
| draw | `DRAW` | `DRAW` |

Map M1's `EndReason::Elimination` and `EndReason::TurnCap` to their proto
enums. Calls to `MatchEnded` are best-effort because the authoritative result
already exists. Print failures to stderr, but do not erase the match result.

After those calls:

1. print the winner or draw and reason;
2. shut down both child processes;
3. wait for them to exit.

## 10. Render domain state, not protobuf

The renderer belongs on the engine side and should accept `&MatchState`.
Rendering a `BattlefieldView` would accidentally make the presentation
dependent on what one bot is allowed to see once fog of war arrives.

Prefer a function that returns a `String`:

```rust
pub fn render_board(state: &MatchState) -> String
```

That makes formatting testable and leaves the caller in charge of stdout.

For every coordinate:

1. show a living knight ID if one occupies the tile;
2. otherwise show `#` for shrubbery;
3. otherwise show `·`.

Knight display must win over shrubbery display because a knight may stand in
cover. Dead knights do not occupy tiles.

Add:

- the turn number and chapter that just acted;
- each living knight's HP;
- one line per `OrderOutcome`;
- final winner and reason.

Use a fixed cell width large enough for `A1` and `B1`. Avoid terminal-control
codes in M2; ordinary lines are easier to test, redirect, quote in a blog
post, and compare across runs.

A small renderer test can assert that:

- `A1` appears at its starting coordinate;
- a shrubbery tile appears as `#`;
- a knight standing in shrubbery appears as the knight, not `#`;
- a dead knight disappears from the grid but can still be listed as zero HP
  if desired.

## 11. Add the CLI last

The target interface from the plan is:

```sh
ni-engine run \
  --bot-a ./target/debug/ni-bot \
  --bot-b ./target/debug/ni-bot \
  --delay 200ms
```

Use a Clap subcommand so future tournament and replay modes have somewhere
to live:

```rust
use std::{path::PathBuf, time::Duration};

fn parse_duration(
    value: &str,
) -> std::result::Result<Duration, String> {
    let milliseconds = value
        .strip_suffix("ms")
        .ok_or_else(|| "duration must end in ms, for example 200ms".to_string())?
        .parse::<u64>()
        .map_err(|error| format!("invalid millisecond duration: {error}"))?;

    Ok(Duration::from_millis(milliseconds))
}

#[derive(clap::Parser)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(clap::Subcommand)]
enum Command {
    Run {
        #[arg(long)]
        bot_a: PathBuf,
        #[arg(long)]
        bot_b: PathBuf,
        #[arg(long, default_value = "0ms", value_parser = parse_duration)]
        delay: Duration,
        #[arg(long)]
        quiet: bool,
    },
}
```

For M2, a tiny parser that accepts milliseconds is enough, or you can expose
`--delay-ms 200` first and polish the syntax afterward. Duration parsing is
not part of the gRPC lesson.

The bot CLI only needs:

```text
ni-bot --listen tcp://127.0.0.1:0
```

Build the workspace before the smoke command. `cargo run -p ni-engine` builds
the engine package, but the child path must already exist:

```sh
cargo build --workspace
cargo run -p ni-engine -- run \
  --bot-a ./target/debug/ni-bot \
  --bot-b ./target/debug/ni-bot \
  --delay 200ms
```

## 12. Verification plan

Run fast checks after each slice:

```sh
cargo fmt --check
cargo clippy --workspace --all-targets
cargo test --workspace
```

Then perform the real M2 smoke test and verify:

- two different child processes start;
- each reports an ephemeral loopback port;
- the engine identifies both over gRPC;
- each receives its own chapter in `NewMatch`;
- `GetOrders` alternates A, B, A, B;
- response turn numbers match request turn numbers;
- all orders pass through `apply_orders`;
- illegal orders would be outcomes rather than panics;
- board output changes over time;
- the match ends by elimination or turn cap;
- both bots receive `MatchEnded`;
- both child processes are reaped;
- a second run has the same result and turn sequence.

For automated tests, use three layers:

1. Unit-test conversion functions.
2. Unit-test the pure bot strategy.
3. Add one TCP integration test for a short match.

The repository's full verification plan mentions TCP and UDS integration
tests. Only TCP is an M2 gate; add the same test over UDS in M5.

An in-process server test is useful for deterministic CI, but retain a smoke
test that starts real binaries. Running a tonic server and client in one
process proves the RPC wiring; it does not prove the spawn/readiness/cleanup
protocol.

## 13. Common problems and what they mean

### `TcpListenerStream` cannot be imported

Enable the `net` feature on `tokio-stream`:

```toml
tokio-stream = { version = "0.1", features = ["net"] }
```

### `lines()` is missing from `BufReader`

Enable Tokio's `io-util` feature and import `AsyncBufReadExt`.

### The bot compiles only after implementing methods you do not call

The generated server trait represents the complete declared service.
Implement all five methods. `SubmitReplay` can acknowledge and do nothing in
M2.

### A proto `Position` does not fit where a `Position` is expected

It is probably `Option<Position>`. Protobuf message presence is explicit in
generated Rust. Validate incoming required concepts and wrap outgoing nested
messages in `Some`.

### A proto `Chapter` is an `i32`

For outgoing values use `ProtoChapter::A as i32`. For incoming values use
`ProtoChapter::try_from(value)` and handle an unknown number.

### The engine waits forever for the readiness line

Check that the bot:

- accepted `--listen`;
- stripped `tcp://` before binding;
- printed exactly the agreed prefix;
- printed a newline;
- flushed stdout;
- sent logs to stderr.

Startup timeouts are useful but can be added with M3's failure handling.

### The client rejects `tcp://...`

The bot launch target is Ni syntax. Build tonic's channel with
`http://host:port` for plaintext loopback TCP.

### The engine gets connection refused

Print the readiness line only after `TcpListener::bind` succeeds. Do not have
the parent reserve a port and release it for the child; that creates a race
in which another process can claim the port.

### The match never eliminates a chapter

First verify that the turn number advances and that the bot returns a
non-empty order. Then print each `OrderOutcome`. The turn cap should still
guarantee termination, so an infinite loop usually means `end_turn` or the
post-`end_turn` status check is missing.

### Output differs between identical runs

Look for iteration over `HashSet` or `HashMap` without sorting and strategy
choices without explicit tie-breakers.

### Bot processes remain after the engine exits

Use `kill_on_drop(true)` as a safety net, then explicitly kill and `wait` for
each child on both success and error paths.

## 14. Questions to answer while implementing

Keep short notes as you work. These answers can become the spine of blog
post 1:

1. What did the `.proto` file generate for the client and server?
2. Which validation did generated code perform, and which validation was
   still application work?
3. Why is the engine the client and the bot the server?
4. Why are `MatchState` and `BattlefieldView` separate types?
5. What code did gRPC remove compared with designing serialization, routes,
   client models, and error framing independently?
6. What did gRPC not remove—process startup, readiness, conversion,
   supervision, and game validation?
7. Where does an ordinary Rust error become a gRPC `Status`?
8. Why is an illegal order a successful RPC with a game outcome rather than
   a failed RPC?
9. Why does loopback TCP still count as a real process boundary?
10. What would a Python bot need besides the `.proto` file and the game
    rules?

Avoid framing the result as "gRPC makes remote calls local." M2 demonstrates
the opposite: the generated API makes the call convenient, while async
execution, status values, process lifetime, and explicit conversion keep the
boundary visible.

## 15. Material likely to belong in blog post 1

A focused post can use this narrative:

1. Begin with M1: a pure function can apply orders, but another process
   cannot call that function directly.
2. Introduce the desired contract in domain language: identify, announce a
   match, request orders, report the result.
3. Show one service definition and the generated Rust client/server shapes.
4. Explain the surprising plugin direction: engine as client, bot as server.
5. Follow one turn across the boundary.
6. Show explicit `MatchState -> BattlefieldView` projection and explain why
   domain state is not a wire contract.
7. Run two real processes and show the text board.
8. End with what the boundary has already cost: serialization, mapping,
   lifecycle, and failure possibilities.
9. Open the next post with the contract-evolution question: what happens
   when the message changes after a second-language bot has shipped?

Useful concrete exhibits:

- the `BotService` section of the proto;
- the generated trait signature for one unary method;
- the `grpcurl` `Identify` call;
- the one-turn sequence diagram;
- a board frame before and after an RPC;
- the small order-conversion function;
- a note that the same proto will later generate a Python server.

Do not force deadlines, status-code taxonomy, UDS, or benchmarks into the
first post. M2 should make the basic boundary tangible; later milestones
exist so each of those topics can fail in an interesting, observable way.

## M2 definition of done

M2 is complete when this command starts two reference bots and returns
without manual cleanup:

```sh
cargo build --workspace
cargo run -p ni-engine -- run \
  --bot-a ./target/debug/ni-bot \
  --bot-b ./target/debug/ni-bot \
  --delay 200ms
```

The output must show a deterministic sequence of rendered turns and a final
win or draw. Every state mutation must still pass through `ni-game`; every
bot interaction must pass through the generated gRPC contract.

At that point Ni has its first complete boundary. M3 can begin breaking it.
