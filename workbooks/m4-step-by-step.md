# M4 step by step — spans, trace context, and a dashboard to watch it in

This is the hands-on workbook for milestone **M4**, in the same shape as
[`m2-step-by-step.md`](m2-step-by-step.md) and
[`m3-step-by-step.md`](m3-step-by-step.md): which file to open, what to put in
it, what the Rust means, why the design is the way it is, and what command to
run before continuing.

M3 made the engine survive every way a bot can fail. It reported those
failures as lines of text on stdout, one process at a time, with no way to
line up *the engine's view of a call* against *the bot's view of the same
call*. M4 fixes that in the way blog post 3 needs: one span per gRPC call, W3C
trace context on the wire so the bot's spans land inside the engine's, a JSONL
match log carrying the trace ids, and — the part that makes all of it worth
doing — a **Grafana/Tempo/Loki stack in one Docker container** so you can look
at a match instead of reading about it.

Every Rust block below was compiled, formatted, clippy-cleaned and run against
the repository's M3 implementation with tonic 0.13, opentelemetry 0.32 and
tracing-opentelemetry 0.33. Terminal transcripts are real output. The Grafana
walkthrough describes the UI rather than pasting screenshots; the trace trees
quoted are decoded straight off the OTLP wire by the 100-line receiver in
[Appendix A](#appendix-a--no-docker-a-100-line-otlp-receiver), which is also
how the "spans per service" counts were produced.

## How to use this workbook

Work through the checkpoints in order. At each checkpoint:

1. edit only the files named in that checkpoint;
2. run `cargo fmt`;
3. run the checkpoint command;
4. fix errors before moving on;
5. make a short note about what you observed.

Run every command from the repository root:

```sh
cd /home/gstewart/projects/ni
```

Checkpoint 5 needs Docker (`docker compose`). If you would rather not run a
container, Appendix A gets you the same information in a terminal.

## What M3 leaves invisible

Read this list before writing code; each line is something you cannot
currently see.

- **A slow bot is a number, not a shape.** `GetOrders -> chapter B failed with
  Cancelled in 100ms` tells you the engine gave up. It does not tell you
  whether the bot was still thinking, had already answered, or never woke up.
- **Two processes, two unrelated logs.** The engine says the call timed out at
  100ms. Roger says he slept 400ms. Nothing connects those two statements — no
  shared id, no shared clock, nothing to join on.
- **Timings are measured and thrown away.** `call_get_orders` starts an
  `Instant`, prints the elapsed milliseconds, and drops it. There is no record
  of a match to compare against another match.
- **`SubmitReplay` is a stub on both sides.** The `Replay` message has existed
  since M0 and nothing has ever built one.
- **Diagnostics fight with the UI.** Both the board and the engine's
  commentary go to stdout, so `--quiet` is the only volume control there is.

## What M4 adds

- A **span per gRPC call** in the engine, tagged with the OpenTelemetry RPC
  conventions, plus a span per turn and one per match.
- **W3C trace context propagation** engine → bot: `traceparent` in the gRPC
  metadata, extracted on the bot side, so a bot's handler span is a *child* of
  the engine's call span across a process boundary.
- **`tracing` events instead of `println!`** for everything that is a
  diagnostic, on **stderr**, each one automatically stamped with the trace and
  span id of the span it happened in.
- **A JSONL match log**: one object per turn with latency, status, strikes,
  order outcomes and the trace id — greppable, `jq`-able, and a bridge into
  the dashboard.
- **`SubmitReplay` wired end to end**, built from the same records the JSONL
  file gets, with its encoded size recorded on the span (post 6's payload,
  measured for free).
- **A telemetry stack in Docker** — Grafana, Tempo, Loki, Prometheus and an
  OTel collector in one container — and the `OTEL_EXPORTER_OTLP_ENDPOINT`
  wiring to point Ni at it.
- **Graceful bot shutdown**, because a process that is `SIGKILL`ed never gets
  to send its spans. This one is not optional; see Checkpoint 6.

## What M4 deliberately leaves alone

- **The proto does not change.** Not one line. Trace context travels in gRPC
  *metadata*, which is exactly what metadata is for, and `Replay`/`TurnRecord`
  were designed in M0. `buf breaking` stays quiet, which is the point.
- **Unix domain sockets** stay unwritten — still loopback TCP (M5).
- **Metrics.** The stack collects them and Ni exports none. A match is a trace
  and a log; counters and histograms come with the measurement harness in M5,
  where there is something worth counting.
- **Sampling.** Everything is sampled, always. A tournament is where
  head-based sampling starts to matter.
- **`ni-game`** has no new code in it. Again.

## Design decisions this milestone settles

| Question | M4's answer | Why |
|---|---|---|
| Where does trace context live? | gRPC metadata (`traceparent`), never in a proto message | Context is per-call transport plumbing, not part of the contract. Putting it in the proto would make every bot author implement it |
| One trace per match, or per turn? | Per **match**: `match` → `turn` → `grpc` → the bot's `rpc` span | A match is the unit you debug. Three spans per turn over ~34 turns is a ~120-span trace, which every backend handles comfortably |
| Who creates the bot's span? | The bot, from the metadata the engine sent | The engine cannot know how long the bot's own work took, and a bot that ignores the header still works — it just becomes a root span |
| stdout or stderr for logs? | **stderr**, always | A bot's stdout carries the `LISTENING` readiness line the engine parses; the engine's stdout carries the board. Telemetry on stdout would corrupt both |
| What if no collector is running? | Everything still works: stderr logs, no exporters, no trace ids in the log | Telemetry must never be a requirement for playing a match |
| Can telemetry fail a match? | No. Log write errors warn once and disable the writer | The log is evidence, not gameplay |
| How do bots get shut down? | `SIGTERM`, 2s grace, then `SIGKILL` | A batch exporter needs a chance to flush. `SIGKILL` silently loses every span the bot ever recorded |
| Where do the trace ids in the JSONL come from? | The current span, via `ni-telemetry::current_ids()` | It makes the log file and the dashboard two views of one event instead of two sources of truth |

## What you will create

```text
crates/ni-telemetry/            NEW CRATE
  Cargo.toml
  src/lib.rs             subscriber, OTLP exporters, traceparent in/out

crates/ni-engine/src/
  log.rs                 NEW  JSONL match log + the replay it accumulates
  match_runner.rs        rewritten  spans, injection, log, replay
  process.rs             edited     SIGTERM before SIGKILL
  main.rs                edited     telemetry init, --match-log, --match-id
  lib.rs                 edited     module list
crates/ni-engine/tests/
  failure_modes.rs       edited     three new tests over the log file

crates/ni-bot/src/
  lib.rs                 edited     a span per handler
  server.rs              edited     graceful shutdown on SIGTERM
  main.rs                edited     telemetry init
crates/roger-the-shrubber/src/
  lib.rs                 edited     spans; eprintln! -> tracing events
  main.rs                edited     telemetry init

deploy/telemetry/
  docker-compose.yml     NEW  Grafana + Tempo + Loki + collector, one container
```

Plus the workspace `Cargo.toml` and three crate manifests.

## A small Rust map for M4

M2's and M3's maps still apply. These are the new shapes.

```rust
async { /* ... */ }.instrument(span).await
```

`Instrument` attaches a span to a *future*. This is the async-correct way:
`span.enter()` returns a guard tied to the current thread, and a future that
`.await`s while holding one can be moved to another thread with the guard
still "entered", which corrupts the span stack. Rule of thumb: `enter()` for
synchronous blocks, `.instrument()` for anything with an `await` in it.

```rust
#[tracing::instrument(name = "match", skip_all, fields(ni.match_id = %options.match_id))]
pub async fn run_match(/* ... */) { }
```

The attribute form does the same thing for a whole function. `skip_all` stops
it recording every argument (a `MatchState` as a span field would be absurd);
`fields(...)` adds back the ones you want. `%` means "record via `Display`",
`?` means "via `Debug`".

```rust
let span = info_span!("grpc", rpc.grpc.status_code = field::Empty);
span.record("rpc.grpc.status_code", 4);
```

A span's *field set* is fixed when it is created. Any field you intend to fill
in later must be declared as `field::Empty` up front; `record` on an
undeclared field is silently dropped. This is the single most common way to
lose an attribute.

```rust
let Self { match_id, writer, turns, .. } = self;
```

Destructuring `self` gives you *disjoint* borrows of individual fields, so you
can pass `&mut writer` to something while `&match_id` is still borrowed. Doing
the same thing through `self.writer` and `self.match_id` in one expression
does not compile.

```rust
#[derive(Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum Entry<'a> { MatchStarted { /* ... */ }, Turn { /* ... */ } }
```

An *internally tagged* enum: serde writes `"kind": "match_started"` alongside
the variant's own fields, so one JSONL file can hold several line shapes and
`jq 'select(.kind == "turn")'` sorts them out.

```rust
trait Injector { fn set(&mut self, key: &str, value: String); }
trait Extractor { fn get(&self, key: &str) -> Option<&str>; fn keys(&self) -> Vec<&str>; }
```

OpenTelemetry's two-trait carrier abstraction. Implementing them for tonic's
`MetadataMap` is the whole of "putting trace context on the wire" — the
propagator decides *what* to write, these decide *where*.

```rust
unsafe { libc::kill(pid as libc::pid_t, libc::SIGTERM); }
```

`tokio::process::Child` can only `SIGKILL`. Anything politer goes through
`libc`, and `libc` is `unsafe` by definition; the safety argument is that the
pid belongs to a child we spawned and have not reaped, so it cannot have been
recycled onto someone else's process.

```rust
#[must_use = "hold the guard until the process is finished, then shut it down"]
pub struct Telemetry { /* ... */ }
```

`#[must_use]` on a type makes the compiler complain when a value of it is
dropped on the floor. For an exporter guard that is exactly right: dropping it
early throws away whatever is still queued.


---

## Checkpoint 0 — prove M3 is healthy

```sh
cargo test --workspace
cargo build --workspace
./target/debug/ni-engine run \
  --bot-a target/debug/ni-bot \
  --bot-b target/debug/roger-the-shrubber \
  --bot-b-arg --sleep-ms --bot-b-arg 400 \
  --turn-deadline 100ms
```

Expected: tests pass, and the match ends with chapter B forfeiting on
timeouts.

Now read the forfeit line and try to answer these three questions from it:

- Did Roger ever produce orders for the turn the engine gave up on?
- How much of the 100ms was the engine's own work rather than Roger's?
- Which turn was slow — and was it the same turn in the previous run?

You cannot answer any of them. That is M4's reason to exist.


---

## Checkpoint 1 — the telemetry crate

Everything about *context* — building a subscriber, exporting, putting
`traceparent` on the wire and taking it off again — is the same in the engine
and in every bot. It goes in one crate that knows nothing about the game.

### 1.1 Add the dependencies to the workspace `Cargo.toml`

```toml
[workspace]
resolver = "2"
members = [
    "crates/ni-proto",
    "crates/ni-game",
    "crates/ni-engine",
    "crates/ni-bot",
    "crates/ni-telemetry",
    "crates/roger-the-shrubber",
]
```

Then, in `[workspace.dependencies]`, add `ni-telemetry` next to the other
path dependencies, `libc`, and the telemetry stack; and add `"signal"` to
tokio's feature list:

```toml
ni-telemetry = { path = "crates/ni-telemetry" }
anyhow = "1"
libc = "0.2"
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
    "signal",
    "sync",
] }
tokio-stream = { version = "0.1", features = ["net"] }
tonic-build = "0.13"
protoc-bin-vendored = "3"
serde = { version = "1", features = ["derive"] }
serde_json = "1"
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter"] }
tracing-opentelemetry = "0.33"
opentelemetry = "0.32"
opentelemetry_sdk = "0.32"
opentelemetry-otlp = "0.32"
opentelemetry-appender-tracing = "0.32"
```

Six crates where you might have expected one, so: `tracing` is the *facade*
you write spans and events against; `tracing-subscriber` is the machinery that
collects them (the `EnvFilter`, the stderr formatter);
`tracing-opentelemetry` is the bridge that turns `tracing` spans into
OpenTelemetry spans; `opentelemetry` is the API (contexts, propagators);
`opentelemetry_sdk` is the implementation (providers, batch processors,
resources); `opentelemetry-otlp` is the exporter that speaks OTLP to a
collector; `opentelemetry-appender-tracing` does for *events* what
`tracing-opentelemetry` does for spans, so your log lines arrive with trace
ids attached.

The versions matter and they move together: `tracing-opentelemetry` 0.33 pairs
with `opentelemetry` 0.32. Mixing 0.32 and 0.31 across these crates produces
trait-mismatch errors that read as if you had written the wrong code.

`opentelemetry-otlp`'s default features are `http-proto` +
`reqwest-blocking-client`, which is what you want here: the batch exporter
runs on its own thread and a blocking HTTP client on that thread needs no
runtime of its own. The `grpc-tonic` feature works too, at the price of a
second tonic version in your dependency tree.

### 1.2 Create `crates/ni-telemetry/Cargo.toml`

```toml
[package]
name = "ni-telemetry"
version.workspace = true
edition.workspace = true
repository.workspace = true
description = "Shared tracing/OTLP wiring and W3C trace-context propagation for Ni"

[dependencies]
opentelemetry = { workspace = true }
opentelemetry-appender-tracing = { workspace = true }
opentelemetry-otlp = { workspace = true }
opentelemetry_sdk = { workspace = true }
tonic = { workspace = true }
tracing = { workspace = true }
tracing-opentelemetry = { workspace = true }
tracing-subscriber = { workspace = true }
```

### 1.3 Create `crates/ni-telemetry/src/lib.rs`

```rust
//! M4 telemetry wiring, shared by the engine and every bot.
//!
//! Three jobs, and nothing else:
//!
//! 1. **Build a subscriber.** Human-readable logs on *stderr* always; OTLP
//!    traces and logs as well when `OTEL_EXPORTER_OTLP_ENDPOINT` is set.
//! 2. **Put the current trace on the wire** ([`inject_context`]), so a bot's
//!    spans become children of the engine's.
//! 3. **Take it off again** ([`server_span`]), so the bot's span knows which
//!    call it belongs to.
//!
//! Everything here is about *context*, not about the game. No game types
//! appear in this crate, and no telemetry types leak into `ni-game`.

use std::time::Duration;

use opentelemetry::{
    global,
    propagation::{Extractor, Injector},
    trace::TracerProvider as _,
    Context,
};
use opentelemetry_appender_tracing::layer::OpenTelemetryTracingBridge;
use opentelemetry_otlp::WithExportConfig;
use opentelemetry_sdk::{
    logs::SdkLoggerProvider, propagation::TraceContextPropagator, trace::SdkTracerProvider,
    Resource,
};
use tonic::{metadata::MetadataMap, Request};
use tracing::Span;
use tracing_opentelemetry::OpenTelemetrySpanExt;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt, EnvFilter};

/// Standard OTLP environment variable. Unset means "no collector": the
/// process still logs to stderr, and every span still exists in-process.
const ENDPOINT_ENV: &str = "OTEL_EXPORTER_OTLP_ENDPOINT";

/// Providers kept alive for the life of the process. Dropping them without
/// [`Telemetry::shutdown`] loses whatever is still in the batch queue.
#[must_use = "hold the guard until the process is finished, then shut it down"]
pub struct Telemetry {
    tracer_provider: Option<SdkTracerProvider>,
    logger_provider: Option<SdkLoggerProvider>,
}

impl Telemetry {
    /// Flush and stop the exporters. Call this on the way out; a batch
    /// exporter that is dropped mid-batch simply drops the spans.
    pub fn shutdown(self) {
        if let Some(provider) = self.tracer_provider {
            if let Err(error) = provider.shutdown() {
                eprintln!("telemetry: tracer shutdown failed: {error}");
            }
        }

        if let Some(provider) = self.logger_provider {
            if let Err(error) = provider.shutdown() {
                eprintln!("telemetry: logger shutdown failed: {error}");
            }
        }
    }
}

/// Install the subscriber for this process.
///
/// `service_name` is what the dashboard groups spans under, so it must be
/// per-binary: `ni-engine`, `reference-bot`, `roger-the-shrubber`.
pub fn init(service_name: &str) -> Telemetry {
    // Trace context travels as the W3C `traceparent` header. Registering the
    // propagator globally is what makes `inject_context` and `server_span`
    // agree on a format without either of them naming one.
    global::set_text_map_propagator(TraceContextPropagator::new());

    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));

    // stderr, never stdout: a bot's stdout carries the readiness line the
    // engine parses, and the engine's stdout carries the board.
    let stderr_layer = tracing_subscriber::fmt::layer()
        .with_writer(std::io::stderr)
        .with_target(false);

    let endpoint = match std::env::var(ENDPOINT_ENV) {
        Ok(endpoint) if !endpoint.trim().is_empty() => {
            endpoint.trim().trim_end_matches('/').to_string()
        }
        _ => {
            tracing_subscriber::registry()
                .with(filter)
                .with(stderr_layer)
                .init();

            return Telemetry {
                tracer_provider: None,
                logger_provider: None,
            };
        }
    };

    let resource = Resource::builder()
        .with_service_name(service_name.to_string())
        .build();

    let span_exporter = opentelemetry_otlp::SpanExporter::builder()
        .with_http()
        .with_endpoint(format!("{endpoint}/v1/traces"))
        .with_timeout(Duration::from_secs(3))
        .build()
        .expect("OTLP span exporter");

    let tracer_provider = SdkTracerProvider::builder()
        .with_resource(resource.clone())
        .with_batch_exporter(span_exporter)
        .build();

    let log_exporter = opentelemetry_otlp::LogExporter::builder()
        .with_http()
        .with_endpoint(format!("{endpoint}/v1/logs"))
        .with_timeout(Duration::from_secs(3))
        .build()
        .expect("OTLP log exporter");

    let logger_provider = SdkLoggerProvider::builder()
        .with_resource(resource)
        .with_batch_exporter(log_exporter)
        .build();

    let tracer = tracer_provider.tracer("ni");
    global::set_tracer_provider(tracer_provider.clone());

    tracing_subscriber::registry()
        .with(filter)
        .with(stderr_layer)
        // Spans -> Tempo.
        .with(tracing_opentelemetry::layer().with_tracer(tracer))
        // Events -> Loki, each stamped with the trace id of its span.
        .with(OpenTelemetryTracingBridge::new(&logger_provider))
        .init();

    tracing::info!(
        service = service_name,
        endpoint,
        "telemetry exporting over OTLP"
    );

    Telemetry {
        tracer_provider: Some(tracer_provider),
        logger_provider: Some(logger_provider),
    }
}

/// Write the current span's context into a request's gRPC metadata as
/// `traceparent`. Call it *inside* the span that represents the call.
pub fn inject_context<T>(request: &mut Request<T>) {
    let context = Span::current().context();

    global::get_text_map_propagator(|propagator| {
        propagator.inject_context(&context, &mut MetadataInjector(request.metadata_mut()));
    });
}

/// A server-side span for one RPC, parented to the caller's span if the
/// caller sent a `traceparent`.
///
/// `rpc` is the full gRPC method name, e.g. `ni.v1.BotService/GetOrders` —
/// that is what the dashboard shows as the span name.
pub fn server_span(rpc: &'static str, metadata: &MetadataMap) -> Span {
    let span = tracing::info_span!(
        "rpc",
        otel.name = rpc,
        otel.kind = "server",
        rpc.system = "grpc",
    );

    let parent = global::get_text_map_propagator(|propagator| {
        propagator.extract(&MetadataExtractor(metadata))
    });
    // The only error here is "no OpenTelemetry layer installed", i.e. this
    // process is running without a collector. The span is still a perfectly
    // good local span, so there is nothing to report.
    let _ = span.set_parent(parent);

    span
}

/// Trace and span id of the current span, as the 32- and 16-character hex
/// strings a dashboard search box expects. `None` when nothing is sampled —
/// with no collector configured there is no trace to point at.
pub fn current_ids() -> Option<TraceIds> {
    let context = Span::current().context();
    let span = opentelemetry::trace::TraceContextExt::span(&context);
    let span_context = span.span_context();

    span_context.is_valid().then(|| TraceIds {
        trace_id: span_context.trace_id().to_string(),
        span_id: span_context.span_id().to_string(),
    })
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TraceIds {
    pub trace_id: String,
    pub span_id: String,
}

/// `traceparent` out. gRPC metadata keys must be lowercase ASCII, which the
/// W3C names already are.
struct MetadataInjector<'a>(&'a mut MetadataMap);

impl Injector for MetadataInjector<'_> {
    fn set(&mut self, key: &str, value: String) {
        if let Ok(name) = tonic::metadata::MetadataKey::from_bytes(key.as_bytes()) {
            if let Ok(value) = tonic::metadata::MetadataValue::try_from(&value) {
                self.0.insert(name, value);
            }
        }
    }
}

/// `traceparent` in. Anything unparseable is simply absent, which yields a
/// root span rather than an error — telemetry must never fail a match.
struct MetadataExtractor<'a>(&'a MetadataMap);

impl Extractor for MetadataExtractor<'_> {
    fn get(&self, key: &str) -> Option<&str> {
        self.0.get(key).and_then(|value| value.to_str().ok())
    }

    fn keys(&self) -> Vec<&str> {
        self.0
            .keys()
            .filter_map(|key| match key {
                tonic::metadata::KeyRef::Ascii(name) => Some(name.as_str()),
                tonic::metadata::KeyRef::Binary(_) => None,
            })
            .collect()
    }
}

/// Sanity check for the two halves of propagation, with no network in sight.
#[doc(hidden)]
pub fn round_trip_for_tests(traceparent: &str) -> Option<String> {
    let mut metadata = MetadataMap::new();
    metadata.insert("traceparent", traceparent.parse().ok()?);

    let context: Context = global::get_text_map_propagator(|propagator| {
        propagator.extract(&MetadataExtractor(&metadata))
    });

    let span = opentelemetry::trace::TraceContextExt::span(&context);
    let span_context = span.span_context();

    span_context
        .is_valid()
        .then(|| span_context.trace_id().to_string())
}
```

### 1.4 What that file is doing, in order

**The propagator is global, deliberately.** `set_text_map_propagator` installs
one format for the whole process. `inject_context` and `server_span` never
mention W3C; they ask the global propagator to do whatever it does. Swap in a
B3 propagator and both halves change together, which is the argument for the
indirection.

**Two exporters, not one.** Traces and logs are separate OTLP signals with
separate endpoints (`/v1/traces`, `/v1/logs`) and separate providers. What
joins them is neither: `OpenTelemetryTracingBridge` stamps each event with the
trace and span id of the span it was emitted in. That stamp is why "show me
the logs for this span" works at all.

**A `Resource` is what the dashboard groups by.** `service.name` is the one
attribute you cannot skip, and it must differ per binary — three processes
reporting as `ni-engine` produce a trace you cannot read.

**The no-collector path returns early.** If `OTEL_EXPORTER_OTLP_ENDPOINT` is
unset you get a stderr-only subscriber and `None` providers. Every span still
exists locally, `current_ids()` returns `None`, and nothing anywhere has to
check whether telemetry is on.

**`server_span` builds an empty span and then adopts a parent.** `otel.name`,
`otel.kind` and friends are magic field names that `tracing-opentelemetry`
reads: `otel.name` overrides the span name (so it can be a full gRPC method
name rather than a Rust identifier), `otel.kind` sets CLIENT/SERVER, which is
what makes a backend draw the two-process relationship rather than a plain
nesting.

**`set_parent` returns a `Result`.** In tracing-opentelemetry 0.33 the only
failure is "no OpenTelemetry layer is installed" — i.e. you are running
without a collector — so it is deliberately ignored.

### 1.5 Append the tests to `crates/ni-telemetry/src/lib.rs`

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_traceparent_header_survives_extraction() {
        global::set_text_map_propagator(TraceContextPropagator::new());

        let trace_id =
            round_trip_for_tests("00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01");

        assert_eq!(
            trace_id.as_deref(),
            Some("4bf92f3577b34da6a3ce929d0e0e4736")
        );
    }

    #[test]
    fn nonsense_is_no_parent_rather_than_an_error() {
        global::set_text_map_propagator(TraceContextPropagator::new());

        assert_eq!(round_trip_for_tests("not-a-traceparent"), None);
    }

    #[test]
    fn injection_writes_a_traceparent_when_a_span_is_sampled() {
        let mut request = Request::new(());
        inject_context(&mut request);

        // With no exporter installed in a unit test there is no sampled
        // span, so there is deliberately nothing to inject.
        assert!(request.metadata().get("traceparent").is_none());
    }
}
```

The middle test is the important one: a bot that receives a malformed
`traceparent` must produce a root span, not an error. Telemetry that can
reject a call is worse than no telemetry.

The third test looks like it is asserting the wrong thing, and is worth
keeping for exactly that reason: with no OTel layer installed there is no
sampled span, so there is no context and nothing gets injected. If you ever
see a `traceparent` here, something is installing a global subscriber behind
your back.

### 1.6 Run the checkpoint

```sh
cargo fmt
cargo test -p ni-telemetry
```

```text
running 3 tests
test tests::a_traceparent_header_survives_extraction ... ok
test tests::injection_writes_a_traceparent_when_a_span_is_sampled ... ok
test tests::nonsense_is_no_parent_rather_than_an_error ... ok
```


---

## Checkpoint 2 — the match log

Before instrumenting anything, give the engine somewhere to write what it
already measures. This module is pure bookkeeping: no gRPC, no async, and one
`ni_telemetry` call.

### 2.1 Edit `crates/ni-engine/Cargo.toml`

```toml
[dependencies]
ni-proto = { workspace = true }
ni-game = { workspace = true }
ni-telemetry = { workspace = true }
anyhow = { workspace = true }
clap = { workspace = true }
libc = { workspace = true }
prost = { workspace = true }
serde = { workspace = true }
serde_json = { workspace = true }
tonic = { workspace = true }
tokio = { workspace = true }
tracing = { workspace = true }
```

`prost` is here for one line — `replay.encoded_len()` — and it earns its place
by turning "the replay is the big message" into a number.

### 2.2 Create `crates/ni-engine/src/log.rs`

```rust
//! The match log: one JSON object per line, and the replay it accumulates.
//!
//! Two consumers, one source of truth:
//!
//! - a **JSONL file** you can `tail -f`, `jq`, or paste a trace id out of;
//! - a **`ni.v1.Replay`** message, sent to both bots at the end of the match.
//!
//! Nothing in here may fail a match. A log line that cannot be written is a
//! warning and a dropped writer, never a `?` that unwinds the game.

use std::{
    fs::File,
    io::{BufWriter, Write},
    path::Path,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result};
use ni_game::{Chapter, MatchState, OrderOutcome, OrderResult};
use ni_proto::ni::v1::{
    BoardLayout, KnightOrder, OrderOutcome as ProtoOutcome, OrderResult as ProtoResult, Position,
    Replay, Rules as ProtoRules, TurnRecord,
};
use serde::Serialize;

use crate::{
    convert::{board_layout, chapter_to_proto, rules_to_proto},
    policy::{MatchConclusion, TimeControl},
};

/// What the engine observed about one `GetOrders` call, in the terms the log
/// cares about: how long, how it ended, what it changed.
pub struct TurnEvent<'a> {
    pub turn: u32,
    pub acting: Chapter,
    pub latency: Duration,
    /// `ok`, `timeout`, `unreachable` or `protocol`.
    pub status: &'static str,
    pub detail: Option<&'a str>,
    pub outcomes: &'a [OrderOutcome],
    pub strikes: u32,
    /// Number of `GetOrders` calls this turn: 2 after a `SHRUBBERY_REQUIRED`
    /// recovery, 1 otherwise.
    pub attempts: u32,
}

pub struct MatchLog {
    match_id: String,
    board: BoardLayout,
    rules: ProtoRules,
    writer: Option<BufWriter<File>>,
    turns: Vec<TurnRecord>,
}

impl MatchLog {
    /// `path: None` keeps the replay and skips the file — the engine still
    /// records everything, it just has nowhere to put it.
    pub fn create(
        match_id: &str,
        state: &MatchState,
        time: TimeControl,
        path: Option<&Path>,
    ) -> Result<Self> {
        let mut writer = match path {
            Some(path) => Some(BufWriter::new(File::create(path).with_context(|| {
                format!("could not create match log {}", path.display())
            })?)),
            None => None,
        };

        let match_id = match_id.to_string();
        let board = board_layout(state);

        write(
            &mut writer,
            &Entry::MatchStarted {
                ts_ms: now_ms(),
                match_id: &match_id,
                trace: TraceRef::current(),
                turn_deadline_ms: time.turn_deadline_ms(),
                strike_limit: time.strike_limit,
                board_width: board.width,
                board_height: board.height,
            },
        );

        Ok(Self {
            match_id,
            board,
            rules: rules_to_proto(state, time),
            writer,
            turns: Vec::new(),
        })
    }

    pub fn record_turn(&mut self, event: TurnEvent<'_>) {
        // Destructuring `self` gives disjoint borrows of the fields, so the
        // entry can borrow `match_id` while `writer` is borrowed mutably.
        let Self {
            match_id,
            writer,
            turns,
            ..
        } = self;

        write(
            writer,
            &Entry::Turn {
                ts_ms: now_ms(),
                match_id,
                trace: TraceRef::current(),
                turn: event.turn,
                acting: chapter_name(event.acting),
                status: event.status,
                detail: event.detail,
                latency_us: event.latency.as_micros(),
                deadline_exceeded: event.status == "timeout",
                attempts: event.attempts,
                strikes: event.strikes,
                orders: event.outcomes.iter().map(OrderEntry::from).collect(),
            },
        );

        turns.push(TurnRecord {
            turn: event.turn,
            acting: chapter_to_proto(event.acting),
            outcomes: event.outcomes.iter().map(proto_outcome).collect(),
            get_orders_latency_us: u64::try_from(event.latency.as_micros()).unwrap_or(u64::MAX),
            deadline_exceeded: event.status == "timeout",
        });
    }

    pub fn record_result(&mut self, conclusion: MatchConclusion) {
        let Self {
            match_id,
            writer,
            turns,
            ..
        } = self;

        write(
            writer,
            &Entry::MatchEnded {
                ts_ms: now_ms(),
                match_id,
                trace: TraceRef::current(),
                turns: turns.len(),
                winner: conclusion.winner().map(chapter_name),
                reason: conclusion.end_reason(),
            },
        );

        if let Some(writer) = self.writer.as_mut() {
            if let Err(error) = writer.flush() {
                tracing::warn!(%error, "could not flush the match log");
            }
        }
    }

    /// The replay message, built from the same records the JSONL file got.
    pub fn replay(&self, conclusion: MatchConclusion) -> Replay {
        Replay {
            match_id: self.match_id.clone(),
            board: Some(self.board.clone()),
            rules: Some(self.rules),
            turns: self.turns.clone(),
            winner: conclusion
                .winner()
                .map(chapter_to_proto)
                .unwrap_or_default(),
            reason: conclusion.end_reason(),
        }
    }
}

/// A free function rather than a method, so callers can hold a borrow of
/// another field of `MatchLog` while writing.
fn write(writer: &mut Option<BufWriter<File>>, entry: &Entry<'_>) {
    let Some(sink) = writer.as_mut() else {
        return;
    };

    let line = match serde_json::to_string(entry) {
        Ok(line) => line,
        Err(error) => {
            tracing::warn!(%error, "could not serialise a match log entry");
            return;
        }
    };

    // Flushed per line on purpose: `tail -f` during a match is the point.
    if let Err(error) = writeln!(sink, "{line}").and_then(|()| sink.flush()) {
        tracing::warn!(%error, "match log disabled after a write error");
        *writer = None;
    }
}

/// The three line shapes. `tag = "kind"` puts a `"kind"` field in the JSON,
/// so one file can hold all three and `jq 'select(.kind == "turn")'` works.
#[derive(Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum Entry<'a> {
    MatchStarted {
        ts_ms: u128,
        match_id: &'a str,
        #[serde(flatten)]
        trace: TraceRef,
        turn_deadline_ms: u32,
        strike_limit: u32,
        board_width: u32,
        board_height: u32,
    },
    Turn {
        ts_ms: u128,
        match_id: &'a str,
        #[serde(flatten)]
        trace: TraceRef,
        turn: u32,
        acting: &'static str,
        status: &'static str,
        #[serde(skip_serializing_if = "Option::is_none")]
        detail: Option<&'a str>,
        latency_us: u128,
        deadline_exceeded: bool,
        attempts: u32,
        strikes: u32,
        orders: Vec<OrderEntry>,
    },
    MatchEnded {
        ts_ms: u128,
        match_id: &'a str,
        #[serde(flatten)]
        trace: TraceRef,
        turns: usize,
        #[serde(skip_serializing_if = "Option::is_none")]
        winner: Option<&'static str>,
        reason: i32,
    },
}

/// The bridge between the two halves of M4: every log line carries the ids
/// of the span it happened in, so a line in the file and a span in the
/// dashboard are the same event seen twice.
#[derive(Default, Serialize)]
struct TraceRef {
    #[serde(skip_serializing_if = "Option::is_none")]
    trace_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    span_id: Option<String>,
}

impl TraceRef {
    fn current() -> Self {
        match ni_telemetry::current_ids() {
            Some(ids) => Self {
                trace_id: Some(ids.trace_id),
                span_id: Some(ids.span_id),
            },
            None => Self::default(),
        }
    }
}

#[derive(Serialize)]
struct OrderEntry {
    unit_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    move_to: Option<[u32; 2]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    attack_target: Option<String>,
    result: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    damage: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    reason: Option<String>,
}

impl From<&OrderOutcome> for OrderEntry {
    fn from(outcome: &OrderOutcome) -> Self {
        let (result, damage, reason) = match &outcome.result {
            OrderResult::Applied { damage_dealt } => ("applied", Some(*damage_dealt), None),
            OrderResult::Illegal { reason } => ("illegal", None, Some(format!("{reason:?}"))),
            OrderResult::NotReached => ("not_reached", None, None),
        };

        Self {
            unit_id: outcome.order.unit_id.clone(),
            move_to: outcome
                .order
                .move_to
                .map(|position| [position.x, position.y]),
            attack_target: outcome.order.attack_target.clone(),
            result,
            damage,
            reason,
        }
    }
}

fn proto_outcome(outcome: &OrderOutcome) -> ProtoOutcome {
    let (result, detail, damage_dealt) = match &outcome.result {
        OrderResult::Applied { damage_dealt } => {
            (ProtoResult::Applied, String::new(), *damage_dealt)
        }
        OrderResult::Illegal { reason } => (ProtoResult::Illegal, format!("{reason:?}"), 0),
        OrderResult::NotReached => (ProtoResult::NotReached, String::new(), 0),
    };

    ProtoOutcome {
        order: Some(KnightOrder {
            unit_id: outcome.order.unit_id.clone(),
            move_to: outcome.order.move_to.map(|position| Position {
                x: position.x,
                y: position.y,
            }),
            attack_target: outcome.order.attack_target.clone(),
        }),
        result: result as i32,
        detail,
        damage_dealt,
    }
}

fn chapter_name(chapter: Chapter) -> &'static str {
    match chapter {
        Chapter::A => "A",
        Chapter::B => "B",
    }
}

fn now_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|since| since.as_millis())
        .unwrap_or_default()
}
```

### 2.3 What that file is doing

**One recording, two outputs.** `record_turn` writes a JSON line *and* pushes
a `TurnRecord` onto the replay. If those were two code paths they would drift;
a file that disagrees with the replay you sent the bots is worse than either
alone.

**`SystemTime` here, `Instant` there.** The log needs wall-clock timestamps
you can line up against a dashboard, so `ts_ms` comes from `SystemTime`.
Durations still come from the `Instant` in `match_runner` — wall clocks can
jump, monotonic clocks cannot.

**Two kinds of failure, both non-fatal.** A serialisation error warns and
skips one line (the next one may be fine). An I/O error warns and sets
`*writer = None`, because a full disk will not un-fill itself and 100 warnings
about it are noise.

**`chapter_name` appears here and in `match_runner`.** Two three-line
functions in two modules, rather than a shared helper that couples the log's
JSON spelling to the renderer's display spelling. They are allowed to diverge;
that is the point.

### 2.4 Append the tests to `log.rs`

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use ni_game::{Order, Position as GamePosition, Rules};

    fn applied(unit: &str, damage: u32) -> OrderOutcome {
        OrderOutcome {
            order: Order {
                unit_id: unit.to_string(),
                move_to: Some(GamePosition { x: 3, y: 4 }),
                attack_target: Some("B1".to_string()),
            },
            result: OrderResult::Applied {
                damage_dealt: damage,
            },
        }
    }

    fn log_in(directory: &Path) -> (MatchLog, std::path::PathBuf) {
        let path = directory.join("match.jsonl");
        let state = ni_game::standard_match(Rules::standard());
        let log =
            MatchLog::create("m4-test", &state, TimeControl::standard(), Some(&path)).unwrap();
        (log, path)
    }

    #[test]
    fn every_turn_is_one_line_of_json() {
        let directory = std::env::temp_dir().join("ni-m4-log-lines");
        std::fs::create_dir_all(&directory).unwrap();
        let (mut log, path) = log_in(&directory);

        log.record_turn(TurnEvent {
            turn: 1,
            acting: Chapter::A,
            latency: Duration::from_micros(1500),
            status: "ok",
            detail: None,
            outcomes: &[applied("A1", 4)],
            strikes: 0,
            attempts: 1,
        });
        log.record_turn(TurnEvent {
            turn: 2,
            acting: Chapter::B,
            latency: Duration::from_millis(100),
            status: "timeout",
            detail: Some("no answer within 100ms"),
            outcomes: &[],
            strikes: 1,
            attempts: 1,
        });
        log.record_result(MatchConclusion::Forfeit {
            loser: Chapter::B,
            reason: crate::policy::ForfeitReason::Timeout,
        });

        let text = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<_> = text.lines().collect();
        assert_eq!(lines.len(), 4, "started + two turns + ended: {text}");

        let turn: serde_json::Value = serde_json::from_str(lines[1]).unwrap();
        assert_eq!(turn["kind"], "turn");
        assert_eq!(turn["latency_us"], 1500);
        assert_eq!(turn["orders"][0]["result"], "applied");
        assert_eq!(turn["orders"][0]["damage"], 4);
        assert_eq!(turn["orders"][0]["move_to"], serde_json::json!([3, 4]));

        let forfeit: serde_json::Value = serde_json::from_str(lines[2]).unwrap();
        assert_eq!(forfeit["status"], "timeout");
        assert_eq!(forfeit["deadline_exceeded"], true);
        assert_eq!(forfeit["strikes"], 1);
        assert_eq!(forfeit["orders"], serde_json::json!([]));

        let ended: serde_json::Value = serde_json::from_str(lines[3]).unwrap();
        assert_eq!(ended["kind"], "match_ended");
        assert_eq!(ended["turns"], 2);
        assert_eq!(ended["winner"], "A");
    }

    #[test]
    fn the_replay_carries_the_same_turns_as_the_file() {
        let directory = std::env::temp_dir().join("ni-m4-log-replay");
        std::fs::create_dir_all(&directory).unwrap();
        let (mut log, _) = log_in(&directory);

        log.record_turn(TurnEvent {
            turn: 1,
            acting: Chapter::A,
            latency: Duration::from_micros(900),
            status: "ok",
            detail: None,
            outcomes: &[applied("A1", 4)],
            strikes: 0,
            attempts: 1,
        });

        let replay = log.replay(MatchConclusion::Decided(ni_game::MatchStatus::Winner {
            chapter: Chapter::A,
            reason: ni_game::EndReason::Elimination,
        }));

        assert_eq!(replay.match_id, "m4-test");
        assert_eq!(replay.turns.len(), 1);
        assert_eq!(replay.turns[0].get_orders_latency_us, 900);
        assert_eq!(replay.turns[0].outcomes[0].damage_dealt, 4);
        assert_eq!(replay.board.unwrap().width, 10);
        assert!(replay.rules.unwrap().turn_deadline_ms > 0);
    }

    #[test]
    fn no_path_means_no_file_but_still_a_replay() {
        let state = ni_game::standard_match(Rules::standard());
        let mut log = MatchLog::create("m4-quiet", &state, TimeControl::standard(), None).unwrap();

        log.record_turn(TurnEvent {
            turn: 1,
            acting: Chapter::A,
            latency: Duration::from_micros(10),
            status: "ok",
            detail: None,
            outcomes: &[],
            strikes: 0,
            attempts: 1,
        });

        assert_eq!(
            log.replay(MatchConclusion::Decided(ni_game::MatchStatus::Draw {
                reason: ni_game::EndReason::TurnCap,
            }))
            .turns
            .len(),
            1
        );
    }
}
```

### 2.5 Register the module

`crates/ni-engine/src/lib.rs`:

```rust
//! M4 match orchestration around the pure `ni-game` rules engine:
//! deadlines and failure policy from M3, plus a span per call, trace context
//! on the wire, a JSONL match log and the replay built from it.

pub mod convert;
pub mod log;
pub mod match_runner;
pub mod policy;
pub mod process;
pub mod render;

pub use log::MatchLog;
pub use match_runner::{run_match, RunOptions};
pub use policy::{ForfeitReason, MatchConclusion, TimeControl};
pub use process::BotProcess;
```

### 2.6 Run the checkpoint

```sh
cargo fmt
cargo test -p ni-engine --lib
```

Expected: 18 tests pass — M3's 15 plus the three new `log::tests`.


---

## Checkpoint 3 — instrument the engine

Now the match loop. Three new things happen in it: every call gets a span,
every span gets `traceparent` injected into its request, and every turn gets
recorded.

### 3.1 Replace `crates/ni-engine/src/match_runner.rs`

The whole file, since almost every function grew a span:

```rust
// The authoritative match loop: M3's policy, M4's instrumentation.

use std::{
    path::PathBuf,
    time::{Duration, Instant},
};

use crate::{
    convert::{battlefield_view, new_match_request, order_from_proto, resume_match_request},
    log::{MatchLog, TurnEvent},
    policy::{
        classify, describe, CallFailure, ForfeitReason, MatchConclusion, Strikes, TimeControl,
    },
    process::BotProcess,
    render::{render_board, render_forfeited_turn, render_result, render_turn},
};
use anyhow::{ensure, Result};
use ni_game::{Chapter, MatchState, MatchStatus, Order, Rules};
use ni_proto::{
    ni::v1::{
        GetOrdersRequest, GetOrdersResponse, IdentifyRequest, MatchEndedRequest, Replay,
        SubmitReplayRequest,
    },
    PROTOCOL_VERSION,
};
use prost::Message as _;
use tonic::{Request, Status};
use tracing::{field, info, info_span, warn, Instrument, Span};

const SETUP_DEADLINE: Duration = Duration::from_secs(5);
/// Deliberately generous: the replay is the largest message in the system,
/// and post 6 wants to measure it rather than time it out.
const REPLAY_DEADLINE: Duration = Duration::from_secs(10);
const SERVICE: &str = "ni.v1.BotService";

pub struct RunOptions {
    pub delay: Duration,
    pub quiet: bool,
    pub match_id: String,
    pub time: TimeControl,
    /// `None` disables the JSONL file; the replay is built either way.
    pub match_log: Option<PathBuf>,
}

enum TurnOutcome {
    Orders(Vec<Order>),
    TurnForfeited {
        reason: ForfeitReason,
        detail: String,
    },
    MatchForfeited {
        reason: ForfeitReason,
        detail: String,
    },
}

/// What one turn's worth of asking cost, whatever the answer was.
struct CallReport {
    outcome: TurnOutcome,
    /// Wall-clock duration of the attempt that produced `outcome`.
    latency: Duration,
    /// 2 after a `SHRUBBERY_REQUIRED` recovery, 1 otherwise.
    attempts: u32,
}

/// One span per outgoing gRPC call, named and tagged the way a tracing
/// backend expects. `otel.*` fields are read by `tracing-opentelemetry`;
/// `rpc.*` are the OpenTelemetry semantic conventions for RPC clients.
///
/// Every field a caller might `record` later must be declared here as
/// `field::Empty`: a `tracing` span's field *set* is fixed at creation, and
/// recording an undeclared field is silently dropped.
fn client_span(method: &'static str) -> Span {
    let span = info_span!(
        "grpc",
        otel.name = field::Empty,
        otel.kind = "client",
        otel.status_code = field::Empty,
        rpc.system = "grpc",
        rpc.service = SERVICE,
        rpc.method = method,
        rpc.grpc.status_code = field::Empty,
        ni.turn = field::Empty,
        ni.latency_us = field::Empty,
        ni.replay_bytes = field::Empty,
    );

    span.record("otel.name", format!("{SERVICE}/{method}").as_str());
    span
}

/// Record how a call ended on its own span, so a failure is visible in the
/// dashboard without reading any logs.
fn record_status(span: &Span, result: &Result<impl Sized, Status>) {
    match result {
        Ok(_) => {
            span.record("rpc.grpc.status_code", tonic::Code::Ok as i32);
            span.record("otel.status_code", "OK");
        }
        Err(status) => {
            span.record("rpc.grpc.status_code", status.code() as i32);
            span.record("otel.status_code", "ERROR");
        }
    }
}

async fn new_match(
    bot: &mut BotProcess,
    state: &MatchState,
    chapter: Chapter,
    options: &RunOptions,
) -> Result<()> {
    let span = client_span("NewMatch");

    async {
        let mut request = Request::new(new_match_request(
            state,
            &options.match_id,
            chapter,
            options.time,
        ));
        request.set_timeout(SETUP_DEADLINE);
        ni_telemetry::inject_context(&mut request);

        let result = bot.client.new_match(request).await;
        record_status(&Span::current(), &result);
        result?;

        info!(chapter = ?chapter, "match announced");
        Ok(())
    }
    .instrument(span)
    .await
}

#[tracing::instrument(
    name = "match",
    skip_all,
    fields(otel.name = "match", ni.match_id = %options.match_id)
)]
pub async fn run_match(
    bot_a: &mut BotProcess,
    bot_b: &mut BotProcess,
    options: RunOptions,
) -> Result<MatchConclusion> {
    identify(bot_a, "A").await?;
    identify(bot_b, "B").await?;

    let mut state = ni_game::standard_match(Rules::standard());

    let mut log = MatchLog::create(
        &options.match_id,
        &state,
        options.time,
        options.match_log.as_deref(),
    )?;

    new_match(bot_a, &state, Chapter::A, &options).await?;
    new_match(bot_b, &state, Chapter::B, &options).await?;

    info!(
        match_id = %options.match_id,
        turn_deadline_ms = options.time.turn_deadline_ms(),
        strike_limit = options.time.strike_limit,
        "match begins"
    );

    if !options.quiet {
        print!("{}", render_board(&state));
    }

    let mut strikes_a = Strikes::default();
    let mut strikes_b = Strikes::default();

    let conclusion = loop {
        let acting = state.to_act;
        let turn = state.turn;

        let (bot, strikes) = match acting {
            Chapter::A => (&mut *bot_a, &mut strikes_a),
            Chapter::B => (&mut *bot_b, &mut strikes_b),
        };

        // One span per turn, so a whole turn — including a recovery and its
        // retry — is a single subtree in the trace.
        let turn_span = info_span!(
            "turn",
            otel.name = "turn",
            ni.turn = turn,
            ni.chapter = chapter_name(acting),
        );

        let report = request_orders(bot, &state, acting, &options)
            .instrument(turn_span.clone())
            .await;

        let CallReport {
            outcome,
            latency,
            attempts,
        } = report;

        match outcome {
            TurnOutcome::Orders(orders) => {
                strikes.clear();

                let (next_state, outcomes) = ni_game::apply_orders(state, acting, &orders);
                state = next_state;

                turn_span.in_scope(|| {
                    info!(
                        turn,
                        chapter = chapter_name(acting),
                        orders = outcomes.len(),
                        latency_us = latency.as_micros(),
                        "turn resolved"
                    );

                    log.record_turn(TurnEvent {
                        turn,
                        acting,
                        latency,
                        status: "ok",
                        detail: None,
                        outcomes: &outcomes,
                        strikes: 0,
                        attempts,
                    });
                });

                if !options.quiet {
                    print!("{}", render_turn(&state, acting, turn, &outcomes));
                }
            }
            TurnOutcome::TurnForfeited { reason, detail } => {
                strikes.record(reason);
                let strikes = *strikes;

                turn_span.in_scope(|| {
                    warn!(
                        turn,
                        chapter = chapter_name(acting),
                        reason = ?reason,
                        detail = %detail,
                        strike = strikes.count(),
                        limit = options.time.strike_limit,
                        "turn forfeited"
                    );

                    log.record_turn(TurnEvent {
                        turn,
                        acting,
                        latency,
                        status: log_status(reason),
                        detail: Some(&detail),
                        outcomes: &[],
                        strikes: strikes.count(),
                        attempts,
                    });
                });

                if !options.quiet {
                    print!(
                        "{}",
                        render_forfeited_turn(
                            turn,
                            acting,
                            reason,
                            &detail,
                            strikes,
                            options.time.strike_limit
                        )
                    );
                }

                if let Some(reason) = strikes.exhausted(options.time.strike_limit) {
                    break MatchConclusion::Forfeit {
                        loser: acting,
                        reason,
                    };
                }
            }
            TurnOutcome::MatchForfeited { reason, detail } => {
                turn_span.in_scope(|| {
                    warn!(
                        turn,
                        chapter = chapter_name(acting),
                        reason = ?reason,
                        detail = %detail,
                        "chapter cannot continue"
                    );

                    log.record_turn(TurnEvent {
                        turn,
                        acting,
                        latency,
                        status: log_status(reason),
                        detail: Some(&detail),
                        outcomes: &[],
                        strikes: strikes.count(),
                        attempts,
                    });
                });

                break MatchConclusion::Forfeit {
                    loser: acting,
                    reason,
                };
            }
        }

        match ni_game::match_status(&state) {
            MatchStatus::InProgress => {}
            finished => break MatchConclusion::Decided(finished),
        }

        ni_game::end_turn(&mut state);

        match ni_game::match_status(&state) {
            MatchStatus::InProgress => {}
            finished => break MatchConclusion::Decided(finished),
        }

        if !options.delay.is_zero() {
            tokio::time::sleep(options.delay).await;
        }
    };

    log.record_result(conclusion);

    info!(
        winner = ?conclusion.winner(),
        turns = state.turn,
        "match ended"
    );

    notify_match_ended(bot_a, &options.match_id, Chapter::A, conclusion).await;
    notify_match_ended(bot_b, &options.match_id, Chapter::B, conclusion).await;

    let replay = log.replay(conclusion);
    submit_replay(bot_a, Chapter::A, &replay).await;
    submit_replay(bot_b, Chapter::B, &replay).await;

    println!("{}", render_result(conclusion));
    Ok(conclusion)
}

async fn request_orders(
    bot: &mut BotProcess,
    state: &MatchState,
    acting: Chapter,
    options: &RunOptions,
) -> CallReport {
    let (result, latency) = call_get_orders(bot, state, acting, options).await;

    let status = match result {
        Ok(response) => {
            return CallReport {
                outcome: orders_or_protocol_error(response, state.turn),
                latency,
                attempts: 1,
            }
        }
        Err(status) => status,
    };

    let outcome = match classify(&status) {
        CallFailure::Timeout => TurnOutcome::TurnForfeited {
            reason: ForfeitReason::Timeout,
            detail: format!(
                "no answer within {}ms ({})",
                options.time.turn_deadline_ms(),
                describe(&status)
            ),
        },
        CallFailure::NeedsShrubbery => {
            return recover_and_retry(bot, state, acting, options).await;
        }
        CallFailure::Unreachable(detail) => TurnOutcome::MatchForfeited {
            reason: ForfeitReason::Crash,
            detail: with_exit_status(bot, detail),
        },
        CallFailure::Protocol(detail) => TurnOutcome::TurnForfeited {
            reason: ForfeitReason::Protocol,
            detail,
        },
    };

    CallReport {
        outcome,
        latency,
        attempts: 1,
    }
}

fn with_exit_status(bot: &mut BotProcess, detail: String) -> String {
    match bot.exit_status() {
        Ok(Some(code)) => format!("{detail} (process exited with code {code})"),
        _ => format!("{detail} (process still running)"),
    }
}

async fn recover_and_retry(
    bot: &mut BotProcess,
    state: &MatchState,
    acting: Chapter,
    options: &RunOptions,
) -> CallReport {
    warn!(
        turn = state.turn,
        chapter = chapter_name(acting),
        "bot has not been brought a shrubbery; re-sending NewMatch and retrying"
    );

    let resume = client_span("NewMatch");
    let resume_result = async {
        let mut request = Request::new(resume_match_request(
            state,
            &options.match_id,
            acting,
            options.time,
        ));
        request.set_timeout(SETUP_DEADLINE);
        ni_telemetry::inject_context(&mut request);

        let result = bot.client.new_match(request).await;
        record_status(&Span::current(), &result);
        result
    }
    .instrument(resume)
    .await;

    if let Err(status) = resume_result {
        let outcome = match classify(&status) {
            CallFailure::Unreachable(detail) => TurnOutcome::MatchForfeited {
                reason: ForfeitReason::Crash,
                detail: with_exit_status(bot, detail),
            },
            _ => TurnOutcome::MatchForfeited {
                reason: ForfeitReason::Protocol,
                detail: format!("rejected the replacement NewMatch: {}", describe(&status)),
            },
        };

        return CallReport {
            outcome,
            latency: Duration::ZERO,
            attempts: 2,
        };
    }

    let (result, latency) = call_get_orders(bot, state, acting, options).await;

    let status = match result {
        Ok(response) => {
            return CallReport {
                outcome: orders_or_protocol_error(response, state.turn),
                latency,
                attempts: 2,
            }
        }
        Err(status) => status,
    };

    let outcome = match classify(&status) {
        CallFailure::Timeout => TurnOutcome::TurnForfeited {
            reason: ForfeitReason::Timeout,
            detail: format!("no answer after recovery ({})", describe(&status)),
        },
        CallFailure::Unreachable(detail) => TurnOutcome::MatchForfeited {
            reason: ForfeitReason::Crash,
            detail: with_exit_status(bot, detail),
        },
        CallFailure::NeedsShrubbery => TurnOutcome::MatchForfeited {
            reason: ForfeitReason::Protocol,
            detail: "demanded a shrubbery again after NewMatch".to_string(),
        },
        CallFailure::Protocol(detail) => TurnOutcome::TurnForfeited {
            reason: ForfeitReason::Protocol,
            detail,
        },
    };

    CallReport {
        outcome,
        latency,
        attempts: 2,
    }
}

fn orders_or_protocol_error(response: GetOrdersResponse, turn: u32) -> TurnOutcome {
    if response.turn != turn {
        return TurnOutcome::TurnForfeited {
            reason: ForfeitReason::Protocol,
            detail: format!(
                "echoed turn {} while the engine plays {turn}",
                response.turn
            ),
        };
    }

    TurnOutcome::Orders(response.orders.into_iter().map(order_from_proto).collect())
}

async fn call_get_orders(
    bot: &mut BotProcess,
    state: &MatchState,
    acting: Chapter,
    options: &RunOptions,
) -> (Result<GetOrdersResponse, Status>, Duration) {
    let span = client_span("GetOrders");
    span.record("ni.turn", state.turn);

    async {
        let mut request = Request::new(GetOrdersRequest {
            match_id: options.match_id.clone(),
            turn: state.turn,
            view: Some(battlefield_view(state)),
        });
        request.set_timeout(options.time.turn_deadline);

        // The one line that makes the bot's spans children of this one.
        // It must run inside the span it is describing.
        ni_telemetry::inject_context(&mut request);

        let started = Instant::now();
        let result = bot.client.get_orders(request).await;
        let latency = started.elapsed();

        record_status(&Span::current(), &result);
        Span::current().record("ni.latency_us", latency.as_micros() as i64);

        match &result {
            Ok(_) => info!(
                turn = state.turn,
                chapter = chapter_name(acting),
                latency_us = latency.as_micros(),
                "GetOrders answered"
            ),
            Err(status) => warn!(
                turn = state.turn,
                chapter = chapter_name(acting),
                latency_us = latency.as_micros(),
                code = ?status.code(),
                "GetOrders failed"
            ),
        }

        (result.map(|response| response.into_inner()), latency)
    }
    .instrument(span)
    .await
}

async fn identify(bot: &mut BotProcess, label: &str) -> Result<()> {
    let span = client_span("Identify");

    let identity = async {
        let mut request = Request::new(IdentifyRequest {
            protocol_version: PROTOCOL_VERSION,
        });
        request.set_timeout(SETUP_DEADLINE);
        ni_telemetry::inject_context(&mut request);

        let result = bot.client.identify(request).await;
        record_status(&Span::current(), &result);
        result
    }
    .instrument(span)
    .await?
    .into_inner();

    ensure!(
        identity.protocol_version == PROTOCOL_VERSION,
        "bot {label} ({}) speaks protocol {}, engine requires {}",
        identity.name,
        identity.protocol_version,
        PROTOCOL_VERSION
    );

    info!(
        bot = label,
        pid = bot.id(),
        endpoint = bot.endpoint(),
        name = identity.name,
        version = identity.version,
        protocol = identity.protocol_version,
        "bot identified"
    );

    Ok(())
}

async fn notify_match_ended(
    bot: &mut BotProcess,
    match_id: &str,
    chapter: Chapter,
    conclusion: MatchConclusion,
) {
    let span = client_span("MatchEnded");

    async {
        let mut request = Request::new(MatchEndedRequest {
            match_id: match_id.to_string(),
            outcome: conclusion.outcome_for(chapter),
            reason: conclusion.end_reason(),
        });
        request.set_timeout(SETUP_DEADLINE);
        ni_telemetry::inject_context(&mut request);

        let result = bot.client.match_ended(request).await;
        record_status(&Span::current(), &result);

        // Best effort by definition: the most likely reason a match ended is
        // that this bot stopped answering.
        if let Err(status) = result {
            warn!(chapter = ?chapter, code = ?status.code(), "could not deliver MatchEnded");
        }
    }
    .instrument(span)
    .await
}

async fn submit_replay(bot: &mut BotProcess, chapter: Chapter, replay: &Replay) {
    let span = client_span("SubmitReplay");

    async {
        let bytes = replay.encoded_len();

        let mut request = Request::new(SubmitReplayRequest {
            replay: Some(replay.clone()),
        });
        request.set_timeout(REPLAY_DEADLINE);
        ni_telemetry::inject_context(&mut request);

        Span::current().record("ni.replay_bytes", bytes as i64);

        let result = bot.client.submit_replay(request).await;
        record_status(&Span::current(), &result);

        match result {
            Ok(_) => info!(
                chapter = ?chapter,
                turns = replay.turns.len(),
                encoded_bytes = bytes,
                "replay accepted"
            ),
            Err(status) => warn!(
                chapter = ?chapter,
                code = ?status.code(),
                "could not deliver the replay"
            ),
        }
    }
    .instrument(span)
    .await
}

fn chapter_name(chapter: Chapter) -> &'static str {
    match chapter {
        Chapter::A => "A",
        Chapter::B => "B",
    }
}

/// The forfeit taxonomy, spelled the way the JSONL log spells it.
fn log_status(reason: ForfeitReason) -> &'static str {
    match reason {
        ForfeitReason::Timeout => "timeout",
        ForfeitReason::Crash => "unreachable",
        ForfeitReason::Protocol => "protocol",
    }
}
```

### 3.2 The five things that changed, and why

**1. Every call is wrapped in `async { … }.instrument(span)`.** The pattern is
always the same:

```rust
let span = client_span("GetOrders");
async {
    /* build request, inject, call, record status */
}
.instrument(span)
.await
```

The span is created *outside* the async block and attached to it, so
`Span::current()` inside the block is that span. Everything the call does —
including the events it emits and the metadata it injects — belongs to it.

**2. `inject_context` sits between `set_timeout` and `await`.** Both lines
write to the same request; one adds `grpc-timeout`, the other adds
`traceparent`. M3's deadline and M4's trace context are the same idea applied
twice: information the caller has and the callee needs, travelling in
metadata rather than in the message.

**3. The status is recorded on the span, twice.** `rpc.grpc.status_code` is
the numeric gRPC code for filtering (`4` for `DEADLINE_EXCEEDED`, `1` for
`CANCELLED`, `14` for `UNAVAILABLE`); `otel.status_code = "ERROR"` is what
makes a backend paint the span red. A dashboard that shows you the shape of a
failure without a query is the entire value proposition.

**4. `CallReport` replaces a bare `TurnOutcome`.** M3 measured the latency and
printed it; the log needs it, so `request_orders` now returns the outcome plus
the latency of the attempt that produced it plus how many attempts there were.
`attempts: 2` is a `SHRUBBERY_REQUIRED` recovery, visible in the log without
parsing prose.

**5. `println!` became `info!`/`warn!`, except for the board.** The board and
the result line are the *program's output*; everything else is a diagnostic
and now goes to stderr as a structured event. This is what makes `RUST_LOG`
work as a volume control, and what puts every diagnostic in Loki with a trace
id on it.

`turn_span.in_scope(|| { … })` is the synchronous counterpart to
`.instrument()`: no `await` inside the closure, so entering the span directly
is correct — and it is what makes the log line and the events carry the turn's
trace ids.

### 3.3 Replace `crates/ni-engine/src/main.rs`

```rust
use std::{path::PathBuf, time::Duration};

use anyhow::Result;
use clap::{Parser, Subcommand};
use ni_engine::{run_match, BotProcess, MatchConclusion, RunOptions, TimeControl};

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

        #[arg(long = "bot-a-arg", allow_hyphen_values = true)]
        bot_a_arg: Vec<String>,

        #[arg(long = "bot-b-arg", allow_hyphen_values = true)]
        bot_b_arg: Vec<String>,

        #[arg(
            long,
            default_value= "0ms",
            value_parser = parse_duration
        )]
        delay: Duration,

        #[arg(
            long,
            default_value = "500ms",
            value_parser = parse_duration
        )]
        turn_deadline: Duration,

        #[arg(long, default_value_t = 3)]
        strike_limit: u32,

        /// Append one JSON object per turn to this file.
        #[arg(long, default_value = "ni-match.jsonl")]
        match_log: PathBuf,

        /// Write no match log at all. The replay is still built and sent.
        #[arg(long, conflicts_with = "match_log")]
        no_match_log: bool,

        #[arg(long, default_value = "m4-demo")]
        match_id: String,

        #[arg(long)]
        quiet: bool,
    },
}

fn parse_duration(value: &str) -> std::result::Result<Duration, String> {
    let milliseconds = value
        .strip_suffix("ms")
        .ok_or_else(|| "duration must end in ms, for example 200ms".to_string())?
        .parse::<u64>()
        .map_err(|error| format!("invalid millisecond duration: {error}"))?;

    Ok(Duration::from_millis(milliseconds))
}

#[tokio::main]
async fn main() -> Result<()> {
    // First line of the program: everything after this point can be traced,
    // and nothing before it can.
    let telemetry = ni_telemetry::init("ni-engine");

    let Command::Run {
        bot_a,
        bot_b,
        bot_a_arg,
        bot_b_arg,
        delay,
        turn_deadline,
        strike_limit,
        match_log,
        no_match_log,
        match_id,
        quiet,
    } = Cli::parse().command;

    let options = RunOptions {
        delay,
        quiet,
        match_id,
        time: TimeControl {
            turn_deadline,
            strike_limit,
        },
        match_log: (!no_match_log).then_some(match_log),
    };

    let result = run(bot_a, bot_a_arg, bot_b, bot_b_arg, options).await;

    // Flush spans and logs before exiting, whatever happened to the match.
    telemetry.shutdown();

    let conclusion = result?;

    if let MatchConclusion::Forfeit { loser, reason } = conclusion {
        eprintln!("note: chapter {loser:?} forfeited ({reason:?})");
    }

    Ok(())
}

async fn run(
    bot_a_path: PathBuf,
    bot_a_args: Vec<String>,
    bot_b_path: PathBuf,
    bot_b_args: Vec<String>,
    options: RunOptions,
) -> Result<MatchConclusion> {
    let mut bot_a = BotProcess::spawn_with_args(&bot_a_path, "bot A", &bot_a_args).await?;

    let mut bot_b = match BotProcess::spawn_with_args(&bot_b_path, "bot B", &bot_b_args).await {
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

    let match_result = run_match(&mut bot_a, &mut bot_b, options).await;

    let cleanup_a = bot_a.shutdown().await;
    let cleanup_b = bot_b.shutdown().await;

    let conclusion = match_result?;
    cleanup_a?;
    cleanup_b?;
    Ok(conclusion)
}
```

Two details worth pausing on. `let result = run(...).await;` then
`telemetry.shutdown();` then `result?` — the `?` moved *after* the shutdown on
purpose, because an early return would take the spans that explain the failure
with it. And `match_log: (!no_match_log).then_some(match_log)` turns two flags
into the one `Option<PathBuf>` the engine actually wants; clap's
`conflicts_with` only fires on explicitly-passed arguments, so it does not
trip over the default value.

### 3.4 Keep the integration tests compiling

`RunOptions` grew a field, so `crates/ni-engine/tests/failure_modes.rs` no
longer compiles. Fix its constructor now — the interesting new tests come in
Checkpoint 7:

```rust
fn options(deadline_ms: u64, strike_limit: u32) -> RunOptions {
    RunOptions {
        delay: Duration::ZERO,
        quiet: true,
        match_id: "m4-test".to_string(),
        time: TimeControl {
            turn_deadline: Duration::from_millis(deadline_ms),
            strike_limit,
        },
        match_log: None,
    }
}
```

`match_log: None` is the right default for a test: seven tests writing to one
path in parallel would be a race, and none of them look at the file yet.

### 3.5 Run the checkpoint

```sh
cargo fmt
cargo test --workspace
cargo build --workspace
rm -f ni-match.jsonl
./target/debug/ni-engine run \
  --bot-a target/debug/ni-bot \
  --bot-b target/debug/ni-bot \
  --quiet
```

Expected: the result line on stdout, structured logs on stderr, and a new
`ni-match.jsonl`. The stderr lines carry their whole span context, which is
verbose in a terminal and exactly what you want in a dashboard:

```text
 INFO match{otel.name="match" ni.match_id=m4-demo}:turn{otel.name="turn" ni.turn=1
 ni.chapter="A"}:grpc{otel.kind="client" rpc.system="grpc" rpc.service="ni.v1.BotService"
 rpc.method="GetOrders" otel.name="ni.v1.BotService/GetOrders" ni.turn=1
 rpc.grpc.status_code=0 otel.status_code="OK" ni.latency_us=1722}: GetOrders answered
 turn=1 chapter="A" latency_us=1722
```

(wrapped here; it is one line). `RUST_LOG=warn` turns the chatter off and
leaves the failures.

And the log file:

```sh
head -3 ni-match.jsonl
```

```text
{"kind":"match_started","ts_ms":1786976543789,"match_id":"m4-demo","turn_deadline_ms":500,"strike_limit":3,"board_width":10,"board_height":10}
{"kind":"turn","ts_ms":1786976543795,"match_id":"m4-demo","turn":1,"acting":"A","status":"ok","latency_us":1722,"deadline_exceeded":false,"attempts":1,"strikes":0,"orders":[{"unit_id":"A1","move_to":[4,1],"result":"applied","damage":0}]}
{"kind":"turn","ts_ms":1786976543797,"match_id":"m4-demo","turn":2,"acting":"B","status":"ok","latency_us":1833,"deadline_exceeded":false,"attempts":1,"strikes":0,"orders":[{"unit_id":"B1","move_to":[5,1],"attack_target":"A1","result":"applied","damage":4}]}
```

No `trace_id` on those lines. Nothing is wrong: with no collector configured
there is no sampled trace to point at, so the field is omitted rather than
written as a row of zeroes. It appears as soon as one is running, which is the
next checkpoint but one.


---

## Checkpoint 4 — the bots join the trace

The engine is now sending `traceparent` on every call. Nobody is reading it.

### 4.1 Edit `crates/ni-bot/Cargo.toml`

Add two dependencies:

```toml
ni-telemetry = { workspace = true }
tracing = { workspace = true }
```

### 4.2 Edit `crates/ni-bot/src/lib.rs`

Add the import:

```rust
use tracing::{info, Instrument};
```

Then give every handler a span. The pattern for a handler with a body worth
keeping is: build the span from the metadata *before* consuming the request,
hand the message to an inner method, and instrument that.

```rust
#[tonic::async_trait]
impl BotService for ReferenceBot {
    async fn identify(
        &self,
        request: Request<IdentifyRequest>,
    ) -> Result<Response<IdentifyResponse>, Status> {
        let span = ni_telemetry::server_span("ni.v1.BotService/Identify", request.metadata());

        async {
            info!(protocol = PROTOCOL_VERSION, "identified");

            Ok(Response::new(IdentifyResponse {
                name: "reference-bot".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                protocol_version: PROTOCOL_VERSION,
            }))
        }
        .instrument(span)
        .await
    }

    async fn new_match(
        &self,
        request: Request<NewMatchRequest>,
    ) -> Result<Response<NewMatchResponse>, Status> {
        let span = ni_telemetry::server_span("ni.v1.BotService/NewMatch", request.metadata());
        self.handle_new_match(request.into_inner())
            .instrument(span)
            .await
    }

    async fn get_orders(
        &self,
        request: Request<GetOrdersRequest>,
    ) -> Result<Response<GetOrdersResponse>, Status> {
        let span = ni_telemetry::server_span("ni.v1.BotService/GetOrders", request.metadata());
        self.handle_get_orders(request.into_inner())
            .instrument(span)
            .await
    }

    async fn match_ended(
        &self,
        request: Request<MatchEndedRequest>,
    ) -> Result<Response<MatchEndedResponse>, Status> {
        let span = ni_telemetry::server_span("ni.v1.BotService/MatchEnded", request.metadata());

        async {
            info!("match ended");
            Ok(Response::new(MatchEndedResponse {}))
        }
        .instrument(span)
        .await
    }

    async fn submit_replay(
        &self,
        request: Request<SubmitReplayRequest>,
    ) -> Result<Response<SubmitReplayResponse>, Status> {
        let span = ni_telemetry::server_span("ni.v1.BotService/SubmitReplay", request.metadata());

        async {
            let replay = request.into_inner().replay;

            info!(
                turns = replay
                    .as_ref()
                    .map(|replay| replay.turns.len())
                    .unwrap_or(0),
                "replay received"
            );

            Ok(Response::new(SubmitReplayResponse {}))
        }
        .instrument(span)
        .await
    }
}
```

`request.metadata()` borrows; `request.into_inner()` consumes. Build the span
first or the borrow checker will make the point for you.

Then move M3's `new_match` and `get_orders` bodies into an inherent impl,
taking the message rather than the request:

```rust
impl ReferenceBot {
    async fn handle_new_match(
        &self,
        request: NewMatchRequest,
    ) -> Result<Response<NewMatchResponse>, Status> {
        let NewMatchRequest {
            match_id,
            chapter,
            board,
            rules,
            turn,
        } = request;

        // ... M3's validation, unchanged ...

        info!(%match_id, ?chapter, turn, "match announced");

        self.matches
            .write()
            .await
            .insert(match_id, MatchInfo { chapter });

        Ok(Response::new(NewMatchResponse {}))
    }

    async fn handle_get_orders(
        &self,
        request: GetOrdersRequest,
    ) -> Result<Response<GetOrdersResponse>, Status> {
        // ... M3's lookup and validation, unchanged ...

        let orders: Vec<KnightOrder> = choose_order(&view, info.chapter).into_iter().collect();

        info!(turn = request.turn, orders = orders.len(), "orders chosen");

        Ok(Response::new(GetOrdersResponse {
            turn: request.turn,
            orders,
        }))
    }
}
```

Note that `submit_replay` is no longer a bare `Ok(...)`: it says how many
turns it received. That is the whole bot-side of `SubmitReplay` — the bot's
job is to *have been sent* the replay.

### 4.3 Replace `crates/ni-bot/src/main.rs`

```rust
use anyhow::Result;
use clap::Parser;
use ni_bot::{serve, ReferenceBot};

#[derive(Parser)]
#[command(about = "Run the Ni reference bot gRPC server")]
struct Cli {
    #[arg(long, default_value = "tcp://127.0.0.1:0")]
    listen: String,
}

#[tokio::main]
async fn main() -> Result<()> {
    // The engine spawns this process, so it inherits the engine's
    // OTEL_EXPORTER_OTLP_ENDPOINT — but the service name is per-binary.
    let telemetry = ni_telemetry::init("reference-bot");

    let cli = Cli::parse();
    let result = serve(&cli.listen, ReferenceBot::default()).await;

    telemetry.shutdown();
    result
}
```

**The inheritance is the trick.** The engine spawns bots with
`tokio::process::Command`, which passes the parent's environment through. Set
`OTEL_EXPORTER_OTLP_ENDPOINT` once in your shell and all three processes
export to the same collector, with no flags and no config file. Do *not* set
`OTEL_SERVICE_NAME` — the code names each service, and an inherited service
name would collapse all three into one.

### 4.4 Do the same for Roger

`crates/roger-the-shrubber/Cargo.toml` gains the same two dependencies, and
`src/lib.rs` gains

```rust
use tracing::{info, warn, Instrument};
```

Roger's handlers get spans exactly as above (`handle_new_match`,
`handle_get_orders`, `handle_match_ended`), and — the part that matters — his
`eprintln!`s become events:

```rust
        warn!(turn, "forgetting every match");
        warn!(
            turn,
            sleep_ms = self.mischief.sleep_ms,
            "thinking far too hard"
        );
        warn!(turn, "dying mid-call");
```

They were already the right lines; now they are attached to the very span the
engine is about to cancel. "The bot said it was sleeping" and "the engine said
the call timed out" stop being two claims in two terminals.

`src/main.rs`:

```rust
#[tokio::main]
async fn main() -> Result<()> {
    let telemetry = ni_telemetry::init("roger-the-shrubber");

    let cli = Cli::parse();

    tracing::info!(mischief = ?cli.mischief, "roger reporting for duty");

    let result = serve(&cli.listen, Roger::new(cli.mischief)).await;

    telemetry.shutdown();
    result
}
```

### 4.5 Run the checkpoint

```sh
cargo fmt
cargo clippy --workspace --all-targets
cargo test --workspace
cargo build --workspace
./target/debug/ni-engine run \
  --bot-a target/debug/ni-bot \
  --bot-b target/debug/roger-the-shrubber \
  --bot-b-arg --sleep-ms --bot-b-arg 400 \
  --turn-deadline 100ms --quiet
```

Expected: green, and the bots' events now appear on stderr in the same format
as the engine's, with `rpc{otel.name="ni.v1.BotService/GetOrders" …}` span
context in front of them.


---

## Checkpoint 5 — the dashboard

One container, four systems: an OpenTelemetry collector to receive OTLP, Tempo
for traces, Loki for logs, Prometheus for metrics, and Grafana in front of all
of it with the datasources already wired up.

### 5.1 Create `deploy/telemetry/docker-compose.yml`

```yaml
name: ni-telemetry

# One container: an OpenTelemetry collector wired to Tempo (traces),
# Loki (logs), Prometheus (metrics) and Grafana (the dashboard).
#
#   docker compose -f deploy/telemetry/docker-compose.yml up -d
#   open http://localhost:3000
#
# Ni exports over OTLP/HTTP to 4318; 4317 is the gRPC port, kept open so you
# can switch the exporter and see the difference.
services:
  telemetry:
    image: grafana/otel-lgtm:latest
    container_name: ni-telemetry
    ports:
      - "3000:3000"   # Grafana
      - "4317:4317"   # OTLP/gRPC
      - "4318:4318"   # OTLP/HTTP
    environment:
      # Keep the collector's own logs quiet unless something is wrong.
      - ENABLE_LOGS_ALL=false
    volumes:
      - telemetry-data:/data
    restart: unless-stopped

volumes:
  telemetry-data:
```

`grafana/otel-lgtm` is Grafana's own demo image and the shortest path from "I
have OTLP" to "I can see it". Everything in it is a real component — swapping
it for four separate services later changes the compose file and nothing else,
because Ni only ever talks OTLP to a port.

### 5.2 Start it

```sh
docker compose -f deploy/telemetry/docker-compose.yml up -d
docker compose -f deploy/telemetry/docker-compose.yml logs --tail 5
```

The first run pulls about a gigabyte. Wait for the log line saying Grafana is
ready, then open <http://localhost:3000> (admin/admin if it asks; the image
normally allows anonymous access).

### 5.3 Play a traced match

```sh
export OTEL_EXPORTER_OTLP_ENDPOINT=http://localhost:4318
./target/debug/ni-engine run \
  --bot-a target/debug/ni-bot \
  --bot-b target/debug/roger-the-shrubber \
  --bot-b-arg --sleep-ms --bot-b-arg 400 \
  --bot-b-arg --sleep-on-turn --bot-b-arg 4 \
  --turn-deadline 100ms --quiet
```

The first stderr line is now the one you are looking for:

```text
 INFO telemetry exporting over OTLP service="ni-engine" endpoint="http://localhost:4318"
```

…and it appears three times, once per process, with three different service
names.

### 5.4 Find the match in Grafana

1. **Explore → Tempo → Search.** Set *Service Name* to `ni-engine` and run
   the query. One result per match; the duration is the whole match.
2. Click it. The waterfall is the match: a `match` span at the top, a `turn`
   span per chapter-action, and a `ni.v1.BotService/GetOrders` span inside each
   turn.
3. Click the span for turn 4 — the long one — and read its attributes:
   `ni.turn`, `ni.latency_us`, `rpc.grpc.status_code`, `rpc.method`.
4. **Explore → Loki.** Query `{service_name="ni-engine"}` and you have the
   same match as log lines. Add `| json` and filter, or paste a trace id:
   `{service_name="ni-engine"} |= "<trace id>"`.

Grafana also offers a *Logs for this span* action on a span when the
Tempo→Loki link is configured; the manual query above works either way.

### 5.5 The thing that is missing

Look at the trace again. Every span says `ni-engine`. Not one span from
`reference-bot` or `roger-the-shrubber` — even though you instrumented both
handlers in Checkpoint 4, and even though their stderr shows the spans exist.

Decoded off the wire, a plain `ni-bot` vs `ni-bot` match arrives as **75 spans
from one service**:

```text
=== 75 spans, 75 log records
=== 1 distinct trace ids

trace 2cab61f02f1e1c0023451b5f03b4ebd1 (75 spans)
ni-engine            match                                 103481us
  ni-engine            ni.v1.BotService/Identify               3055us
  ni-engine            ni.v1.BotService/NewMatch               2310us
  ni-engine            turn                                    2768us
    ni-engine            ni.v1.BotService/GetOrders              2487us
  ...
```

The bots recorded their spans, queued them in the batch exporter — and then
the engine killed them. `SIGKILL` cannot be caught, so `telemetry.shutdown()`
never ran and the queue died with the process. That is Checkpoint 6.


---

## Checkpoint 6 — shutting a bot down without losing its spans

A batch exporter is a promise to send *later*. `SIGKILL` is a promise there is
no later. M3's `BotProcess::shutdown` was fine when a bot had nothing to say
on the way out; it is now the reason half your trace is missing.

### 6.1 Edit `crates/ni-engine/src/process.rs`

Add the grace period next to the startup deadline:

```rust
pub const STARTUP_DEADLINE: Duration = Duration::from_secs(5);
/// How long a bot gets to finish its own shutdown — which, from M4 on,
/// includes flushing whatever spans are still sitting in its batch queue.
pub const SHUTDOWN_GRACE: Duration = Duration::from_secs(2);
```

and replace `shutdown`:

```rust
    /// Ask, wait, then insist.
    ///
    /// M3 killed bots outright. That was fine when a bot had nothing to say
    /// on the way out; a bot that exports telemetry has a batch queue, and
    /// `SIGKILL` throws it away. So: `SIGTERM` first, a short grace period,
    /// and `SIGKILL` only for a bot that ignores it.
    pub async fn shutdown(&mut self) -> Result<()> {
        if self.child.try_wait()?.is_none() {
            self.request_termination();

            if tokio::time::timeout(SHUTDOWN_GRACE, self.child.wait())
                .await
                .is_err()
            {
                tracing::warn!(
                    grace_ms = SHUTDOWN_GRACE.as_millis(),
                    "bot ignored SIGTERM; killing it"
                );
                let _ = self.child.kill().await;
            }
        }

        let _ = self.child.wait().await;
        let _ = (&mut self.stdout_task).await;
        Ok(())
    }

    /// `tokio::process::Child` can only `SIGKILL`, so the polite signal goes
    /// through `libc`. The pid belongs to a child we spawned and have not
    /// reaped, so it cannot have been recycled.
    fn request_termination(&mut self) {
        let Some(pid) = self.child.id() else {
            return;
        };

        // SAFETY: `kill` with a pid we own and a valid signal number.
        unsafe {
            libc::kill(pid as libc::pid_t, libc::SIGTERM);
        }
    }
```

`kill_on_drop(true)` stays on the `Command` — it is the backstop for a panic
that skips `shutdown` entirely. Politeness has a timeout; a bot that ignores
`SIGTERM` still dies, and says so in the log.

### 6.2 Edit `crates/ni-bot/src/server.rs`

The other half: a server that returns instead of being killed.

```rust
use tokio::{
    net::TcpListener,
    signal::unix::{signal, SignalKind},
};

// ...

    Server::builder()
        .add_service(BotServiceServer::new(service))
        // With a shutdown future the server stops accepting, finishes the
        // calls in flight, and *returns* — which is what lets `main` flush
        // its spans instead of dying with them still queued.
        .serve_with_incoming_shutdown(TcpListenerStream::new(listener), terminated())
        .await?;

    Ok(())
}

/// Resolves when the engine asks this process to stop.
async fn terminated() {
    match signal(SignalKind::terminate()) {
        Ok(mut sigterm) => {
            sigterm.recv().await;
            tracing::info!("SIGTERM: draining and flushing telemetry");
        }
        Err(error) => {
            tracing::warn!(%error, "cannot listen for SIGTERM; running until killed");
            std::future::pending::<()>().await;
        }
    }
}
```

`serve_with_incoming_shutdown` is the same server with one extra argument.
When the future resolves, tonic performs a graceful shutdown and `serve`
returns `Ok(())`, `main` reaches `telemetry.shutdown()`, and the batch
exporter sends what it has. Both bots get this for free — the scaffolding has
been shared since M3.

The `Err` arm matters more than it looks: if the signal handler cannot be
installed, the server must keep serving forever, not exit immediately.
`std::future::pending()` is the future that never completes, which is the
correct "no shutdown signal" behaviour.

### 6.3 Run the checkpoint

```sh
cargo fmt
cargo clippy --workspace --all-targets
cargo build --workspace
./target/debug/ni-engine run \
  --bot-a target/debug/ni-bot \
  --bot-b target/debug/roger-the-shrubber \
  --bot-b-arg --sleep-ms --bot-b-arg 400 \
  --bot-b-arg --sleep-on-turn --bot-b-arg 4 \
  --turn-deadline 100ms --quiet
```

Reload the trace in Grafana. Now every gRPC span has a child from the other
process, decoded here off the wire:

```text
=== 119 spans, 108 log records
=== 1 distinct trace ids

trace 235a866c9c73d18be292da468fce7149 (119 spans)
ni-engine            match                                 243416us status=0
  ni-engine            ni.v1.BotService/Identify               3912us status=1
    reference-bot        ni.v1.BotService/Identify                383us status=0
  ni-engine            ni.v1.BotService/Identify               3058us status=1
    roger-the-shrubber   ni.v1.BotService/Identify                300us status=0
  ni-engine            ni.v1.BotService/NewMatch               3092us status=1
    reference-bot        ni.v1.BotService/NewMatch                275us status=0
  ...
  ni-engine            turn                                    3818us status=0
    ni-engine            ni.v1.BotService/GetOrders              3469us status=1
      reference-bot        ni.v1.BotService/GetOrders               465us status=0
```

Counted by service, that is exactly the match you played:

```text
ni-engine:          {match: 1, Identify: 2, NewMatch: 2, turn: 34,
                     GetOrders: 34, MatchEnded: 2, SubmitReplay: 2}
reference-bot:      {Identify: 1, NewMatch: 1, GetOrders: 17,
                     MatchEnded: 1, SubmitReplay: 1}
roger-the-shrubber: {Identify: 1, NewMatch: 1, GetOrders: 17,
                     MatchEnded: 1, SubmitReplay: 1}
```

Three processes, one trace, and the seats add up: 34 engine `GetOrders`
spans, 17 to each bot. The gap between an engine span and its bot child —
3469us versus 465us — is gRPC framing plus loopback TCP plus the engine's own
view building. That gap is what post 6 measures.

And the JSONL log now carries the ids:

```sh
grep '"timeout"' ni-match.jsonl
```

```text
{"kind":"turn","ts_ms":1786976893941,"match_id":"m4-demo","trace_id":"8656b62ac5cd08f8c3b6661edbdc89a1","span_id":"793ad9325979a730","turn":4,"acting":"B","status":"timeout","detail":"no answer within 100ms (Cancelled: Timeout expired)","latency_us":101397,"deadline_exceeded":true,"attempts":1,"strikes":1,"orders":[]}
```

Paste that `trace_id` into Tempo's search box and you are looking at the turn
that produced the line. That is the whole point of M4 in one copy-paste.


---

## Checkpoint 7 — the integration tests

The log file is an interface now, so test it like one.

### 7.1 Edit `crates/ni-engine/tests/failure_modes.rs`

Add the import and helpers, and the new field in `options`:

```rust
use serde_json::Value;

fn options(deadline_ms: u64, strike_limit: u32) -> RunOptions {
    RunOptions {
        delay: Duration::ZERO,
        quiet: true,
        match_id: "m4-test".to_string(),
        time: TimeControl {
            turn_deadline: Duration::from_millis(deadline_ms),
            strike_limit,
        },
        match_log: None,
    }
}

/// A log path unique to one test, so the suite can run in parallel.
fn log_path(name: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!("ni-m4-{name}.jsonl"));
    let _ = std::fs::remove_file(&path);
    path
}

fn read_entries(path: &PathBuf) -> Vec<Value> {
    std::fs::read_to_string(path)
        .expect("the engine wrote a match log")
        .lines()
        .map(|line| serde_json::from_str(line).expect("every line is one JSON object"))
        .collect()
}
```

Then append three tests:

```rust
#[tokio::test]
async fn a_normal_match_writes_a_readable_jsonl_log() {
    let path = log_path("normal");
    let mut options = options(500, 3);
    options.match_log = Some(path.clone());

    let conclusion = play_against_roger(&[], options).await;
    assert!(matches!(conclusion, MatchConclusion::Decided(_)));

    let entries = read_entries(&path);

    assert_eq!(entries.first().unwrap()["kind"], "match_started");
    assert_eq!(entries.last().unwrap()["kind"], "match_ended");

    let turns: Vec<_> = entries
        .iter()
        .filter(|entry| entry["kind"] == "turn")
        .collect();

    assert!(turns.len() > 4, "a full match has many turns");
    assert!(turns.iter().all(|turn| turn["status"] == "ok"));
    assert!(turns.iter().all(|turn| turn["attempts"] == 1));
    assert!(turns
        .iter()
        .all(|turn| turn["latency_us"].as_u64().is_some()));
    assert_eq!(
        entries.last().unwrap()["turns"].as_u64().unwrap() as usize,
        turns.len()
    );
}

#[tokio::test]
async fn a_forfeited_turn_is_visible_in_the_log() {
    let path = log_path("timeout");
    let mut options = options(100, 2);
    options.match_log = Some(path.clone());

    play_against_roger(&["--sleep-ms", "400"], options).await;

    let entries = read_entries(&path);
    let timeouts: Vec<_> = entries
        .iter()
        .filter(|entry| entry["status"] == "timeout")
        .collect();

    assert_eq!(timeouts.len(), 2, "two strikes, two log lines");
    assert!(timeouts
        .iter()
        .all(|turn| turn["deadline_exceeded"] == true));
    assert!(timeouts.iter().all(|turn| turn["acting"] == "B"));
    assert!(timeouts[0]["detail"]
        .as_str()
        .unwrap()
        .contains("no answer within 100ms"));
}

#[tokio::test]
async fn a_recovered_turn_records_two_attempts() {
    let path = log_path("recovery");
    let mut options = options(500, 3);
    options.match_log = Some(path.clone());

    play_against_roger(&["--forget-on-turn", "4"], options).await;

    let entries = read_entries(&path);
    let recovered: Vec<_> = entries
        .iter()
        .filter(|entry| entry["attempts"] == 2)
        .collect();

    assert_eq!(recovered.len(), 1, "exactly one turn needed a shrubbery");
    assert_eq!(recovered[0]["turn"], 4);
    assert_eq!(recovered[0]["status"], "ok");
}
```

The last one is the test M3 could not write. "Recovery happened, once, on turn
4, and the turn still resolved" was previously only observable by reading
prose on stdout; now it is a field in a file.

These tests deliberately assert nothing about trace ids. No collector runs in
CI, so there is no sampled trace, so `trace_id` is absent — and asserting its
absence would be asserting that CI has no collector.

### 7.2 Run the checkpoint

```sh
cargo fmt --check
cargo clippy --workspace --all-targets
cargo build --workspace
cargo test --workspace
```

Expected: everything green, including 10 tests in `failure_modes` (M3's seven
plus these three) and 3 in `ni-telemetry`. The suite still finishes in under a
second.


---

## Checkpoint 8 — watch each thing happen

Tests assert; this checkpoint is for looking. Every transcript below is real
output, trimmed of board frames.

### 8.1 A slow bot, seen from both sides

```sh
export OTEL_EXPORTER_OTLP_ENDPOINT=http://localhost:4318
./target/debug/ni-engine run \
  --bot-a target/debug/ni-bot \
  --bot-b target/debug/roger-the-shrubber \
  --bot-b-arg --sleep-ms --bot-b-arg 400 \
  --bot-b-arg --sleep-on-turn --bot-b-arg 4 \
  --turn-deadline 100ms --quiet
```

The subtree for turn 4:

```text
  ni-engine            turn                                  102624us status=0
    ni-engine            ni.v1.BotService/GetOrders            102340us status=2   (ERROR)
      roger-the-shrubber   ni.v1.BotService/GetOrders            101107us status=0
```

Roger was told to sleep **400ms**. His span lasts **101ms**. Read that twice:
the deadline the engine set with `set_timeout` did not merely stop the
*client* waiting — it cancelled the *server's* work. tonic drops the handler
future when the call is cancelled, so `tokio::time::sleep(400ms)` never
finished. The bot's own span is the evidence, and you cannot get that evidence
from either process alone.

The engine's span attributes for that call, from one such run:

```text
rpc.system=grpc rpc.service=ni.v1.BotService rpc.method=GetOrders
ni.turn=4 rpc.grpc.status_code=1 ni.latency_us=101397
```

`status_code=1` is `CANCELLED` — M3's finding, now a queryable attribute
rather than a sentence. In Tempo:
`{ .rpc.grpc.status_code = 1 }` finds every cancelled call you have ever run.

### 8.2 A bot that dies mid-call

```sh
./target/debug/ni-engine run \
  --bot-a target/debug/ni-bot \
  --bot-b target/debug/roger-the-shrubber \
  --bot-b-arg --crash-on-turn --bot-b-arg 6 --quiet
```

```text
 INFO rpc{otel.name="ni.v1.BotService/SubmitReplay" otel.kind="server"}: replay received turns=6
 INFO grpc{... rpc.method="SubmitReplay" ni.replay_bytes=250 rpc.grpc.status_code=0
      otel.status_code="OK"}: replay accepted chapter=A turns=6 encoded_bytes=250
 WARN grpc{... rpc.method="SubmitReplay" ni.replay_bytes=250 rpc.grpc.status_code=14
      otel.status_code="ERROR"}: could not deliver the replay chapter=B code=Unavailable
result: chapter A wins - chapter B forfeits (unreachable)
 INFO SIGTERM: draining and flushing telemetry
note: chapter B forfeited (Crash)
```

Three things in one transcript. The surviving bot got the replay and said so.
The dead one produced `UNAVAILABLE` (14) on a span marked ERROR — a failure
that is *fine*, recorded honestly. And the last log line is the surviving bot
flushing its spans because it was asked to stop rather than killed.

Roger's own spans for turns 1–5 are in the trace. His span for turn 6 is not:
he called `std::process::exit(101)` mid-handler, and a process that exits
inside a span never reports it. **A crashed process's last span is always
missing** — the engine's span is the only evidence a call happened at all.
Worth knowing before you spend an afternoon looking for it.

### 8.3 A bot that forgets the match

```sh
./target/debug/ni-engine run \
  --bot-a target/debug/ni-bot \
  --bot-b target/debug/roger-the-shrubber \
  --bot-b-arg --forget-on-turn --bot-b-arg 6 --quiet
```

In the trace, one `turn` span contains **four** children: `GetOrders`
(FAILED_PRECONDITION), `NewMatch`, `GetOrders` (OK), and Roger's server spans
under each. The recovery is a shape now — a turn that is visibly wider than
its neighbours — and in the log it is `"attempts":2` on a turn with
`"status":"ok"`.

### 8.4 Reading the log with `jq`

```sh
jq -r 'select(.kind=="turn") | "turn \(.turn) \(.acting) \(.status) \(.latency_us)us"' \
  ni-match.jsonl | head -5
```

```text
turn 1 A ok 1738us
turn 2 B ok 1906us
turn 3 A ok 1626us
turn 4 B ok 1935us
turn 5 A ok 1549us
```

The three turns that took longest:

```sh
jq -s 'map(select(.kind=="turn")) | sort_by(-.latency_us) | .[0:3]
       | map({turn, acting, latency_us})' ni-match.jsonl
```

```text
[
  { "turn": 8,  "acting": "B", "latency_us": 2946 },
  { "turn": 11, "acting": "A", "latency_us": 2709 },
  { "turn": 10, "acting": "B", "latency_us": 2401 }
]
```

How the match went, by status:

```sh
jq -s 'map(select(.kind=="turn").status) | group_by(.)
       | map({status: .[0], count: length})' ni-match.jsonl
```

```text
[{ "status": "ok", "count": 33 }]
```

Every trace id that belongs to a forfeited turn — the query you actually use:

```sh
jq -r 'select(.status? and .status != "ok") | "\(.status) \(.turn) \(.trace_id)"' \
  ni-match.jsonl
```

### 8.5 How big is a replay?

```sh
./target/debug/ni-engine run --bot-a target/debug/ni-bot --bot-b target/debug/ni-bot \
  --quiet 2>&1 | grep -o "encoded_bytes=[0-9]*"
```

```text
encoded_bytes=977
```

977 bytes for a 33-turn match — about 30 bytes per turn, because a
`TurnRecord` is a handful of varints and one order. That is a useful number to
have written down *before* post 6 starts arguing about payload sizes, and a
reminder that "the big message" is only big once the match is long. A 100-turn
match with four orders per turn is the payload to measure.


---

## What the trace actually shows

Five things this milestone taught, in the order they surprised.

**1. A propagated deadline cancels the server's work.** Roger sleeps 400ms
behind a 100ms deadline and his span is 101ms long. The deadline is not a
client-side stopwatch; tonic sends `grpc-timeout`, and when the call is
cancelled the handler future is dropped mid-`await`. This is the concrete
version of post 3's claim, and it is only visible with spans on both sides.

**2. `SIGKILL` and batch export are incompatible.** M3's `shutdown` was
correct for M3. Adding telemetry to a child process silently changed the
contract between parent and child, and the symptom — half a trace — looked
like a propagation bug for as long as it took to notice which service was
missing. Telemetry changes how you have to *stop* things.

**3. A crashed process cannot report its own crash.** Roger's fatal turn has
an engine span and no bot span, forever. The lesson generalises: the caller's
span is the only reliable record that a call happened, so the caller's span is
where the status belongs.

**4. `tracing` fields must be declared before they can be recorded.** A span's
field set is fixed at creation. Every missing attribute in this milestone was
a `record` call for a field that was not in the `info_span!`, and it fails
silently — no warning, no error, just an absent attribute in the dashboard.

**5. One match is one trace, and that is the right size.** 119 spans for a
34-turn match, three or four per turn. Tempo shows it as a single waterfall you can read
top to bottom. A trace per turn would have been tidier per-span and useless
for the question you actually ask, which is "what happened in that match?".

## Follow one slow turn through your code

Roger sleeps 400ms; the deadline is 100ms; it is turn 4.

1. `run_match` (inside the `match` span) creates the `turn` span with
   `ni.turn = 4`, `ni.chapter = "B"`.
2. `request_orders`, instrumented with that span, calls `call_get_orders`.
3. `client_span("GetOrders")` creates the `grpc` span as a child of `turn`,
   and records `ni.turn = 4` on it.
4. Inside that span: `set_timeout(100ms)` writes `grpc-timeout: 100m`;
   `inject_context` writes `traceparent: 00-<trace>-<span of the grpc span>-01`.
5. Roger's `get_orders` calls `server_span(...)`, which extracts that
   `traceparent` and adopts it as the parent. His span is now a child of the
   engine's `grpc` span, in a different process.
6. Roger's `maybe_sleep` emits `WARN thinking far too hard turn=4 sleep_ms=400`
   *inside that span*, so the event carries the same trace id, and starts
   sleeping.
7. At 100ms the client cancels. tonic drops Roger's handler future: his sleep
   never finishes, his span closes at ~101ms with no status.
8. The engine's call returns `Status { code: Cancelled }`. `record_status`
   writes `rpc.grpc.status_code = 1` and `otel.status_code = "ERROR"`;
   `ni.latency_us = 101397` goes on the same span.
9. `classify` → `CallFailure::Timeout` → `TurnOutcome::TurnForfeited` (M3,
   unchanged).
10. `run_match` records the strike, then — in `turn_span.in_scope(...)` —
    emits `WARN turn forfeited …` and calls `log.record_turn`, which asks
    `ni_telemetry::current_ids()` for the turn span's ids and writes them into
    the JSONL line.
11. `telemetry.shutdown()` in `main` flushes the engine's spans;
    `BotProcess::shutdown` `SIGTERM`s Roger, who drains, flushes and exits.
12. In Grafana: one red span, one wide turn, and two log lines from two
    processes sharing a trace id.

The only new *decision* in that list is step 4. Everything else is plumbing
that exists to make step 12 possible.

## Who owns what

| Concern | Owner | Where |
|---|---|---|
| Spans, events, fields | `tracing` | everywhere |
| Collecting, filtering, formatting | `tracing-subscriber` | `ni-telemetry::init` |
| Span → OTel span, `set_parent` | `tracing-opentelemetry` | `ni-telemetry` |
| Event → OTel log record with trace ids | `opentelemetry-appender-tracing` | `ni-telemetry::init` |
| `traceparent` format | `opentelemetry_sdk::propagation` | `ni-telemetry` |
| Where the bytes go | `opentelemetry-otlp` + the collector | env var + compose file |
| Which calls get spans, and what they are named | Ni | `client_span`, `server_span` |
| What a match looks like as a trace | Ni | `match` → `turn` → `grpc` |
| What is worth writing down per turn | Ni | `log.rs` |
| Whether a turn or a match is lost | Ni (M3) | `policy.rs`, untouched |
| Whether an order is legal | Ni (M1) | `ni-game`, untouched |

The middle rows are the milestone. OpenTelemetry gives you a vocabulary for
context; it has no opinion about what your system's spans should be, and the
three-level `match → turn → grpc` shape is where you write that opinion down.


---

## Beginner troubleshooting

### Nothing appears in Grafana

Check, in order: is the container up (`docker compose … ps`), is
`OTEL_EXPORTER_OTLP_ENDPOINT` exported in *this* shell, does the first stderr
line say `telemetry exporting over OTLP`, and are you looking at a time range
that includes the last minute. A match lasting 100ms is easy to scroll past.

### "cannot find function `init` in crate `ni_telemetry`"

The crate is not in the workspace `members` list, or not in that crate's
`[dependencies]`. Both are needed.

### Trait errors mentioning `SpanExporter` or `WithExportConfig`

Two causes. A missing `use opentelemetry_otlp::WithExportConfig;` — the
`with_endpoint` and `with_timeout` methods come from that trait, and rustc
will tell you so if you read past the first line. Or a version skew: all the
`opentelemetry*` crates must be 0.32 and `tracing-opentelemetry` must be 0.33.

### An attribute is missing from a span in the dashboard, but the code records it

The field was not declared in the `info_span!`. Add
`your.field = tracing::field::Empty` to the macro. `record` on an undeclared
field does nothing and warns nowhere.

### Bot spans never appear

Checkpoint 6. Without `SIGTERM` and `serve_with_incoming_shutdown` the bots
are `SIGKILL`ed with their spans still queued.

### Every span says `ni-engine`

`OTEL_SERVICE_NAME` is set in your shell and the SDK is preferring it over the
per-binary name. Unset it: `unset OTEL_SERVICE_NAME`.

### The bot hangs at startup, or the engine says "invalid bot readiness line"

Something is writing to the bot's **stdout**. The engine parses the first
stdout line as `LISTENING tcp://…`. Keep the fmt layer on stderr
(`.with_writer(std::io::stderr)`), and use `tracing::info!`, never `println!`,
inside a bot.

### `` `Span` cannot be sent between threads safely `` / clippy's `await_holding_span_guard`

You wrote `let _guard = span.enter();` in an `async fn`. Use
`async { … }.instrument(span).await` instead; `enter()` is for synchronous
blocks only.

### `borrow of moved value: request` in a bot handler

`request.into_inner()` consumes the request, and `server_span` needs
`request.metadata()`. Build the span first.

### The match log is empty, or missing

`--no-match-log` was passed, or `--match-log` points somewhere unwritable —
in which case `MatchLog::create` returns an error and the run stops before the
first turn, which is deliberate: a log you asked for and did not get is worth
failing over. Write errors *during* a match only warn.

### `unused_must_use` on `Telemetry`

You called `ni_telemetry::init(...)` without binding the result. Bind it and
call `.shutdown()` at the end of `main`, or you will export nothing.

### The container eats a gigabyte of disk

It is Grafana, Tempo, Loki, Prometheus and a collector.
`docker compose -f deploy/telemetry/docker-compose.yml down -v` removes the
container and the volume.


---

## Appendix A — no Docker? A 100-line OTLP receiver

Everything M4 exports can be inspected without a dashboard. This script
accepts OTLP/HTTP, decodes it with the official protobuf definitions, and
prints the trace tree on `SIGTERM`. It is how the trace trees in this workbook
were produced.

```sh
pip install opentelemetry-proto
python3 otlp_inspect.py 4318 &
OTEL_EXPORTER_OTLP_ENDPOINT=http://127.0.0.1:4318 \
  ./target/debug/ni-engine run --bot-a target/debug/ni-bot --bot-b target/debug/ni-bot --quiet
sleep 5 && pkill -TERM -f otlp_inspect.py
```

```python
#!/usr/bin/env python3
"""Minimal OTLP/HTTP receiver: prints the trace tree and log lines it is sent."""
import signal
import sys
from http.server import BaseHTTPRequestHandler, HTTPServer

from opentelemetry.proto.collector.logs.v1 import logs_service_pb2
from opentelemetry.proto.collector.trace.v1 import trace_service_pb2

SPANS, LOGS = [], []


def service_of(resource):
    for attribute in resource.attributes:
        if attribute.key == "service.name":
            return attribute.value.string_value
    return "?"


def attrs_of(item):
    return {a.key: a.value.string_value or a.value.int_value for a in item.attributes}


class Server(HTTPServer):
    allow_reuse_address = True


class Handler(BaseHTTPRequestHandler):
    def log_message(self, *args):
        pass

    def do_POST(self):
        body = self.rfile.read(int(self.headers.get("content-length", 0)))

        if self.path.endswith("/v1/traces"):
            request = trace_service_pb2.ExportTraceServiceRequest()
            request.ParseFromString(body)
            for rs in request.resource_spans:
                service = service_of(rs.resource)
                for ss in rs.scope_spans:
                    for span in ss.spans:
                        SPANS.append(dict(
                            service=service, name=span.name,
                            trace_id=span.trace_id.hex(), span_id=span.span_id.hex(),
                            parent=span.parent_span_id.hex(), kind=span.kind,
                            start=span.start_time_unix_nano, end=span.end_time_unix_nano,
                            status=span.status.code, attrs=attrs_of(span)))
        elif self.path.endswith("/v1/logs"):
            request = logs_service_pb2.ExportLogsServiceRequest()
            request.ParseFromString(body)
            for rl in request.resource_logs:
                service = service_of(rl.resource)
                for sl in rl.scope_logs:
                    for record in sl.log_records:
                        LOGS.append(dict(
                            service=service, severity=record.severity_text,
                            body=record.body.string_value,
                            trace_id=record.trace_id.hex(), span_id=record.span_id.hex()))

        print(f"POST {self.path} {len(body)} bytes", flush=True)
        self.send_response(200)
        self.send_header("content-length", "0")
        self.end_headers()


def report(*_args):
    print(f"\n=== {len(SPANS)} spans, {len(LOGS)} log records")

    traces = {}
    for span in SPANS:
        traces.setdefault(span["trace_id"], []).append(span)
    print(f"=== {len(traces)} distinct trace ids")

    def tree(spans, parent=None, depth=0):
        roots = [s for s in spans
                 if (s["parent"] in ("", "0" * 16) if parent is None else s["parent"] == parent)]
        for span in sorted(roots, key=lambda s: s["start"]):
            micros = (span["end"] - span["start"]) // 1000
            print(f"{'  ' * depth}{span['service']:<20} {span['name']:<36} "
                  f"{micros:>7}us status={span['status']}")
            tree(spans, span["span_id"], depth + 1)

    for trace_id, spans in traces.items():
        print(f"\ntrace {trace_id} ({len(spans)} spans)")
        tree(spans)

    for record in LOGS[:14]:
        print(f"{record['service']:<20} {record['severity']:<5} {record['body']:<28} "
              f"trace={record['trace_id'][:16]}… span={record['span_id']}")

    sys.stdout.flush()
    sys.exit(0)


if __name__ == "__main__":
    signal.signal(signal.SIGTERM, report)
    port = int(sys.argv[1]) if len(sys.argv) > 1 else 4318
    print(f"listening on http://127.0.0.1:{port}", flush=True)
    Server(("127.0.0.1", port), Handler).serve_forever()
```

Two things worth noticing while you have it running. The trace ids in the log
records are the same ids in your JSONL file — that is the join. And an OTLP
export is one HTTP POST with a protobuf body, roughly 50KB of spans for a
33-turn match; whatever a dashboard does with that is its own business.

## Notes to collect for blog post 3

Write these down while the work is fresh.

- The exact shape of a two-process span pair, and the gap between the client
  span and the server span it contains.
- That a *propagated* deadline cancels the server's work: 400ms of sleep,
  101ms of span. This is the strongest available argument for propagating a
  deadline rather than timing out locally, and M3 could not make it.
- Why trace context belongs in metadata and not in the proto, phrased as a
  question about who has to implement it.
- The `SIGKILL` discovery: adding telemetry to a child process changed how the
  parent must shut it down, and the symptom looked like a propagation bug.
- The crashed-bot asymmetry: the caller's span is the only record that
  survives, therefore the caller's span is where the status goes.
- That `tracing` drops recorded fields that were not declared — a
  silent-failure mode worth one paragraph on its own.
- The number: 977 bytes of replay for a 33-turn match, and what that implies
  about post 6's payload sizes.
- One screenshot of the waterfall for a match with a forfeit in it. That
  picture is the post's opening image.

## M4 completion checklist

```sh
cargo fmt --check
cargo clippy --workspace --all-targets
cargo build --workspace
cargo test --workspace
```

- [ ] `ni-telemetry` exists and is the only crate that names OpenTelemetry
- [ ] Every gRPC call in the engine has a span, and every span injects
      `traceparent`
- [ ] Both bots create a server span parented to the caller's
- [ ] Three service names appear in the dashboard, never one
- [ ] Nothing but the board, the result line and the readiness line goes to
      stdout
- [ ] The match runs identically with no collector configured
- [ ] `ni-match.jsonl` has one line per turn, with latency, status, attempts,
      strikes and order outcomes
- [ ] A forfeited turn's log line carries a trace id that finds the turn in
      Tempo
- [ ] `SubmitReplay` reaches both bots, and its encoded size is on the span
- [ ] Bots are `SIGTERM`ed, drain, and flush their spans before exiting
- [ ] `cargo test --workspace` includes 10 failure-mode tests and 3 telemetry
      tests
- [ ] `proto/ni/v1/ni.proto` is byte-identical to M3's
- [ ] `ni-game` has no new code in it

M5 next: Unix domain sockets behind the transport flag, and the measurement
harness — empty call, 1KB, 1MB, over UDS versus loopback TCP. The spans you
just built are how you will tell whether the numbers mean anything.
