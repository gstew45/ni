# M2 step by step — a Rust beginner's workbook

This is the hands-on version of
[`m2-core-loop-tutorial.md`](m2-core-loop-tutorial.md). The companion explains
the architecture and the reasons behind it; this workbook tells you which
file to open, what to put there, what the Rust means, and what command to run
before continuing.

The code in this workbook has been checked against the repository's current
M1 implementation and tonic/prost 0.13.

## How to use this workbook

Work through the checkpoints in order. At each checkpoint:

1. edit only the files named in that checkpoint;
2. run `cargo fmt`;
3. run the checkpoint command;
4. fix errors before moving on;
5. make a short note about what you observed.

Do not worry about memorizing the Rust syntax. The repeated patterns—
`Result`, `Option`, `match`, `&mut`, `.await`, and `?`—will become clearer as
you use them.

Run every command from the repository root:

```sh
cd /home/gstewart/projects/ni
```

The relative paths `./proto` and `./target/debug/ni-bot` assume that working
directory.

M2 intentionally implements the happy path. A bot timeout can still hang the
engine. Retries, deadlines, strikes, crash forfeits, and recovery are M3.

## What you will create

```text
crates/ni-bot/src/
  lib.rs                 gRPC service and reference strategy
  main.rs                bot CLI and tonic server

crates/ni-engine/src/
  lib.rs                 module list and public exports
  main.rs                engine CLI and process cleanup
  process.rs             spawn/connect/stop a bot
  convert.rs             ni-game <-> protobuf conversion
  render.rs              text board and results
  match_runner.rs         authoritative game loop
```

You will also edit three `Cargo.toml` files.

## A small Rust map before you start

A Cargo **package** can contain:

- `src/lib.rs`: reusable code called a library crate;
- `src/main.rs`: an executable called a binary crate;
- both at once.

That is what we will use. `ni_bot` in `ni-bot/src/main.rs` refers to the
library defined by `ni-bot/src/lib.rs`. Rust changes the package's hyphen to
an underscore in code.

These forms will appear often:

```rust
use some_crate::SomeType;
```

`use` brings a name into scope so you do not have to write its complete path
every time.

```rust
pub fn example() {}
```

`pub` makes an item available outside its module.

```rust
fn read(value: &Thing) {}
fn change(value: &mut Thing) {}
```

`&Thing` borrows a value without taking ownership. `&mut Thing` borrows it
and permits mutation.

```rust
Option<T>          // Some(value) or None
Result<T, E>       // Ok(value) or Err(error)
```

`Option` models presence. `Result` models success or failure.

```rust
let value = fallible_operation()?;
```

`?` means: on success, take the value; on error, return the error from the
current function.

```rust
let value = async_operation().await?;
```

`.await` pauses this async task until the operation completes. It does not
block the whole Tokio runtime.

```rust
match chapter {
    Chapter::A => do_a(),
    Chapter::B => do_b(),
}
```

`match` handles every possible form of an enum. Rust normally reports an
error if you forget one.

Attributes begin with `#`:

```rust
#[derive(Default)]
#[tokio::main]
#[test]
```

Attributes attach instructions to an item. `#[derive(Default)]` invokes a
derive macro that generates a sensible empty constructor.
`#[tokio::main]` is an attribute macro that transforms `main`, while
`#[test]` and `#[cfg(...)]` are built-in compiler attributes.

Return to this section when syntax looks unfamiliar.

---

## Checkpoint 0 — prove M1 is healthy

Do not begin the networking layer until the pure rules engine passes:

```sh
cargo test -p ni-game
```

Expected result: every `ni-game` test passes.

Why start here? If the final match behaves incorrectly, you want to know that
the rules were already good before gRPC was introduced.

---

## Checkpoint 1 — add the M2 dependencies

### 1.1 Edit the workspace `Cargo.toml`

Open the root `Cargo.toml`.

Under `[workspace.dependencies]`, add `anyhow`, `clap`, and `tokio-stream`.
Replace the existing one-line Tokio entry with the expanded entry below.
The finished dependency section should look like this:

```toml
[workspace.dependencies]
ni-proto = { path = "crates/ni-proto" }
ni-game = { path = "crates/ni-game" }

anyhow = "1"
clap = { version = "4", features = ["derive"] }
tonic = "0.13"
prost = "0.13"
tokio = { version = "1", features = [
    "macros",
    "rt-multi-thread",
    "net",
    "process",
    "time",
    "io-util",
    "sync",
] }
tokio-stream = { version = "0.1", features = ["net"] }

tonic-build = "0.13"
protoc-bin-vendored = "3"
```

What each addition does:

- `anyhow`: convenient error reporting for command-line applications;
- `clap`: turns Rust structs and enums into command-line parsers;
- `io-util`: gives Tokio's buffered reader its `.lines()` method;
- `sync`: gives the bot an async `RwLock`;
- `tokio-stream` with `net`: adapts `TcpListener` for tonic.

`tokio-stream` does not mean the gRPC API is streaming. `GetOrders` remains a
unary request/response RPC.

### 1.2 Edit `crates/ni-bot/Cargo.toml`

Replace its `[dependencies]` section with:

```toml
[dependencies]
ni-proto = { workspace = true }
anyhow = { workspace = true }
clap = { workspace = true }
tonic = { workspace = true }
tokio = { workspace = true }
tokio-stream = { workspace = true }
```

### 1.3 Edit `crates/ni-engine/Cargo.toml`

Replace its `[dependencies]` section with:

```toml
[dependencies]
ni-proto = { workspace = true }
ni-game = { workspace = true }
anyhow = { workspace = true }
clap = { workspace = true }
tonic = { workspace = true }
tokio = { workspace = true }
```

### 1.4 Let Cargo resolve the dependencies

Run:

```sh
cargo check --workspace
```

Cargo will update `Cargo.lock`. Do not edit the lockfile by hand.

Expected result: the existing scaffolding still compiles.

---

## Checkpoint 2 — implement the reference bot

The engine will eventually call the bot, so build the server side first.

### 2.1 Create the smallest compiling service

Create `crates/ni-bot/src/lib.rs` with this temporary implementation:

```rust
use ni_proto::{
    ni::v1::{
        bot_service_server::BotService, GetOrdersRequest,
        GetOrdersResponse, IdentifyRequest, IdentifyResponse,
        MatchEndedRequest, MatchEndedResponse, NewMatchRequest,
        NewMatchResponse, SubmitReplayRequest, SubmitReplayResponse,
    },
    PROTOCOL_VERSION,
};
use tonic::{Request, Response, Status};

#[derive(Default)]
pub struct ReferenceBot;

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

    async fn new_match(
        &self,
        _request: Request<NewMatchRequest>,
    ) -> Result<Response<NewMatchResponse>, Status> {
        Ok(Response::new(NewMatchResponse {}))
    }

    async fn get_orders(
        &self,
        request: Request<GetOrdersRequest>,
    ) -> Result<Response<GetOrdersResponse>, Status> {
        Ok(Response::new(GetOrdersResponse {
            turn: request.into_inner().turn,
            orders: vec![],
        }))
    }

    async fn match_ended(
        &self,
        _request: Request<MatchEndedRequest>,
    ) -> Result<Response<MatchEndedResponse>, Status> {
        Ok(Response::new(MatchEndedResponse {}))
    }

    async fn submit_replay(
        &self,
        _request: Request<SubmitReplayRequest>,
    ) -> Result<Response<SubmitReplayResponse>, Status> {
        Ok(Response::new(SubmitReplayResponse {}))
    }
}
```

Run the first bot-library check:

```sh
cargo fmt
cargo check -p ni-bot --lib
```

This service is intentionally incomplete: it accepts every match and always
passes its turn. Its purpose is to expose the generated trait and prove that
you must implement all five declared RPCs. The next step adds lifecycle
validation and a strategy.

### 2.2 Replace `crates/ni-bot/src/lib.rs` with the complete service

Replace the temporary file with:

```rust
//! Reference bot service and its deliberately conservative strategy.

use std::collections::{HashMap, HashSet};

use ni_proto::{
    ni::v1::{
        bot_service_server::BotService, BattlefieldView, Chapter,
        GetOrdersRequest, GetOrdersResponse, IdentifyRequest,
        IdentifyResponse, Knight, KnightOrder, MatchEndedRequest,
        MatchEndedResponse, NewMatchRequest, NewMatchResponse, Position,
        SubmitReplayRequest, SubmitReplayResponse,
    },
    PROTOCOL_VERSION, SHRUBBERY_REQUIRED,
};
use tokio::sync::RwLock;
use tonic::{Request, Response, Status};

#[derive(Clone, Copy)]
struct MatchInfo {
    chapter: Chapter,
}

#[derive(Default)]
pub struct ReferenceBot {
    matches: RwLock<HashMap<String, MatchInfo>>,
}

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

    async fn new_match(
        &self,
        request: Request<NewMatchRequest>,
    ) -> Result<Response<NewMatchResponse>, Status> {
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

        let _board =
            board.ok_or_else(|| Status::invalid_argument("board is required"))?;
        let _rules =
            rules.ok_or_else(|| Status::invalid_argument("rules are required"))?;

        self.matches
            .write()
            .await
            .insert(match_id, MatchInfo { chapter });

        Ok(Response::new(NewMatchResponse {}))
    }

    async fn get_orders(
        &self,
        request: Request<GetOrdersRequest>,
    ) -> Result<Response<GetOrdersResponse>, Status> {
        let request = request.into_inner();

        let info = self
            .matches
            .read()
            .await
            .get(&request.match_id)
            .copied()
            .ok_or_else(shrubbery_required)?;

        let view = request
            .view
            .ok_or_else(|| Status::invalid_argument("view is required"))?;

        if request.turn != view.turn {
            return Err(Status::invalid_argument(
                "request turn and view turn must match",
            ));
        }

        let to_act = Chapter::try_from(view.to_act)
            .map_err(|_| Status::invalid_argument("view has unknown chapter"))?;

        if to_act != info.chapter {
            return Err(Status::invalid_argument(
                "view is not for this bot's chapter",
            ));
        }

        let orders = choose_order(&view, info.chapter).into_iter().collect();

        Ok(Response::new(GetOrdersResponse {
            turn: request.turn,
            orders,
        }))
    }

    async fn match_ended(
        &self,
        request: Request<MatchEndedRequest>,
    ) -> Result<Response<MatchEndedResponse>, Status> {
        let match_id = request.into_inner().match_id;

        if self.matches.write().await.remove(&match_id).is_none() {
            return Err(shrubbery_required());
        }

        Ok(Response::new(MatchEndedResponse {}))
    }

    async fn submit_replay(
        &self,
        _request: Request<SubmitReplayRequest>,
    ) -> Result<Response<SubmitReplayResponse>, Status> {
        Ok(Response::new(SubmitReplayResponse {}))
    }
}

fn shrubbery_required() -> Status {
    Status::failed_precondition(SHRUBBERY_REQUIRED)
}

fn coords(knight: &Knight) -> Option<(u32, u32)> {
    knight
        .position
        .as_ref()
        .map(|position| (position.x, position.y))
}

fn distance(a: (u32, u32), b: (u32, u32)) -> u32 {
    a.0.abs_diff(b.0) + a.1.abs_diff(b.1)
}

pub fn choose_order(
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
        knight.hp > 0
            && Chapter::try_from(knight.chapter).ok() == Some(chapter)
    }) {
        let Some(ally_position) = coords(ally) else {
            continue;
        };

        for enemy in view.knights.iter().filter(|knight| {
            knight.hp > 0
                && Chapter::try_from(knight.chapter).ok() == Some(opponent)
        }) {
            let Some(enemy_position) = coords(enemy) else {
                continue;
            };

            let candidate_key = (
                distance(ally_position, enemy_position),
                ally.unit_id.as_str(),
                enemy.unit_id.as_str(),
            );

            let is_better = match best_pair {
                None => true,
                Some((best_ally, best_enemy)) => {
                    let best_key = (
                        distance(
                            coords(best_ally)
                                .expect("selected ally has a position"),
                            coords(best_enemy)
                                .expect("selected enemy has a position"),
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
    let target_position = coords(target)?;

    let occupied: HashSet<_> = view
        .knights
        .iter()
        .filter(|knight| knight.hp > 0)
        .filter_map(coords)
        .collect();

    let mut destination = start;
    let mut best_key = (
        distance(start, target_position),
        0,
        start.1,
        start.0,
    );

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

            let key = (
                distance(candidate, target_position),
                movement,
                y,
                x,
            );

            if key < best_key {
                destination = candidate;
                best_key = key;
            }
        }
    }

    let target_distance = distance(destination, target_position);

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

Before adding tests or a server, check the complete library:

```sh
cargo fmt
cargo check -p ni-bot --lib
```

If this fails, fix the library before continuing.

### 2.3 What the top of the file means

```rust
use std::collections::{HashMap, HashSet};
```

`HashMap` stores match IDs and their assigned chapters. `HashSet` stores
occupied board coordinates while choosing a move.

```rust
struct MatchInfo {
    chapter: Chapter,
}
```

This is the bot's small amount of per-match state. `#[derive(Clone, Copy)]`
means the value can be copied out of the map because `Chapter` is also
copyable.

```rust
pub struct ReferenceBot {
    matches: RwLock<HashMap<String, MatchInfo>>,
}
```

Generated tonic service methods receive `&self`, not `&mut self`. The
`RwLock` provides safe interior mutation:

- `.read().await` allows shared reading;
- `.write().await` allows one writer.

Do not use `std::sync::RwLock` in async tonic methods. Waiting for that lock
would block a Tokio worker thread.

### 2.4 What the generated trait implementation means

```rust
#[tonic::async_trait]
impl BotService for ReferenceBot {
```

`BotService` is generated from the proto. `impl ... for ...` means
`ReferenceBot` promises to supply every method required by that trait.

Each method returns:

```rust
Result<Response<SomeMessage>, Status>
```

- `Ok(Response::new(...))` sends a successful gRPC response;
- `Err(Status::...)` sends a gRPC status error.

`request.into_inner()` consumes tonic's wrapper and gives you the protobuf
message.

This destructuring:

```rust
let NewMatchRequest {
    match_id,
    chapter,
    board,
    rules,
    ..
} = request.into_inner();
```

pulls named fields out of the request. `..` means "ignore the other fields."

Generated enum fields are `i32`, so this validation is necessary:

```rust
let chapter = Chapter::try_from(chapter)?;
```

The actual code uses `map_err` because a parsing error must become a gRPC
`Status`.

Generated nested message fields are `Option<T>`, so `board` and `rules` can
be absent. `ok_or_else` turns `None` into an error.

### 2.5 Understand `GetOrders`

This chain:

```rust
let info = self
    .matches
    .read()
    .await
    .get(&request.match_id)
    .copied()
    .ok_or_else(shrubbery_required)?;
```

means:

1. acquire a read lock;
2. look up the match ID;
3. copy its small `MatchInfo`;
4. return `SHRUBBERY_REQUIRED` if it does not exist;
5. use `?` to stop the method on that error.

This line turns `Option<KnightOrder>` into zero or one repeated orders:

```rust
let orders = choose_order(&view, info.chapter).into_iter().collect();
```

`GetOrdersResponse.orders` is a `Vec<KnightOrder>`. An `Option` can be
iterated as either one value (`Some`) or no values (`None`), and `collect`
builds the vector.

### 2.6 Understand the strategy before trusting it

The strategy intentionally returns one order:

1. choose the closest living friendly/enemy pair;
2. enumerate every board tile;
3. discard tiles beyond the knight's move range;
4. discard occupied destinations;
5. choose the destination closest to the enemy;
6. attack only if the enemy is adjacent.

It attacks only when adjacent because an adjacent attack has no intermediate
tile where shrubbery can block line of sight.

The bot does not import `ni-game`. That is deliberate. A future Python bot
must be able to make the same decision using only the protobuf contract. The
engine remains responsible for actual legality.

Several values are cloned:

```rust
target.unit_id.clone()
```

`unit_id` is an owned `String`. The bot only has a borrowed `&Knight`, so it
cannot move the string out of the knight. `.clone()` creates the owned string
required by the response.

The strategy also introduces compact iterator syntax:

- `let Some(value) = expression else { continue; };` unwraps `Some` and
  skips the current loop iteration for `None`.
- `.iter()` borrows each item from a collection.
- `.filter(|item| condition)` retains matching borrowed items.
- `.filter_map(function)` both removes `None` and unwraps `Some`.
- `.collect()` builds a collection inferred from the destination type.
- `(x, y)` is a tuple; `.0` and `.1` access its first and second values.
- `HashSet<_>` asks Rust to infer the stored type from inserted values.
- Boolean `.then(|| value)` and `.then_some(value)` produce `Some(value)`
  when true and `None` when false.

Rust compares tuples from left to right. The strategy's tuple keys therefore
compare distance first, then stable unit IDs or coordinates to break ties.

### 2.7 Append tests to `crates/ni-bot/src/lib.rs`

Add this directly after the final `}` of `choose_order`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use ni_proto::ni::v1::{
        bot_service_server::BotService, BoardLayout, MatchEndedRequest,
        Rules,
    };
    use tonic::Code;

    fn knight(
        id: &str,
        chapter: Chapter,
        x: u32,
        move_range: u32,
    ) -> Knight {
        Knight {
            unit_id: id.to_string(),
            chapter: chapter as i32,
            position: Some(Position { x, y: 0 }),
            hp: 10,
            move_range,
            attack_range: 2,
            attack_damage: 4,
        }
    }

    fn view(a_x: u32, b_x: u32) -> BattlefieldView {
        BattlefieldView {
            width: 8,
            height: 1,
            shrubbery: vec![],
            knights: vec![
                knight("A1", Chapter::A, a_x, 3),
                knight("B1", Chapter::B, b_x, 3),
            ],
            to_act: Chapter::A as i32,
            turn: 1,
            time_remaining_ms: 0,
        }
    }

    #[test]
    fn distant_knight_moves_deterministically_toward_enemy() {
        let view = view(0, 7);
        let first = choose_order(&view, Chapter::A).unwrap();
        let second = choose_order(&view, Chapter::A).unwrap();

        assert_eq!(first, second);
        assert_eq!(first.unit_id, "A1");
        assert_eq!(first.move_to, Some(Position { x: 3, y: 0 }));
        assert_eq!(first.attack_target, None);
    }

    #[test]
    fn adjacent_knight_attacks_without_moving() {
        let order = choose_order(&view(3, 4), Chapter::A).unwrap();

        assert_eq!(order.move_to, None);
        assert_eq!(order.attack_target.as_deref(), Some("B1"));
    }

    #[test]
    fn no_living_enemy_means_no_order() {
        let mut view = view(0, 7);
        view.knights[1].hp = 0;

        assert_eq!(choose_order(&view, Chapter::A), None);
    }

    #[tokio::test]
    async fn lifecycle_uses_shrubbery_required_for_unknown_matches() {
        let bot = ReferenceBot::default();

        let error = bot
            .get_orders(Request::new(GetOrdersRequest {
                match_id: "missing".to_string(),
                turn: 1,
                view: Some(view(0, 7)),
            }))
            .await
            .unwrap_err();

        assert_eq!(error.code(), Code::FailedPrecondition);
        assert_eq!(error.message(), SHRUBBERY_REQUIRED);

        bot.new_match(Request::new(NewMatchRequest {
            match_id: "known".to_string(),
            chapter: Chapter::A as i32,
            board: Some(BoardLayout {
                width: 8,
                height: 1,
                shrubbery: vec![],
            }),
            rules: Some(Rules::default()),
            turn: 0,
        }))
        .await
        .unwrap();

        bot.match_ended(Request::new(MatchEndedRequest {
            match_id: "known".to_string(),
            outcome: 0,
            reason: 0,
        }))
        .await
        .unwrap();
    }
}
```

`#[cfg(test)]` means the module is compiled only by test builds.
`use super::*` imports the parent module's private items, which lets the tests
call `choose_order`.

`#[tokio::test]` is the async version of `#[test]`; it supplies a Tokio
runtime so the test can `.await` the lock and service methods.

### 2.8 Replace `crates/ni-bot/src/main.rs`

Delete the scaffolding in that file and replace it with:

```rust
use std::io::Write as _;

use anyhow::{Context, Result};
use clap::Parser;
use ni_bot::ReferenceBot;
use ni_proto::ni::v1::bot_service_server::BotServiceServer;
use tokio::net::TcpListener;
use tokio_stream::wrappers::TcpListenerStream;
use tonic::transport::Server;

#[derive(Parser)]
#[command(about = "Run the Ni reference bot gRPC server")]
struct Cli {
    #[arg(long, default_value = "tcp://127.0.0.1:0")]
    listen: String,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    let bind_address = cli
        .listen
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

    Ok(())
}
```

Important pieces:

- `#[tokio::main]` creates the async runtime and permits `.await` in `main`.
- `Cli::parse()` is generated by Clap's `Parser` derive.
- binding port `0` lets the operating system choose a free port;
- `local_addr()` discovers that chosen port;
- stdout is flushed because the engine will wait for that readiness line;
- `BotServiceServer::new(...)` wraps your trait implementation for tonic.

`use std::io::Write as _;` brings the `Write` trait into scope without
introducing a local name. The trait provides `.flush()` for stdout.

### 2.9 Format and test the bot

Run:

```sh
cargo fmt
cargo test -p ni-bot
cargo check -p ni-bot
```

Expected result: four bot tests pass.

### 2.10 Run the bot by itself

Run it on a fixed development port:

```sh
cargo run -p ni-bot -- --listen tcp://127.0.0.1:50051
```

Expected first line:

```text
LISTENING tcp://127.0.0.1:50051
```

The command keeps running because it is a server. Stop it with Ctrl-C after
the next optional check.

If `grpcurl` is installed, call `Identify` from another terminal:

```sh
grpcurl -plaintext \
  -import-path ./proto \
  -proto ni/v1/ni.proto \
  -d '{"protocolVersion": 1}' \
  127.0.0.1:50051 \
  ni.v1.BotService/Identify
```

This is the first complete gRPC call. `grpcurl` encoded a protobuf request,
tonic routed it to `ReferenceBot::identify`, and the generated response was
decoded for display.

---

## Checkpoint 3 — teach the engine to start a bot process

Do not write the game loop yet. First prove this smaller path:

```text
engine -> spawn bot -> read port -> connect -> Identify -> stop bot
```

### 3.1 Create `crates/ni-engine/src/process.rs`

Put this in the new file:

```rust
//! Spawn, connect to, and reap one bot process.

use std::{path::Path, process::Stdio};

use anyhow::{Context, Result};
use ni_proto::ni::v1::bot_service_client::BotServiceClient;
use tokio::{
    io::{AsyncBufReadExt, BufReader},
    process::{Child, Command},
    task::JoinHandle,
};
use tonic::transport::Channel;

pub struct BotProcess {
    pub client: BotServiceClient<Channel>,
    endpoint: String,
    child: Child,
    stdout_task: JoinHandle<()>,
}

impl BotProcess {
    pub async fn spawn(path: &Path, label: &str) -> Result<Self> {
        let mut child = Command::new(path)
            .arg("--listen")
            .arg("tcp://127.0.0.1:0")
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .kill_on_drop(true)
            .spawn()
            .with_context(|| {
                format!("failed to spawn {label} from {}", path.display())
            })?;

        let setup: Result<_> = async {
            let stdout = child
                .stdout
                .take()
                .context("bot stdout was not piped")?;

            let mut lines = BufReader::new(stdout).lines();

            let ready = lines
                .next_line()
                .await?
                .context("bot exited before its readiness line")?;

            let address = ready
                .strip_prefix("LISTENING tcp://")
                .context("invalid bot readiness line")?
                .to_string();

            let client =
                BotServiceClient::connect(format!("http://{address}"))
                    .await
                    .with_context(|| {
                        format!("failed to connect to {label} at {address}")
                    })?;

            Ok((client, address, lines))
        }
        .await;

        let (client, endpoint, mut lines) = match setup {
            Ok(ready) => ready,
            Err(error) => {
                let _ = child.kill().await;
                let _ = child.wait().await;
                return Err(error);
            }
        };

        let label = label.to_string();
        let stdout_task = tokio::spawn(async move {
            while let Ok(Some(line)) = lines.next_line().await {
                eprintln!("[{label}] {line}");
            }
        });

        Ok(Self {
            client,
            endpoint,
            child,
            stdout_task,
        })
    }

    pub fn id(&self) -> Option<u32> {
        self.child.id()
    }

    pub fn endpoint(&self) -> &str {
        &self.endpoint
    }

    pub async fn shutdown(&mut self) -> Result<()> {
        if self.child.try_wait()?.is_none() {
            self.child.kill().await?;
        }

        let _ = self.child.wait().await;
        let _ = (&mut self.stdout_task).await;
        Ok(())
    }
}
```

### 3.2 Understand the process ownership

`BotProcess` owns four things:

- a generated gRPC client;
- the selected TCP address;
- the child operating-system process;
- a Tokio task draining the child's stdout.

This field is generic:

```rust
BotServiceClient<Channel>
```

`BotServiceClient` can work over different transports. In M2, its transport
is tonic's TCP `Channel`.

The engine launches the bot with:

```rust
Command::new(path)
```

This is Tokio's asynchronous process command, not
`std::process::Command`.

The bot is passed `tcp://127.0.0.1:0`, but tonic connects to
`http://127.0.0.1:<chosen-port>`. The `http://` URI configures a plaintext
HTTP/2 channel; the RPC is still gRPC, not REST.

`kill_on_drop(true)` is emergency protection. `shutdown` still explicitly
kills and waits for the child so it is reaped correctly.

The async block assigned to `setup` lets the method clean up the child if any
startup step fails before a complete `BotProcess` exists.

The `async move` stdout task takes ownership of `lines` and `label`. It keeps
reading so the pipe cannot fill and block the bot.

### 3.3 Create the first version of `crates/ni-engine/src/lib.rs`

Create the file with:

```rust
//! M2 match orchestration around the pure `ni-game` rules engine.

pub mod process;

pub use process::BotProcess;
```

`mod process` tells Rust to compile `process.rs`. `pub use` re-exports
`BotProcess`, allowing the binary to import `ni_engine::BotProcess`.

### 3.4 Temporarily replace `crates/ni-engine/src/main.rs`

This is an intermediate program. It starts and identifies two bots but does
not run a game yet.

Replace the file with:

```rust
use std::path::PathBuf;

use anyhow::{ensure, Result};
use clap::{Parser, Subcommand};
use ni_engine::BotProcess;
use ni_proto::{
    ni::v1::IdentifyRequest,
    PROTOCOL_VERSION,
};

#[derive(Parser)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    Run {
        #[arg(long)]
        bot_a: PathBuf,
        #[arg(long)]
        bot_b: PathBuf,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    match Cli::parse().command {
        Command::Run { bot_a, bot_b } => run(bot_a, bot_b).await,
    }
}

async fn run(bot_a_path: PathBuf, bot_b_path: PathBuf) -> Result<()> {
    let mut bot_a = BotProcess::spawn(&bot_a_path, "bot A").await?;

    let mut bot_b = match BotProcess::spawn(&bot_b_path, "bot B").await {
        Ok(bot) => bot,
        Err(error) => {
            let _ = bot_a.shutdown().await;
            return Err(error);
        }
    };

    let work: Result<()> = async {
        identify(&mut bot_a, "A").await?;
        identify(&mut bot_b, "B").await?;
        Ok(())
    }
    .await;

    let cleanup_a = bot_a.shutdown().await;
    let cleanup_b = bot_b.shutdown().await;

    work?;
    cleanup_a?;
    cleanup_b?;
    Ok(())
}

async fn identify(bot: &mut BotProcess, label: &str) -> Result<()> {
    let identity = bot
        .client
        .identify(IdentifyRequest {
            protocol_version: PROTOCOL_VERSION,
        })
        .await?
        .into_inner();

    ensure!(
        identity.protocol_version == PROTOCOL_VERSION,
        "bot {label} speaks protocol {}, engine requires {}",
        identity.protocol_version,
        PROTOCOL_VERSION,
    );

    println!(
        "bot {label}: pid={}, tcp://{}, {} {}",
        bot.id()
            .map(|id| id.to_string())
            .unwrap_or_else(|| "unknown".to_string()),
        bot.endpoint(),
        identity.name,
        identity.version,
    );

    Ok(())
}
```

Notice that a mutable reference is required:

```rust
identify(&mut bot_a, "A")
```

Generated tonic client methods use `&mut self`, so the client must be reached
through a mutable bot handle.

The `work` result is saved before cleanup. This ensures both child processes
are stopped even if `Identify` fails.

### 3.5 Build the bot binary before running the engine

Run:

```sh
cargo fmt
cargo build -p ni-bot -p ni-engine
cargo run -p ni-engine -- run \
  --bot-a ./target/debug/ni-bot \
  --bot-b ./target/debug/ni-bot
```

Expected result:

- two lines identify two different process IDs;
- two different loopback ports are printed;
- the engine exits;
- both bot processes stop.

If this checkpoint fails, do not add the game loop. Fix process startup or
gRPC connection first.

---

## Checkpoint 4 — create the protobuf/domain adapter

The engine understands both type systems:

```text
ni-game domain types <-> ni-engine adapter <-> ni-proto wire types
```

The bot does not receive `MatchState`. It receives `BattlefieldView`.

### 4.1 Create `crates/ni-engine/src/convert.rs`

Put this in the new file:

```rust
//! Explicit adapters between authoritative game types and wire types.

use ni_game::{
    Chapter as GameChapter, MatchState, Order as GameOrder,
    Position as GamePosition,
};
use ni_proto::ni::v1::{
    BattlefieldView, BoardLayout, Chapter as ProtoChapter,
    Knight as ProtoKnight, KnightOrder as ProtoOrder, NewMatchRequest,
    Position as ProtoPosition, Rules as ProtoRules,
};

pub fn chapter_to_proto(chapter: GameChapter) -> i32 {
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

pub fn board_layout(state: &MatchState) -> BoardLayout {
    let mut shrubbery: Vec<_> =
        state.board.shrubbery.iter().copied().collect();
    shrubbery.sort();

    BoardLayout {
        width: state.board.width,
        height: state.board.height,
        shrubbery: shrubbery
            .into_iter()
            .map(position_to_proto)
            .collect(),
    }
}

pub fn rules_to_proto(state: &MatchState) -> ProtoRules {
    ProtoRules {
        knight_hp: state.rules.knight_hp,
        move_range: state.rules.move_range,
        attack_range: state.rules.attack_range,
        attack_damage: state.rules.attack_damage,
        cover_damage_reduction: state.rules.cover_damage_reduction,
        turn_cap: state.rules.turn_cap,
        turn_deadline_ms: 0,
        timeout_strike_limit: 0,
    }
}

pub fn new_match_request(
    state: &MatchState,
    match_id: &str,
    chapter: GameChapter,
) -> NewMatchRequest {
    NewMatchRequest {
        match_id: match_id.to_string(),
        chapter: chapter_to_proto(chapter),
        board: Some(board_layout(state)),
        rules: Some(rules_to_proto(state)),
        turn: 0,
    }
}

pub fn battlefield_view(state: &MatchState) -> BattlefieldView {
    let board = board_layout(state);

    BattlefieldView {
        width: board.width,
        height: board.height,
        shrubbery: board.shrubbery,
        knights: state
            .knights
            .iter()
            .map(|knight| ProtoKnight {
                unit_id: knight.id.clone(),
                chapter: chapter_to_proto(knight.chapter),
                position: Some(position_to_proto(knight.pos)),
                hp: knight.hp,
                move_range: knight.move_range,
                attack_range: knight.attack_range,
                attack_damage: knight.damage,
            })
            .collect(),
        to_act: chapter_to_proto(state.to_act),
        turn: state.turn,
        time_remaining_ms: 0,
    }
}

pub fn order_from_proto(order: ProtoOrder) -> GameOrder {
    GameOrder {
        unit_id: order.unit_id,
        move_to: order.move_to.map(|position| GamePosition {
            x: position.x,
            y: position.y,
        }),
        attack_target: order.attack_target,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ni_game::{apply_orders, IllegalReason, OrderResult, Rules};

    #[test]
    fn view_contains_sorted_shrubbery_and_all_knights() {
        let mut state = ni_game::standard_match(Rules::standard());
        state.knights[0].hp = 0;

        let view = battlefield_view(&state);

        assert_eq!(view.knights.len(), state.knights.len());
        assert_eq!(view.knights[0].unit_id, "A1");
        assert_eq!(view.knights[0].hp, 0);
        assert_eq!(
            view.knights[0].position,
            Some(ProtoPosition { x: 1, y: 1 })
        );
        assert!(view
            .shrubbery
            .windows(2)
            .all(|pair| {
                (pair[0].x, pair[0].y)
                    <= (pair[1].x, pair[1].y)
            }));
    }

    #[test]
    fn both_chapters_map_to_the_wire_enum() {
        assert_eq!(
            chapter_to_proto(GameChapter::A),
            ProtoChapter::A as i32
        );
        assert_eq!(
            chapter_to_proto(GameChapter::B),
            ProtoChapter::B as i32
        );
    }

    #[test]
    fn order_option_fields_survive_conversion() {
        let order = order_from_proto(ProtoOrder {
            unit_id: "A1".to_string(),
            move_to: Some(ProtoPosition { x: 2, y: 1 }),
            attack_target: Some("B1".to_string()),
        });

        assert_eq!(order.unit_id, "A1");
        assert_eq!(
            order.move_to,
            Some(GamePosition { x: 2, y: 1 })
        );
        assert_eq!(order.attack_target.as_deref(), Some("B1"));
    }

    #[test]
    fn adapter_preserves_illegal_positions_for_game_validation() {
        let state = ni_game::standard_match(Rules::standard());
        let order = order_from_proto(ProtoOrder {
            unit_id: "A1".to_string(),
            move_to: Some(ProtoPosition { x: 999, y: 999 }),
            attack_target: None,
        });

        let (_, outcomes) =
            apply_orders(state, GameChapter::A, &[order]);

        assert_eq!(
            outcomes[0].result,
            OrderResult::Illegal {
                reason: IllegalReason::DestinationOob,
            }
        );
    }
}
```

### 4.2 Update `crates/ni-engine/src/lib.rs`

Replace it with:

```rust
//! M2 match orchestration around the pure `ni-game` rules engine.

pub mod convert;
pub mod process;

pub use process::BotProcess;
```

### 4.3 Understand why conversion is not validation

The engine has two different `Chapter` types, so the imports rename them:

```rust
Chapter as GameChapter
Chapter as ProtoChapter
```

This avoids confusing domain values with wire values.

The new-match turn is `0`, even though M1's first playable turn is `1`.
The proto defines zero as a fresh match and reserves a nonzero value for M3
recovery.

The deadline fields are zero because M2 does not enforce deadlines yet.

Shrubbery is sorted because `ni-game` stores it in a `HashSet`. Hash-set
iteration order is not deterministic; sorting keeps views and later logs
stable.

This conversion deliberately preserves an off-board position:

```rust
ProtoPosition { x: 999, y: 999 }
```

It is representable as a game `Position`, so conversion succeeds. The
authoritative `apply_orders` function then reports `DestinationOob`.

### 4.4 Run the adapter tests

```sh
cargo fmt
cargo test -p ni-engine convert
```

Expected result: four conversion tests pass.

---

## Checkpoint 5 — create the text renderer

Render the authoritative `MatchState`, not the bot's `BattlefieldView`.

### 5.1 Create `crates/ni-engine/src/render.rs`

Put this in the file:

```rust
//! Plain deterministic text rendering of authoritative game state.

use std::fmt::Write as _;

use ni_game::{
    Chapter, EndReason, MatchState, MatchStatus, OrderOutcome, OrderResult,
};

pub fn render_board(state: &MatchState) -> String {
    let mut output = String::new();
    output.push_str("   ");

    for x in 0..state.board.width {
        write!(output, " {x:>2}")
            .expect("writing to String cannot fail");
    }
    output.push('\n');

    for y in 0..state.board.height {
        write!(output, "{y:>2} ")
            .expect("writing to String cannot fail");

        for x in 0..state.board.width {
            let position = ni_game::Position { x, y };

            let cell =
                if let Some(knight) = state.living_at(position) {
                    knight.id.as_str()
                } else if state.board.has_shrubbery(position) {
                    "#"
                } else {
                    "·"
                };

            write!(output, " {cell:>2}")
                .expect("writing to String cannot fail");
        }
        output.push('\n');
    }

    output
}

pub fn render_turn(
    state: &MatchState,
    acting: Chapter,
    turn: u32,
    outcomes: &[OrderOutcome],
) -> String {
    let mut output =
        format!("turn {turn}: chapter {} acted\n", chapter_name(acting));

    output.push_str(&render_board(state));
    output.push_str("hp:");

    for knight in state.knights.iter().filter(|knight| knight.is_alive()) {
        write!(output, " {}={}", knight.id, knight.hp)
            .expect("writing to String cannot fail");
    }
    output.push('\n');

    if outcomes.is_empty() {
        output.push_str("orders: pass\n");
    } else {
        for outcome in outcomes {
            write!(output, "order {}: ", outcome.order.unit_id)
                .expect("writing to String cannot fail");

            match &outcome.result {
                OrderResult::Applied { damage_dealt } => {
                    writeln!(
                        output,
                        "applied, damage={damage_dealt}"
                    )
                    .expect("writing to String cannot fail");
                }
                OrderResult::Illegal { reason } => {
                    writeln!(output, "illegal, {reason:?}")
                        .expect("writing to String cannot fail");
                }
                OrderResult::NotReached => {
                    output.push_str("not reached\n");
                }
            }
        }
    }

    output
}

pub fn render_result(status: MatchStatus) -> String {
    match status {
        MatchStatus::InProgress => {
            "result: match still in progress".to_string()
        }
        MatchStatus::Winner { chapter, reason } => format!(
            "result: chapter {} wins by {}",
            chapter_name(chapter),
            reason_name(reason)
        ),
        MatchStatus::Draw { reason } => {
            format!("result: draw by {}", reason_name(reason))
        }
    }
}

fn chapter_name(chapter: Chapter) -> &'static str {
    match chapter {
        Chapter::A => "A",
        Chapter::B => "B",
    }
}

fn reason_name(reason: EndReason) -> &'static str {
    match reason {
        EndReason::Elimination => "elimination",
        EndReason::TurnCap => "turn cap",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ni_game::Rules;

    #[test]
    fn board_renders_knights_and_shrubbery() {
        let state = ni_game::standard_match(Rules::standard());
        let board = render_board(&state);

        assert!(board.contains("A1"));
        assert!(board.contains('#'));
    }

    #[test]
    fn living_knight_wins_over_shrubbery_and_dead_knight_disappears() {
        let mut state = ni_game::standard_match(Rules::standard());
        let a1_position = state.knight("A1").unwrap().pos;

        state.board.shrubbery.clear();
        state.board.shrubbery.insert(a1_position);

        assert!(render_board(&state).contains("A1"));
        assert!(!render_board(&state).contains('#'));

        state
            .knights
            .iter_mut()
            .find(|knight| knight.id == "A1")
            .unwrap()
            .hp = 0;

        let board = render_board(&state);
        assert!(!board.contains("A1"));
        assert!(board.contains('#'));
    }
}
```

### 5.2 Add the module to `crates/ni-engine/src/lib.rs`

Change the module list to:

```rust
pub mod convert;
pub mod process;
pub mod render;
```

Keep this export beneath it:

```rust
pub use process::BotProcess;
```

### 5.3 Understand the renderer's Rust

`write!` normally writes to files or stdout, but `String` implements Rust's
formatting writer interface. Importing:

```rust
use std::fmt::Write as _;
```

brings the trait's formatting method into scope so `write!` can target the
string. The underscore avoids adding the trait name as a directly usable
local name.

Writing to a `String` is effectively infallible, so the code uses:

```rust
.expect("writing to String cannot fail")
```

This is different from network input. Do not use `expect` for a failed RPC or
missing untrusted protobuf field.

`if let Some(knight)` handles the occupied case while ignoring `None`.

A knight is checked before shrubbery because knights may stand in cover. A
dead knight is not returned by `living_at`.

### 5.4 Run renderer and adapter tests

```sh
cargo fmt
cargo test -p ni-engine
```

Expected result: the conversion and renderer tests pass. The temporary
spawn-and-identify engine still compiles.

---

## Checkpoint 6 — add the authoritative match loop

This is where all previous slices meet:

```text
state -> view -> RPC -> proto order -> game order -> apply -> render
```

### 6.1 Create `crates/ni-engine/src/match_runner.rs`

Put this in the new file:

```rust
//! The authoritative M2 match loop.

use std::time::Duration;

use anyhow::{ensure, Result};
use ni_game::{Chapter, EndReason, MatchStatus, Rules};
use ni_proto::{
    ni::v1::{
        GetOrdersRequest, IdentifyRequest, MatchEndReason,
        MatchEndedRequest, MatchOutcome,
    },
    PROTOCOL_VERSION,
};

use crate::{
    convert::{battlefield_view, new_match_request, order_from_proto},
    process::BotProcess,
    render::{render_board, render_result, render_turn},
};

pub struct RunOptions {
    pub delay: Duration,
    pub quiet: bool,
    pub match_id: String,
}

pub async fn run_match(
    bot_a: &mut BotProcess,
    bot_b: &mut BotProcess,
    options: RunOptions,
) -> Result<MatchStatus> {
    identify(bot_a, "A").await?;
    identify(bot_b, "B").await?;

    let mut state = ni_game::standard_match(Rules::standard());

    bot_a
        .client
        .new_match(new_match_request(
            &state,
            &options.match_id,
            Chapter::A,
        ))
        .await?;

    bot_b
        .client
        .new_match(new_match_request(
            &state,
            &options.match_id,
            Chapter::B,
        ))
        .await?;

    if !options.quiet {
        println!("match {} begins", options.match_id);
        print!("{}", render_board(&state));
    }

    let final_status = loop {
        let acting = state.to_act;
        let turn = state.turn;

        let request = GetOrdersRequest {
            match_id: options.match_id.clone(),
            turn,
            view: Some(battlefield_view(&state)),
        };

        let response = match acting {
            Chapter::A => bot_a.client.get_orders(request).await?,
            Chapter::B => bot_b.client.get_orders(request).await?,
        }
        .into_inner();

        ensure!(
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

        let (next_state, outcomes) =
            ni_game::apply_orders(state, acting, &orders);
        state = next_state;

        if !options.quiet {
            print!("{}", render_turn(&state, acting, turn, &outcomes));
        }

        match ni_game::match_status(&state) {
            MatchStatus::InProgress => {}
            finished => break finished,
        }

        ni_game::end_turn(&mut state);

        match ni_game::match_status(&state) {
            MatchStatus::InProgress => {}
            finished => break finished,
        }

        if !options.delay.is_zero() {
            tokio::time::sleep(options.delay).await;
        }
    };

    notify_match_ended(
        bot_a,
        &options.match_id,
        Chapter::A,
        final_status,
    )
    .await;

    notify_match_ended(
        bot_b,
        &options.match_id,
        Chapter::B,
        final_status,
    )
    .await;

    println!("{}", render_result(final_status));
    Ok(final_status)
}

async fn identify(
    bot: &mut BotProcess,
    label: &str,
) -> Result<()> {
    let identity = bot
        .client
        .identify(IdentifyRequest {
            protocol_version: PROTOCOL_VERSION,
        })
        .await?
        .into_inner();

    ensure!(
        identity.protocol_version == PROTOCOL_VERSION,
        "bot {label} ({}) speaks protocol {}, engine requires {}",
        identity.name,
        identity.protocol_version,
        PROTOCOL_VERSION,
    );

    println!(
        "bot {label}: pid={}, tcp://{}, {} {} (protocol {})",
        bot.id()
            .map(|id| id.to_string())
            .unwrap_or_else(|| "unknown".to_string()),
        bot.endpoint(),
        identity.name,
        identity.version,
        identity.protocol_version,
    );

    Ok(())
}

async fn notify_match_ended(
    bot: &mut BotProcess,
    match_id: &str,
    chapter: Chapter,
    status: MatchStatus,
) {
    let request = MatchEndedRequest {
        match_id: match_id.to_string(),
        outcome: outcome_for(status, chapter),
        reason: reason_for(status),
    };

    if let Err(error) = bot.client.match_ended(request).await {
        eprintln!(
            "could not notify bot {chapter:?} that the match ended: {error}"
        );
    }
}

fn outcome_for(status: MatchStatus, recipient: Chapter) -> i32 {
    match status {
        MatchStatus::Winner { chapter, .. }
            if chapter == recipient =>
        {
            MatchOutcome::Win as i32
        }
        MatchStatus::Winner { .. } => MatchOutcome::Loss as i32,
        MatchStatus::Draw { .. } => MatchOutcome::Draw as i32,
        MatchStatus::InProgress => MatchOutcome::Unspecified as i32,
    }
}

fn reason_for(status: MatchStatus) -> i32 {
    let reason = match status {
        MatchStatus::Winner { reason, .. }
        | MatchStatus::Draw { reason } => reason,
        MatchStatus::InProgress => {
            return MatchEndReason::Unspecified as i32;
        }
    };

    match reason {
        EndReason::Elimination => MatchEndReason::Elimination as i32,
        EndReason::TurnCap => MatchEndReason::TurnCap as i32,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn result_is_mapped_from_each_bots_perspective() {
        let status = MatchStatus::Winner {
            chapter: Chapter::A,
            reason: EndReason::Elimination,
        };

        assert_eq!(
            outcome_for(status, Chapter::A),
            MatchOutcome::Win as i32
        );
        assert_eq!(
            outcome_for(status, Chapter::B),
            MatchOutcome::Loss as i32
        );
        assert_eq!(
            reason_for(status),
            MatchEndReason::Elimination as i32
        );
    }
}
```

### 6.2 Understand the loop in plain language

Before the loop:

1. identify both bots;
2. create the authoritative standard match;
3. send `NewMatch` to A;
4. send `NewMatch` to B;
5. render the initial board.

Every loop iteration:

1. remember the acting chapter and turn;
2. project `MatchState` into a protobuf view;
3. choose the matching bot client;
4. await its `GetOrders` response;
5. check the echoed turn;
6. convert every protobuf order;
7. call M1's `apply_orders`;
8. render the new state and outcomes;
9. check for elimination;
10. call `end_turn`;
11. check for the turn cap;
12. optionally sleep so a human can watch.

There are two terminal checks for a reason. A killing blow should be reported
as elimination before advancing the turn. The turn cap is observed after
`end_turn`.

This line moves the current state into M1:

```rust
let (next_state, outcomes) =
    ni_game::apply_orders(state, acting, &orders);
```

`state` cannot be used until it is replaced by `next_state`. That is Rust's
ownership model making the state transition explicit.

This `match` moves the request into exactly one client call:

```rust
let response = match acting {
    Chapter::A => bot_a.client.get_orders(request).await?,
    Chapter::B => bot_b.client.get_orders(request).await?,
};
```

Only one branch runs, so the request is consumed once.

M2 lets a failed RPC return from `run_match` via `?`. M3 will replace that
single policy with status-specific timeout, retry, recovery, and forfeit
behavior.

### 6.3 Replace `crates/ni-engine/src/lib.rs` with its final M2 form

```rust
//! M2 match orchestration around the pure `ni-game` rules engine.

pub mod convert;
pub mod match_runner;
pub mod process;
pub mod render;

pub use match_runner::{run_match, RunOptions};
pub use process::BotProcess;
```

### 6.4 Replace `crates/ni-engine/src/main.rs` with its final M2 form

The temporary identify-only program has done its job. Replace it with:

```rust
use std::{path::PathBuf, time::Duration};

use anyhow::Result;
use clap::{Parser, Subcommand};
use ni_engine::{run_match, BotProcess, RunOptions};

#[derive(Parser)]
#[command(about = "Run authoritative Ni matches")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    Run {
        #[arg(long)]
        bot_a: PathBuf,

        #[arg(long)]
        bot_b: PathBuf,

        #[arg(
            long,
            default_value = "0ms",
            value_parser = parse_duration
        )]
        delay: Duration,

        #[arg(long)]
        quiet: bool,
    },
}

fn parse_duration(
    value: &str,
) -> std::result::Result<Duration, String> {
    let milliseconds = value
        .strip_suffix("ms")
        .ok_or_else(|| {
            "duration must end in ms, for example 200ms".to_string()
        })?
        .parse::<u64>()
        .map_err(|error| {
            format!("invalid millisecond duration: {error}")
        })?;

    Ok(Duration::from_millis(milliseconds))
}

#[tokio::main]
async fn main() -> Result<()> {
    match Cli::parse().command {
        Command::Run {
            bot_a,
            bot_b,
            delay,
            quiet,
        } => run(bot_a, bot_b, delay, quiet).await,
    }
}

async fn run(
    bot_a_path: PathBuf,
    bot_b_path: PathBuf,
    delay: Duration,
    quiet: bool,
) -> Result<()> {
    let mut bot_a = BotProcess::spawn(&bot_a_path, "bot A").await?;

    let mut bot_b = match BotProcess::spawn(&bot_b_path, "bot B").await {
        Ok(bot) => bot,
        Err(error) => {
            if let Err(cleanup_error) = bot_a.shutdown().await {
                eprintln!(
                    "could not clean up bot A after bot B failed to start: \
                     {cleanup_error}"
                );
            }
            return Err(error);
        }
    };

    let match_result = run_match(
        &mut bot_a,
        &mut bot_b,
        RunOptions {
            delay,
            quiet,
            match_id: "m2-demo".to_string(),
        },
    )
    .await;

    let cleanup_a = bot_a.shutdown().await;
    let cleanup_b = bot_b.shutdown().await;

    match_result?;
    cleanup_a?;
    cleanup_b?;
    Ok(())
}
```

The CLI parser accepts `--delay 200ms`. It deliberately supports only
milliseconds so duration parsing does not distract from gRPC.

Cleanup happens after `run_match` whether that function succeeds or returns
an error. The saved result is inspected only after both shutdown calls.

### 6.5 Temporarily run only one chapter-action

Before trusting a 30-turn match, force the rules to stop after one
chapter-action.

In `crates/ni-engine/src/match_runner.rs`, temporarily replace:

```rust
let mut state = ni_game::standard_match(Rules::standard());
```

with:

```rust
let mut one_turn_rules = Rules::standard();
one_turn_rules.turn_cap = 1;
let mut state = ni_game::standard_match(one_turn_rules);
```

Then run:

```sh
cargo fmt
cargo build -p ni-bot -p ni-engine
cargo run -p ni-engine -- run \
  --bot-a ./target/debug/ni-bot \
  --bot-b ./target/debug/ni-bot
```

Expected result:

- both bots are identified;
- Chapter A supplies one order;
- the engine applies and renders it;
- `end_turn` advances the state;
- the match ends as a turn-cap draw;
- both child processes stop.

Now restore the original line:

```rust
let mut state = ni_game::standard_match(Rules::standard());
```

This temporary experiment isolates one trip across the gRPC boundary before
you watch the complete loop.

### 6.6 Format, test, and lint the complete workspace

Run:

```sh
cargo fmt
cargo test --workspace
cargo clippy --workspace --all-targets
```

Do not continue to the smoke test until all three commands succeed.

---

## Checkpoint 7 — run the complete match

Build both binaries first:

```sh
cargo build --workspace
```

Then run:

```sh
cargo run -p ni-engine -- run \
  --bot-a ./target/debug/ni-bot \
  --bot-b ./target/debug/ni-bot \
  --delay 100ms
```

You should see:

1. two bot identities with different PIDs and ports;
2. the initial board;
3. alternating Chapter A and Chapter B turns;
4. movement and attack outcomes;
5. changing HP;
6. a final result;
7. the command returning to the shell.

With the strategy and M1 rules shown here, the validated implementation
produced a Chapter A elimination victory on turn 33. The important property
is not that A wins; it is that repeated runs produce the same sequence.

Run it again with no frames:

```sh
cargo run -p ni-engine -- run \
  --bot-a ./target/debug/ni-bot \
  --bot-b ./target/debug/ni-bot \
  --quiet
```

The identities and final result should still print.

## Follow one turn through your code

After the match works, trace turn 1 manually:

1. `match_runner.rs` owns a `ni_game::MatchState`.
2. `battlefield_view(&state)` constructs a protobuf message.
3. `BotServiceClient::get_orders` serializes and sends it.
4. tonic dispatches the request to `ReferenceBot::get_orders`.
5. `choose_order` returns a protobuf `KnightOrder`.
6. tonic serializes and returns the response.
7. `order_from_proto` constructs a `ni_game::Order`.
8. `apply_orders` validates and mutates the authoritative state.
9. `render_turn` formats that state.

Put temporary `eprintln!` statements at those points if the path still feels
abstract. Remove them after you understand the sequence.

## What belongs to Rust, tonic, protobuf, and Ni?

When learning several things at once, separate their responsibilities:

| Concern | Owner |
|---|---|
| ownership, borrowing, enums, traits | Rust |
| async tasks, TCP listener, child processes | Tokio |
| generated gRPC client/server and `Status` | tonic |
| generated message structs and enum numbers | prost/protobuf |
| legal movement, attacks, victory | `ni-game` |
| process lifecycle and match loop | `ni-engine` |
| strategy | `ni-bot` |

When an error occurs, first ask which layer owns it.

Examples:

- `cannot borrow as mutable`: Rust ownership/borrowing;
- `TcpListenerStream` missing: dependency feature;
- `connection refused`: process/listener lifecycle;
- `Status::invalid_argument`: bot boundary validation;
- `DestinationOob`: game validation;
- match never reaches turn 2: engine loop.

## Beginner troubleshooting

### Rust says a generated message field has the wrong type

Check whether it is a nested protobuf message. Those are usually
`Option<T>`, so outgoing code needs `Some(value)` and incoming code must
handle `None`.

### Rust says `Chapter` should be `i32`

Generated protobuf structs store enum fields as numbers:

```rust
chapter: ProtoChapter::A as i32
```

When reading one:

```rust
ProtoChapter::try_from(number)
```

### Rust says a value was moved

Ask who should own it.

- Borrow with `&value` when the callee only reads.
- Borrow with `&mut value` when the callee changes it.
- Clone an owned field such as `String` when both old and new owners need
  their own copy.
- Intentionally move `state` into `apply_orders`, then use the returned
  state.

Do not add `.clone()` everywhere merely to silence the compiler. It can hide
the ownership design.

### `.await` is rejected

The containing function must be `async`. Executable entry points also need a
runtime, supplied here by `#[tokio::main]`.

### The bot waits forever

That is normal when it is run by itself: it is a server. The engine kills it
after a match. Use Ctrl-C during the standalone checkpoint.

### The engine waits forever before identifying the bot

Check the bot's readiness line:

```text
LISTENING tcp://127.0.0.1:<port>
```

It must be printed after binding, include a newline, and be flushed.

### The match waits forever during `GetOrders`

M2 has no deadline. First inspect the bot for a panic or deadlock. M3 will
bound this failure with a propagated gRPC deadline.

### The match ends at the turn cap without combat

Print the selected orders and `OrderOutcome` values. Check that:

- `choose_order` returns `Some`;
- `move_to` is changing positions;
- the response echoes the request turn;
- `end_turn` is called.

### Child bots remain after the engine exits

Check every path in `main.rs`:

- bot B startup failure cleans up bot A;
- `run_match` result is saved;
- both `shutdown` calls happen before `match_result?`;
- `shutdown` calls both `kill` and `wait`.

## Notes to collect for blog post 1

After each checkpoint, answer one question:

1. After Checkpoint 2: what did the proto generate that you did not write?
2. After Checkpoint 3: what process-lifecycle code did gRPC not provide?
3. After Checkpoint 4: why is the domain model separate from the wire model?
4. After Checkpoint 5: why does the renderer use authoritative state?
5. After Checkpoint 6: where does the code cross the process boundary?
6. After Checkpoint 7: which failures are now possible that a local
   function call did not have?

A useful post-1 argument is:

> gRPC did not make the remote call local. It generated a shared contract and
> the mechanics of calling it. The application still owns state projection,
> validation, process lifetime, and failure policy.

## M2 completion checklist

- [ ] `cargo test -p ni-game` passes before M2 work.
- [ ] `ni-bot` binds an ephemeral loopback TCP port.
- [ ] `Identify` works through `grpcurl` or the engine.
- [ ] `NewMatch` records the assigned chapter.
- [ ] Unknown matches return `SHRUBBERY_REQUIRED`.
- [ ] The bot strategy is deterministic and uses only proto data.
- [ ] The engine starts and identifies two real child processes.
- [ ] Domain/wire conversions have unit tests.
- [ ] The renderer has unit tests.
- [ ] Every returned order passes through `apply_orders`.
- [ ] The loop checks elimination before advancing the turn.
- [ ] The loop checks the cap after advancing the turn.
- [ ] Both bots receive `MatchEnded`.
- [ ] The result is printed.
- [ ] Both child processes are killed and reaped.
- [ ] `cargo fmt`, `cargo test --workspace`, and Clippy pass.
- [ ] Two complete runs produce the same result.

Once those statements are true, M2 is complete. The next useful experiment
is not more M2 polish: it is to make one bot slow and begin M3.
