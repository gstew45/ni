# M5 step by step — Unix sockets, and what a wire is actually for

This is the hands-on workbook for milestone **M5**, in the same shape as
[`m2-step-by-step.md`](m2-step-by-step.md),
[`m3-step-by-step.md`](m3-step-by-step.md) and
[`m4-step-by-step.md`](m4-step-by-step.md): which file to open, what to put in
it, what the Rust means, why the design is the way it is, and what command to
run before continuing.

M2 built the loop, M3 taught it to survive its opponents, M4 made it visible.
All three ran over loopback TCP, and none of them ever asked what the TCP was
*for*. M5 takes the network away — the bots keep talking gRPC, over the same
service definition, through a Unix domain socket instead of `127.0.0.1` — and
then measures what changed. The answer is the point of blog posts 5 and 6, and
it is not the answer most people expect.

Every Rust block below was compiled, formatted, clippy-cleaned and run against
the repository's M4 implementation with tonic 0.13, hyper-util 0.1 and tower
0.5. Terminal transcripts are real output from a 4-vCPU Linux VM (Intel Xeon
@ 2.10 GHz, kernel 6.18, Ubuntu 24.04, rustc 1.94) — match transcripts from a
debug build, every measurement in Checkpoints 6 and 7 from `--release`. The
machine matters more in this milestone than in any previous one: M5 produces *numbers*, and a
number without a machine attached to it is a rumour.

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

Everything here is Linux and macOS only. Unix domain sockets exist on Windows
10+ as `AF_UNIX`, but without the peer credentials M5 uses, so
`crates/ni-bot/src/server.rs` would need a `#[cfg(windows)]` arm and a
different identity story. Ni does not have one.

Checkpoints 6 and 7 produce measurements. Do not skip the boring instruction
to run the harness more than once — Checkpoint 7 exists mostly to stop you
believing your first run.

## What M4 leaves unmeasured

Read this list before writing code; each line is something the system cannot
currently tell you.

- **The transport is a hard-coded string.** `BotProcess::spawn` writes
  `tcp://127.0.0.1:0` into the child's argv and `serve` refuses anything that
  does not start with `tcp://`. The implementation plan has promised a
  `--transport` flag since M0 and there is nowhere to put it.
- **Nothing knows who a bot is talking to.** A bot answers `Identify` for
  anyone who connects. On loopback, "anyone" is every process on the host.
- **The blog trace's central claim is untested.** "Strip the network from a
  service boundary and you find out which costs were the network's and which
  were the boundary's" is, as of M4, a sentence. There is no second transport
  to compare against and no harness to compare with.
- **`SubmitReplay` is measured but not exercised.** M4 records
  `ni.replay_bytes` on a span for a replay that a 30-turn match makes about 4 KB
  of. Post 6 wants 1 MB, deliberately, repeatedly.
- **Latency is observed one call at a time.** The match log has a `latency_us`
  per turn, which is a sample of one under whatever conditions that turn
  happened in. There is no p50, no p99, no iteration count, no warm-up.

## What M5 adds

- A **`transport` module** in `ni-engine` owning the two socket families: what
  a bot is told to listen on, how the engine dials it, and — new, and more
  interesting than it sounds — who deletes the socket afterwards.
- **`--transport tcp|unix`** on `ni-engine run`, plus `--socket-dir`, with the
  design doc's `/run/ni` demoted to `$XDG_RUNTIME_DIR/ni` because `/run` needs
  root.
- **A bot that binds either family**, announces which one it got, protects its
  socket file, refuses to steal a live one, and unlinks its own on the way out.
- **Peer credentials.** Over a Unix socket the bot logs the pid, uid and gid of
  the process that connected, from the kernel rather than from the peer.
- **Transport parity tests**: the same deterministic match, the same timeout,
  the same crash, over both families, asserting the same results.
- **`ni-bench`** — a new crate: one service definition, two transports, four
  payload sizes, percentiles rather than averages, and a count of the calls
  that went strange.
- **Two small examples** that measure the parts of a round trip a socket cannot
  change, so Checkpoint 7 can attribute milliseconds instead of guessing about
  them.

## What M5 deliberately leaves alone

- **The proto does not change.** Not one line, again. A transport is not part of
  a contract: `ni.v1.BotService` describes messages and methods, and where the
  bytes go is the deployment's business. `buf breaking` stays quiet.
- **The match loop does not branch on the transport.** `RunOptions` gains a
  `transport` field and the loop *records* it. If M5 had needed an `if` in
  `match_runner.rs`, the abstraction would have been wrong.
- **`ni-game`** has no new code in it. Fourth milestone running. This is the
  dividend from M1's rule that the rules engine sees no I/O.
- **No daemon, no discovery.** `/run/ni/*.sock` glob discovery and gRPC health
  checking are M6. M5 creates sockets it already knows the names of.
- **No abstract sockets.** Linux's `\0`-prefixed namespace is described in the
  interlude and not used; it trades filesystem permissions for network-namespace
  permissions, which is a different security model, not a shortcut.
- **No TLS over UDS.** The filesystem is the access control. Checkpoint 8 shows
  what that buys.
- **Metrics still are not exported.** `ni-bench` prints a table and can emit
  JSON. Turning percentiles into OTLP histograms is worth doing when there is a
  tournament producing them continuously, which there is not yet.

## Design decisions this milestone settles

| Question | M5's answer | Why |
|---|---|---|
| Who chooses the socket path? | The **engine**, and it passes it on argv | A port can be requested with `:0` and reported back. Nothing allocates paths, so somebody has to name them, and the engine is the only participant that knows the match id |
| Who deletes the socket file? | Both, at different levels: the **bot** unlinks its own socket, the **engine** owns the directory | A bot that exits normally cleans up after itself. A bot that is `SIGKILL`ed cannot, so the layer above has to be able to |
| How does the engine know what a bot bound? | The readiness line carries the **scheme**: `LISTENING unix:///…` | M4's parser hard-coded `LISTENING tcp://`. Sending the scheme means the engine dials what exists rather than what it asked for — and a bot that fell back to TCP is then not a mystery |
| Where do sockets live by default? | `$XDG_RUNTIME_DIR/ni/<match-id>/`, else `/tmp/ni-<uid>/<match-id>/` | The design doc's `/run/ni` needs root. `$XDG_RUNTIME_DIR` is per-user, `0700`, and on tmpfs |
| Does the match loop know the transport? | It **records** it, and never branches on it | The claim under test is that the loop cannot tell. A branch would be an admission that it can |
| Are stale socket files removed automatically? | Only after **proving** they are stale, by connecting to them | `remove_file` on a live socket silently steals the name from a running bot. Connect first: an answer means misconfiguration, `ECONNREFUSED` means a corpse |
| What identity does a bot get from a connection? | Over UDS, kernel-supplied `pid`/`uid`/`gid`; over TCP, an address | This is the one capability difference that is not about speed, and it is the reason containerd and the CRI use Unix sockets |
| What does the benchmark measure? | The **real RPCs**, at four sizes, sequentially, with warm-up | A synthetic `Echo` method would measure a method nobody calls. `Identify`, `GetOrders` and `SubmitReplay` are the contract, and their sizes are measured with `encoded_len` rather than asserted |
| Mean or percentiles? | Percentiles, nearest-rank, plus a count of calls over 10 ms | The interesting behaviour in this milestone is rare and large. A mean hides it and an interpolated percentile invents latencies that were never observed |

## What you will create

```text
crates/ni-engine/src/
  transport.rs           NEW  Transport, Listen, SocketDir, dial()
  process.rs             edited     spawn on a given Listen; dial by scheme
  match_runner.rs        edited     records the transport, branches on nothing
  log.rs                 edited     `transport` in the match_started line
  main.rs                edited     --transport, --socket-dir, directory ownership
  lib.rs                 edited     module list
crates/ni-engine/tests/
  failure_modes.rs       edited     new spawn signature
  transports.rs          NEW  six tests: parity, failures, cleanup, dial errors

crates/ni-bot/src/
  server.rs              rewritten  bind either family, stale sockets, peer creds
  lib.rs                 edited     log the peer in Identify

crates/ni-bench/         NEW CRATE
  Cargo.toml
  src/lib.rs             Samples, percentiles, replay_of_at_least
  src/main.rs            the harness: two transports, four cases
  examples/boundary_cost.rs   what protobuf costs with no socket at all
  examples/dial_errors.rs     what "nobody is there" looks like on each family
```

Plus the workspace `Cargo.toml`.

---

## Interlude — what a Unix domain socket actually is

Everything in this section is true whether or not you are writing Ni. Read it
before Checkpoint 1; the code makes much more sense afterwards, and about half
of the design decisions above stop looking arbitrary.

### The two-minute version

A **Unix domain socket** (UDS, `AF_UNIX`, sometimes `AF_LOCAL`) is a socket
whose two endpoints are both in the same kernel. You create it with the same
`socket()`, `bind()`, `listen()`, `accept()`, `connect()`, `read()`, `write()`
calls you use for TCP. The API is the same. The address is not:

```c
struct sockaddr_in  { sa_family_t sin_family;  in_port_t sin_port; struct in_addr sin_addr; };
struct sockaddr_un  { sa_family_t sun_family;  char sun_path[108]; };
```

A TCP socket is addressed by *an IP address and a port*, which are values a
routing layer knows how to find. A Unix socket is addressed by *a path*, which
is a name in a filesystem. That single substitution is the whole difference,
and almost everything else follows from it:

- there is nothing to route, so there is no routing;
- there is no port, so there is no port allocator, no ephemeral range and no
  `TIME_WAIT`;
- there is a *file*, so there are permissions, a parent directory, an owner,
  and — the part that bites — a name that outlives the process that made it.

`SOCK_STREAM` over `AF_UNIX` gives you exactly the semantics TCP gives you:
connection-oriented, reliable, ordered, no message boundaries. That is why
HTTP/2 and therefore gRPC run over it unmodified.

### What the kernel stops doing

For a loopback TCP connection, a write travels: your buffer → socket layer →
TCP (segmentation to MSS, sequence numbers, ACK tracking, congestion and
receive windows) → IP (header, route lookup for `127.0.0.1`) → the `lo`
device's transmit path → netfilter hooks (and connection tracking, if
`nf_conntrack` is loaded) → back up through IP and TCP on the receive side →
the receiving socket's queue.

For a Unix socket, a write travels: your buffer → socket layer → the
*receiver's* queue. The kernel copies the payload into an `sk_buff` and appends
it to the peer's receive queue. That is the transport.

Concretely, what disappears:

| Mechanism | Loopback TCP | Unix socket |
|---|---|---|
| Three-way handshake | yes (cheap, but three traversals) | no — `connect` links two structures |
| Sequence numbers, ACKs, retransmit timers | yes | none: delivery is a memcpy, and a memcpy does not get lost |
| Delayed-ACK / Nagle interaction | yes — see Checkpoint 7 | no ACKs to delay |
| Congestion control, receive window | yes (irrelevant at loopback speeds, still executed) | flow control is "is the peer's queue full?" |
| IP header, route lookup, MSS/MTU | yes (loopback MTU 65536) | none; no framing at the socket layer at all |
| netfilter / iptables / conntrack | yes — a stray `INPUT` rule can break your loopback gRPC | not traversed |
| Checksums | already skipped on `lo` | nothing to checksum |
| Port exhaustion, `TIME_WAIT` accumulation | yes, under connection churn | there are no ports |

That table is why UDS is *usually* faster. It is not a promise that it will be
faster on your machine, which is the entire subject of Checkpoint 7.

### What a Unix socket can do that TCP cannot do at all

This is the half of the argument that has nothing to do with speed, and it is
the half that decides real architectures.

**1. Kernel-verified peer identity.** `getsockopt(fd, SOL_SOCKET, SO_PEERCRED)`
returns the pid, uid and gid of the process at the other end. The peer does not
send these; the kernel fills them in from the process that called `connect`.
There is no equivalent for TCP — a connection from `127.0.0.1` proves the
packet came from this host and nothing else. Any local process, any local user,
anything sharing the network namespace could have opened it.

This is why the Docker daemon, containerd, the Kubernetes CRI and the kubelet
device-plugin API all speak gRPC over Unix sockets: `uid == 0` or
`gid == docker` *is* the authorization decision, and it is made by the kernel
rather than by a token the caller supplies. In Ni, Checkpoint 2 makes the
reference bot log it, and Checkpoint 8 shows the difference in one line of
output.

Two caveats worth knowing before you build policy on it. The credentials are
captured **at connect time**, so a process that `exec`s something else
afterwards keeps the identity it connected with; and **pids are recycled**,
which makes `uid`/`gid` the durable half and `pid` a diagnostic. `SCM_CREDENTIALS`
sends credentials per message rather than per connection, if you need the
tighter version.

**2. File descriptor passing.** With `sendmsg` and an `SCM_RIGHTS` control
message, a process can hand a *file descriptor* — an open file, a listening
socket, a pipe, a memfd — to the process on the other end. The receiver gets a
real descriptor into its own table, not a copy of the bytes. This is how
systemd socket activation survives a service restart, how a privileged helper
can open a port and hand it to an unprivileged worker, and how zero-copy
designs move a shared memory region instead of a megabyte. TCP has nothing like
it, because a descriptor is meaningless in another kernel.

**3. Filesystem permissions as access control.** A socket is an inode: it has an
owner, a group and a mode, and it lives in a directory that has the same. On
Linux, connecting requires write permission on the socket, and reaching it at
all requires execute permission on every directory along the path. So "only
this user may call this service" is `chmod`, not a config file — and it is
enforced before your process is even woken up. See `unix(7)`; the portability
note is that some other Unixes historically ignored the socket's own mode,
which is why Ni locks the *directory* as well.

**4. Unreachable from the network, by construction.** The most under-rated
property. A loopback listener is one misconfiguration away from being remote:
bind `0.0.0.0` instead of `127.0.0.1`, publish a container port, add a
well-meaning proxy, get SSRF'd from a service that can reach your host, and
your "local only" service is answering the internet. A Unix socket has no
address a packet can carry. There is no bind mistake that exposes it and no
port-forward that reaches it. It can only be exposed by giving somebody
filesystem access — a decision that looks like a decision.

**5. Message boundaries, if you want them.** `SOCK_SEQPACKET` over `AF_UNIX`
gives connection-oriented, reliable, *ordered*, boundary-preserving datagrams —
TCP's guarantees plus "one send is one receive". TCP cannot offer it and UDP
cannot offer the reliability. gRPC does not use it (HTTP/2 wants a byte
stream), but it is often the right answer for a hand-rolled local protocol, and
`SOCK_DGRAM` over `AF_UNIX` is reliable too, unlike UDP.

### What a Unix socket costs you

Be able to argue the other side.

- **One host, forever.** There is no version of this that reaches another
  machine. A service that might ever be remote should not be UDS-only; the
  point of keeping the *same gRPC contract* is that swapping back is a flag,
  which is exactly what M5 is demonstrating.
- **Lifecycle is your problem.** `close()` on a listening Unix socket does
  **not** remove the file. The name persists, and the next `bind` on it fails
  with `EADDRINUSE` against a socket nobody is listening on. Every UDS service
  needs an answer to "who unlinks this?", and every one that crashes needs an
  answer to "what about now?". Checkpoint 8 does both.
- **108 bytes.** `sun_path` is a fixed-size array — 108 bytes on Linux, 104 on
  macOS. Long paths are not truncated; the `bind` fails. Deep temp directories,
  container mount points and per-test directories all reach this faster than
  you would think, and the error you get is not obviously about length.
- **Different tooling.** `ss -tlnp` will not show it; `ss -xl`, `lsof -U` and
  `/proc/net/unix` will. `tcpdump` cannot see a byte of it — for packet-level
  debugging you need `strace`, an eBPF tool like `sockdump`, or a proxy in the
  middle. Cheap gRPC clients often cannot reach it either: `grpcurl` supports
  `-unix`, but plenty of tools and every browser do not.
- **Namespaces cut both ways.** A socket in a container's filesystem is
  invisible outside it unless the path is bind-mounted, which is a feature for
  isolation and a chore for orchestration. Kubernetes network policies, service
  meshes and per-connection observability all key off IP traffic and see
  nothing here.
- **Portability.** `AF_UNIX` exists on Windows 10+ but without `SO_PEERCRED` or
  `SCM_RIGHTS`; the traditional Windows equivalent is a named pipe, which gRPC
  also supports on some stacks. Cross-platform code needs both paths.

### What does not change when you swap

This is the list post 6 is built on, and the reason M5's diff is as small as it
is:

- **Protobuf encoding.** Byte for byte identical. Serialization cost is a
  property of your messages, and Checkpoint 7 shows it is the largest single
  line item on a 1 MB payload.
- **HTTP/2.** Framing, stream multiplexing, HPACK header compression, flow
  control windows, `RST_STREAM` cancellation — all of it, unchanged, on top of
  a byte stream that happens not to be TCP. gRPC still needs an `:authority`
  header, which is why Checkpoint 1's connector has a fake URI in it.
- **The failure model.** Deadlines still fire, `grpc-timeout` still travels in
  metadata, the server can still die mid-call, and you still cannot tell
  whether the work happened. M3's whole taxonomy — timeout, crash, protocol —
  survives verbatim, which Checkpoint 5 asserts and Checkpoint 8 demonstrates.
- **Status codes.** Same codes, same meanings. Only the text inside a transport
  error changes, and Checkpoint 8 has the table.
- **Your service definition.** One `ni.proto`, two transports, zero
  regenerated stubs.

### Who is already doing this

Useful for the blog post, and useful for reassuring yourself that this is not
an exotic choice:

| System | Socket | Protocol |
|---|---|---|
| Docker daemon | `/var/run/docker.sock` | HTTP |
| containerd | `/run/containerd/containerd.sock` | **gRPC** |
| Kubernetes CRI (containerd, CRI-O) | `unix:///run/containerd/containerd.sock` | **gRPC** |
| kubelet device plugins | `/var/lib/kubelet/device-plugins/kubelet.sock` | **gRPC** |
| systemd | any, plus socket activation via `LISTEN_FDS` | anything |
| PostgreSQL | `/var/run/postgresql/.s.PGSQL.5432` | its own |
| MySQL / MariaDB | `/var/run/mysqld/mysqld.sock` | its own |
| X11 | `/tmp/.X11-unix/X0` | X protocol |
| Envoy | admin socket, hot-restart socket | its own + gRPC |

The pattern is consistent: **a local plugin boundary between processes that
trust the kernel to say who is who**. Which is precisely what a Ni bot is.

### The abstract namespace, for completeness

On Linux, if `sun_path` starts with a null byte, the rest is a name in an
abstract namespace with no filesystem presence at all:

```
\0ni-bot-a
```

It vanishes when the last reference closes — no stale files, no cleanup, no
`EADDRINUSE` after a crash — and it ignores filesystem permissions entirely,
which means anything in the same network namespace can connect. So it swaps
"remember to unlink" for "your only access control is namespace membership".
Linux-only, and not what Ni uses, but worth recognising when you see it in
somebody else's `strace`.

### Side by side

| | loopback TCP | Unix domain socket |
|---|---|---|
| Address | `127.0.0.1:<port>` | `/path/to.sock` |
| Allocated by | kernel (`:0` gets you one) | you |
| Cleaned up by | kernel | you |
| Reachable off-box | one misconfiguration away | never |
| Peer identity | an address | pid, uid, gid, from the kernel |
| Can pass a file descriptor | no | yes |
| Access control | firewall rules, bind address | `chmod`, `chown`, directory modes |
| Name length limit | none that matters | 108 bytes, hard |
| Visible to | `ss -tln`, `tcpdump`, `netstat` | `ss -xl`, `lsof -U`, `/proc/net/unix` |
| Protocol overhead | TCP + IP state machines | a copy into the peer's queue |
| gRPC support | universal | universal in the spec; manual in tonic |

---

## A small Rust map for M5

M2's, M3's and M4's maps still apply. These are the new shapes, and there are
only five of them — M5 is a small milestone in Rust terms and a large one in
argument terms.

```rust
use tower::service_fn;

let connector = service_fn(move |_: Uri| async move { /* -> Result<Io, Error> */ });
```

A `tower::Service` is "a thing that turns a request into a future of a
response" — the trait every layer of tonic, hyper and axum is built on.
`service_fn` wraps a closure into one, the way `Iterator`-returning closures get
wrapped by `std::iter::from_fn`. tonic's client asks its connector for a
connection *per URI*; ours ignores the URI and connects to a fixed path. The
`move` on both the closure and the inner `async` block matters: the closure is
called many times, so it can only *clone* the captured path, and the future it
returns must own its copy.

```rust
use hyper_util::rt::TokioIo;

TokioIo::new(UnixStream::connect(path).await?)
```

Pure adapter. hyper 1.0 stopped depending on tokio's `AsyncRead`/`AsyncWrite`
and defined its own (`hyper::rt::Read`/`Write`), so every custom transport needs
one wrapper to bridge them. If you see a trait error mentioning
`hyper::rt::Read is not implemented for UnixStream`, this is the missing line.

```rust
struct SocketFile(PathBuf);

impl Drop for SocketFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}
```

The **RAII guard**: a value whose only purpose is that its destructor runs.
Rust guarantees `drop` on every path out of a scope — `return`, `?`, a panic
unwinding — which makes it the right tool for "this file must not outlive this
function". It is not magic, though, and Checkpoint 8 shows exactly what it does
not cover: `SIGKILL` runs no destructors, so a layer above still needs to be
able to clean up.

```rust
impl std::str::FromStr for Transport {
    type Err = String;
    fn from_str(value: &str) -> Result<Self, String> { /* ... */ }
}

#[arg(long, default_value = "tcp")]
transport: Transport,
```

clap derives argument parsing for any type implementing `FromStr` where the
error is `Display`. So the flag validates itself: `--transport smoke-signals`
fails in `Cli::parse()` with a decent message, before any socket exists, and
the rest of the program only ever handles a two-variant enum. Parse, don't
validate.

```rust
use std::os::unix::fs::PermissionsExt as _;

std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))?;
```

`std::fs::Permissions` is portable and nearly featureless; the Unix-specific
`from_mode`/`mode` live on an extension trait you have to import. The
`as _` form imports the trait for its methods without binding a name you then
do not use.

One thing that is *not* new but becomes load-bearing here:

```rust
pub async fn dial(endpoint: &str, timeout: Duration) -> Result<BotServiceClient<Channel>>
```

Both transports return `BotServiceClient<Channel>`. `Channel` is tonic's
type-erased connection handle, which is why 700 lines of `match_runner.rs` can
be transport-agnostic without a single generic parameter. When you design the
seam yourself, this is the shape to aim for: erase the difference at the lowest
possible layer and let everything above it stay ignorant.

---

## Checkpoint 0 — prove M4 is healthy

```sh
cargo test --workspace
cargo build --workspace
./target/debug/ni-engine run \
  --bot-a target/debug/ni-bot \
  --bot-b target/debug/ni-bot \
  --quiet
```

Expected: tests pass, and the match prints a result.

Now try the thing M5 exists to make possible, and watch it fail:

```sh
./target/debug/ni-bot --listen unix:///tmp/ni-a.sock
```

```
Error: listen target must begin with tcp://
```

That error is M4's `serve` refusing anything it does not recognise, which is the
correct behaviour for M4 and the first line to change.

Before moving on, answer one question from the M4 code: **if you gave the bot a
Unix socket and it bound one, how would the engine find out?** The answer in M4
is that it could not — `process.rs` parses `LISTENING tcp://` and would reject
its own bot's readiness line. That is the second thing to change, and it is the
reason the scheme goes on the wire in Checkpoint 3.

---

## Checkpoint 1 — the transport module, with no policy in it

Two dependencies and one new file. Nothing in this checkpoint knows what a match
is.

### 1.1 Add the dependencies to the workspace `Cargo.toml`

In `[workspace.dependencies]`, after `prost`:

```toml
tower = { version = "0.5", features = ["util"] }
hyper-util = { version = "0.1", features = ["tokio"] }
```

Also add `ni-engine` itself as a workspace dependency — Checkpoint 6's new crate
will use the engine's spawn code, and declaring the path once here keeps the
crate manifests uniform:

```toml
ni-engine = { path = "crates/ni-engine" }
```

and register the crate you will create later, in `members`:

```toml
    "crates/ni-bench",
```

**Why these two crates and not just tonic.** `service_fn` is the `tower`
combinator that turns a closure into a `Service`; `TokioIo` is the
`hyper-util` adapter between tokio's IO traits and hyper's. Both are already in
your `Cargo.lock` as transitive dependencies of tonic — you are promoting them
to direct dependencies, not adding new code to the build. The features matter:
`tower`'s `util` gates `service_fn`, and `hyper-util`'s `tokio` gates the whole
`rt` module.

Then in `crates/ni-engine/Cargo.toml`, after `prost`:

```toml
hyper-util = { workspace = true }
tower = { workspace = true }
```

Cargo will complain that `crates/ni-bench/Cargo.toml` does not exist. That is
expected until Checkpoint 6; if you would rather stay green in between, add the
`members` line then instead.

### 1.2 Create `crates/ni-engine/src/transport.rs`

Start with the vocabulary — three types, no I/O:

```rust
//! Where a bot listens, and how the engine dials it.
//!
//! M5's whole surface area. Two transports, one service definition:
//!
//! * **loopback TCP** — `tcp://127.0.0.1:0`, the kernel picks a port, the bot
//!   prints the one it got.
//! * **Unix domain socket** — `unix:///run/ni/<match>/a.sock`, the *engine*
//!   picks the path, because a path is not a port: nothing allocates one for
//!   you, and nothing cleans one up either.
//!
//! Everything above this module — deadlines, statuses, spans, the match loop —
//! is written against `BotServiceClient<Channel>` and does not know which of
//! the two it got. That is the claim M5 exists to test.

use std::{
    path::{Path, PathBuf},
    time::Duration,
};

use anyhow::{bail, Context, Result};
use hyper_util::rt::TokioIo;
use ni_proto::ni::v1::bot_service_client::BotServiceClient;
use tokio::net::UnixStream;
use tonic::transport::{Channel, Endpoint, Uri};
use tower::service_fn;

/// `sockaddr_un.sun_path` is a fixed-size array: 108 bytes on Linux, 104 on
/// macOS. A path that does not fit is not truncated — `bind` fails. Ni checks
/// the length itself so the error names the real problem.
pub const SUN_PATH_MAX: usize = 100;

/// Which socket family the match runs over.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum Transport {
    #[default]
    Tcp,
    Unix,
}

impl Transport {
    pub fn as_str(self) -> &'static str {
        match self {
            Transport::Tcp => "tcp",
            Transport::Unix => "unix",
        }
    }
}

impl std::str::FromStr for Transport {
    type Err = String;

    fn from_str(value: &str) -> std::result::Result<Self, Self::Err> {
        match value {
            "tcp" => Ok(Transport::Tcp),
            "unix" | "uds" => Ok(Transport::Unix),
            other => Err(format!("unknown transport {other:?}; expected tcp or unix")),
        }
    }
}

/// The listen target the engine hands a bot on argv.
///
/// The asymmetry is the interesting part. For TCP the engine says "port 0,
/// you tell me"; for UDS the engine says "this exact path". Ports come from
/// an allocator, paths come from whoever is willing to name one.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Listen {
    Tcp,
    Unix(PathBuf),
}

impl Listen {
    /// What goes after `--listen`.
    pub fn argv(&self) -> String {
        match self {
            Listen::Tcp => "tcp://127.0.0.1:0".to_string(),
            // Three slashes: `unix://` + an absolute path that starts with one.
            Listen::Unix(path) => format!("unix://{}", path.display()),
        }
    }
}
```

**Why `SUN_PATH_MAX` is 100 and not 108.** The kernel's limit includes the
terminating null and, on Linux, the 2-byte family field is *not* part of
`sun_path` — but the exact arithmetic differs between platforms, and being 8
bytes conservative costs nothing while turning a confusing `bind` failure into a
sentence. Guessing generously here would be the wrong direction: too strict
produces a clear error, too loose produces `EINVAL` from a syscall.

**Why two enums that look alike.** `Transport` is a *choice*, made once, on the
command line. `Listen` is an *instruction*, produced per bot, that already
contains the specific path. Keeping them separate is what stops
`Transport::Unix` from having to carry an `Option<PathBuf>` that is `None` half
the time — a shape which then forces an `unwrap` at every use.

### 1.3 Add the directory that owns the sockets

Append to `transport.rs`:

```rust
/// A directory of bot sockets that deletes itself.
///
/// A TCP port is reclaimed by the kernel when the last socket closes. A socket
/// *file* is not: it is a name in a filesystem, and names outlive processes.
/// Someone has to own the cleanup, and the engine is the only participant that
/// knows when a match is over.
#[derive(Debug)]
pub struct SocketDir {
    path: PathBuf,
}

impl SocketDir {
    /// Create `<base>/<match_id>/`, owner-only.
    ///
    /// `0o700` on the directory is the load-bearing permission. A socket file
    /// created by `bind` gets its mode from the process umask, and on some
    /// Unixes the socket's own mode is not even consulted on `connect` — but
    /// every Unix checks execute permission on the directories along the path.
    /// Lock the directory and the socket inside it is unreachable, whatever
    /// its own mode says.
    pub fn create(base: &Path, match_id: &str) -> Result<Self> {
        let safe: String = match_id
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
            .collect();

        let path = base.join(safe);

        std::fs::create_dir_all(&path)
            .with_context(|| format!("could not create socket directory {}", path.display()))?;

        set_owner_only(&path)?;

        Ok(Self { path })
    }

    /// The default socket base: `$XDG_RUNTIME_DIR/ni`, falling back to
    /// `/tmp/ni-<uid>`.
    ///
    /// The design doc says `/run/ni`, which needs root. `$XDG_RUNTIME_DIR` is
    /// the unprivileged equivalent — per-user, already `0700`, and on a tmpfs
    /// that the session manager empties at logout.
    pub fn default_base() -> PathBuf {
        match std::env::var_os("XDG_RUNTIME_DIR") {
            Some(dir) if !dir.is_empty() => PathBuf::from(dir).join("ni"),
            // SAFETY: `getuid` reads a process property and cannot fail.
            _ => PathBuf::from(format!("/tmp/ni-{}", unsafe { libc::getuid() })),
        }
    }

    /// A socket path for one bot, length-checked before anything tries to bind it.
    pub fn socket(&self, label: &str) -> Result<PathBuf> {
        let path = self.path.join(format!("{label}.sock"));
        let length = path.as_os_str().len();

        if length > SUN_PATH_MAX {
            bail!(
                "socket path is {length} bytes, over the {SUN_PATH_MAX}-byte limit: {} \
                 — pass a shorter --socket-dir",
                path.display()
            );
        }

        Ok(path)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for SocketDir {
    fn drop(&mut self) {
        // Best effort by definition: a directory we cannot remove is a
        // diagnostic, not a match result.
        if let Err(error) = std::fs::remove_dir_all(&self.path) {
            if error.kind() != std::io::ErrorKind::NotFound {
                tracing::warn!(
                    path = %self.path.display(),
                    %error,
                    "could not remove socket directory"
                );
            }
        }
    }
}

fn set_owner_only(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt as _;

    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
        .with_context(|| format!("could not restrict {} to its owner", path.display()))
}
```

Four decisions in that block, each worth its lines:

**The match id is sanitised, not trusted.** `match_id` reaches the engine from
`--match-id` and becomes a *path component*. Any character that is not
`[A-Za-z0-9]` becomes `-`, which removes `/`, `..` and every other way a name
turns into a directory traversal. This is a small function doing a security job:
a match id of `../../etc` would otherwise be a `remove_dir_all` target in
`Drop`. Whenever external input becomes a path, this conversion belongs at the
boundary, and there is exactly one boundary here.

**`Drop` does the deletion, so every exit path cleans up.** Not just the happy
one: a `?` in the middle of `run`, a bot that fails to start, a panic in the
match loop. The engine's `main` holds the `SocketDir` for the whole match, which
means the *type system* is tracking the lifetime of a filesystem resource.

**`Drop` cannot fail, so it warns.** `Drop::drop` returns `()`; there is
nowhere for an error to go. `tracing::warn!` is the honest response, and
`NotFound` is filtered out because "already gone" is success.

**`unsafe` for `getuid`, and only for that.** `libc::getuid` is `unsafe` because
every `libc` function is, not because this call is risky: it reads a property of
the current process and cannot fail or violate any invariant. That is the whole
safety comment, and the whole reason the `unsafe` block is one line long.

### 1.4 Add the dialling half

Append to `transport.rs`:

```rust
/// Dial an endpoint the bot printed on its readiness line.
///
/// Both arms end in the same type. That is not a convenience — it is the
/// milestone: `Channel` is tonic's transport-erased handle, so the generated
/// client, the deadlines, the statuses and the metadata are identical either
/// way.
pub async fn dial(endpoint: &str, connect_timeout: Duration) -> Result<BotServiceClient<Channel>> {
    if let Some(address) = endpoint.strip_prefix("tcp://") {
        dial_tcp(address, connect_timeout).await
    } else if let Some(path) = endpoint.strip_prefix("unix://") {
        dial_unix(Path::new(path), connect_timeout).await
    } else {
        bail!("endpoint must begin with tcp:// or unix://, got {endpoint:?}")
    }
}

async fn dial_tcp(address: &str, connect_timeout: Duration) -> Result<BotServiceClient<Channel>> {
    let channel = Endpoint::from_shared(format!("http://{address}"))?
        .connect_timeout(connect_timeout)
        .connect()
        .await
        .with_context(|| format!("could not connect to tcp://{address}"))?;

    Ok(BotServiceClient::new(channel))
}

/// The UDS dance, which looks stranger than it is.
///
/// `Endpoint` insists on a URI because HTTP/2 needs an `:authority`
/// pseudo-header and gRPC puts the service's host there. A Unix socket has no
/// host, so the URI below is a placeholder that is never resolved: the
/// connector ignores it and connects to the path instead. It still has to be
/// syntactically valid, and it still ends up in the `:authority` header the
/// bot receives.
async fn dial_unix(path: &Path, connect_timeout: Duration) -> Result<BotServiceClient<Channel>> {
    let path = path.to_path_buf();
    let target = path.clone();

    let channel = Endpoint::try_from("http://ni.invalid")?
        .connect_timeout(connect_timeout)
        // `service_fn` turns a closure into a `tower::Service`. tonic asks the
        // connector for a connection per `Uri`; this one throws the `Uri` away.
        .connect_with_connector(service_fn(move |_: Uri| {
            let path = path.clone();
            async move {
                // `TokioIo` adapts a tokio `AsyncRead`/`AsyncWrite` to hyper's
                // own IO traits. It is pure plumbing — hyper 1.0 stopped
                // depending on tokio's traits directly, so every custom
                // transport needs this wrapper.
                Ok::<_, std::io::Error>(TokioIo::new(UnixStream::connect(path).await?))
            }
        }))
        .await
        .with_context(|| format!("could not connect to unix://{}", target.display()))?;

    Ok(BotServiceClient::new(channel))
}
```

This is the function everybody gets wrong the first time, so here it is line by
line.

**`Endpoint::try_from("http://ni.invalid")`.** A URI is mandatory and, for a
Unix socket, meaningless. tonic will not resolve it — `connect_with_connector`
replaces the DNS-and-TCP connector entirely — but it *will* put the host in the
HTTP/2 `:authority` pseudo-header, because that is what gRPC does with a target.
`.invalid` is the RFC 2606 reserved TLD, so if this string ever does reach a
resolver it is guaranteed to fail rather than to reach somebody. `http://` is
required to mean "no TLS", which is separate from the socket family: TLS over
UDS is possible and, here, pointless.

**`service_fn(move |_: Uri| { … })`.** The connector is a `tower::Service<Uri>`.
tonic calls it every time it needs a fresh connection — at start-up and again
after a reconnect — so it cannot consume the path. Hence: the outer `move`
captures the `PathBuf`, and each call `clone`s it into the future. The `: Uri`
annotation is needed because the closure's argument type is otherwise
unconstrained.

**Why `path` is cloned twice.** Once into the closure (the connector owns a
copy for its whole life) and once per call inside it. The extra `target` binding
exists only so the error message can name the path *after* the closure has
taken ownership of the original. This is one of those places where Rust's move
semantics force you to be explicit about a thing every other language leaves
ambiguous — and where a `String` is cheap enough that being explicit costs
nothing.

**`Ok::<_, std::io::Error>(…)`.** The turbofish pins the error type of the
future. Without it, the compiler cannot infer what `?` on `UnixStream::connect`
should produce, and the error names a half-dozen unrelated trait bounds. This
line, and its absence, is the single most common cause of "why won't my tonic
UDS client compile".

**What is *not* here.** No timeout logic (`connect_timeout` is `Endpoint`'s
job), no retry loop (`BotProcess` already owns start-up retry), and no `Channel`
configuration that differs between the two arms. If the two arms had diverged in
any way beyond the socket, M5's central claim would already be weaker.

Worth knowing: this manual connector is a **tonic** limitation, not a gRPC one.
The gRPC name-resolution spec defines `unix:` and `unix-abstract:` target
schemes, and grpc-go, grpc-java and grpc-c++ will parse
`unix:///run/ni/a.sock` and do all of the above for you. In Rust you write the
twelve lines yourself, once.

### 1.5 Append the tests to `transport.rs`

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_transport_parses_from_its_flag_value() {
        assert_eq!("tcp".parse::<Transport>(), Ok(Transport::Tcp));
        assert_eq!("unix".parse::<Transport>(), Ok(Transport::Unix));
        assert_eq!("uds".parse::<Transport>(), Ok(Transport::Unix));
        assert!("smoke-signals".parse::<Transport>().is_err());
    }

    #[test]
    fn tcp_asks_the_kernel_for_a_port_and_unix_names_a_path() {
        assert_eq!(Listen::Tcp.argv(), "tcp://127.0.0.1:0");
        assert_eq!(
            Listen::Unix(PathBuf::from("/run/ni/m5/a.sock")).argv(),
            "unix:///run/ni/m5/a.sock"
        );
    }

    #[test]
    fn a_socket_directory_is_owner_only_and_removes_itself() {
        use std::os::unix::fs::PermissionsExt as _;

        let base = std::env::temp_dir().join("ni-m5-socketdir-test");
        let path;

        {
            let dir = SocketDir::create(&base, "m5/demo match").expect("directory is created");
            path = dir.path().to_path_buf();

            assert!(path.is_dir());
            assert_eq!(
                std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o700
            );
            // The match id is sanitised: one component, no separators.
            assert_eq!(path.file_name().unwrap(), "m5-demo-match");
        }

        assert!(!path.exists(), "dropping the guard removes the directory");
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn an_over_long_socket_path_is_refused_before_bind() {
        let base = std::env::temp_dir().join("ni-m5-longpath-test");
        let dir = SocketDir::create(&base, &"x".repeat(120)).expect("directory is created");

        let error = dir.socket("a").expect_err("the path is too long");
        assert!(error.to_string().contains("over the 100-byte limit"));

        let _ = std::fs::remove_dir_all(&base);
    }

    #[tokio::test]
    async fn an_unknown_scheme_is_rejected_without_touching_the_network() {
        let error = dial("ni://somewhere", Duration::from_millis(50))
            .await
            .expect_err("no such scheme");

        assert!(error
            .to_string()
            .contains("must begin with tcp:// or unix://"));
    }
}
```

Read the third test again: the assertion after the closing brace is the
interesting one. The inner scope drops the `SocketDir`, and the test then checks
the filesystem. That is how you test a `Drop` impl — introduce a scope, and
assert about the world outside it. `path` is declared before the block and
assigned inside it precisely so it survives the drop.

The `"m5/demo match"` input is deliberately hostile: a slash and a space. If
sanitising were missing, `create` would silently make a nested
`ni-m5-socketdir-test/m5/demo match` and the `file_name` assertion would fail —
which is a much better failure than discovering the same bug via `Drop` deleting
something else.

### 1.6 Register the module

In `crates/ni-engine/src/lib.rs`:

```rust
pub mod transport;
```

```rust
pub use transport::{Listen, SocketDir, Transport};
```

### 1.7 Run the checkpoint

```sh
cargo fmt
cargo test -p ni-engine --lib transport
```

Expected: 5 passed.

```
running 5 tests
test transport::tests::a_transport_parses_from_its_flag_value ... ok
test transport::tests::a_socket_directory_is_owner_only_and_removes_itself ... ok
test transport::tests::an_over_long_socket_path_is_refused_before_bind ... ok
test transport::tests::an_unknown_scheme_is_rejected_without_touching_the_network ... ok
test transport::tests::tcp_asks_the_kernel_for_a_port_and_unix_names_a_path ... ok
```

Five tests over a filesystem and an enum, and not a single gRPC call. That is
the same trick M3 played with `policy.rs`: get the decisions into a module with
no I/O in it, and the I/O becomes the boring part.

---

## Checkpoint 2 — a bot that can listen on either

M4's `serve` is 45 lines and handles one family. The new one handles two,
protects a file, refuses to steal a live socket, and can say who called it.

### 2.1 Replace `crates/ni-bot/src/server.rs`

```rust
//! The listener half of a bot: bind what the engine asked for, announce it,
//! serve until `SIGTERM`.
//!
//! From M5 there are two kinds of "what the engine asked for", and the
//! difference between them is almost entirely about *names*. A TCP listener
//! borrows a port from an allocator and gives it back on close. A Unix
//! listener creates a file, and a file has to be created carefully, protected
//! deliberately, and deleted by somebody.

use std::{
    io::Write as _,
    path::{Path, PathBuf},
};

use anyhow::{bail, Context, Result};
use ni_proto::ni::v1::bot_service_server::{BotService, BotServiceServer};
use tokio::{
    net::{TcpListener, UnixListener, UnixStream},
    signal::unix::{signal, SignalKind},
};
use tokio_stream::wrappers::{TcpListenerStream, UnixListenerStream};
use tonic::{transport::server::UdsConnectInfo, transport::Server, Request};

pub async fn serve<S>(listen: &str, service: S) -> Result<()>
where
    S: BotService,
{
    if let Some(address) = listen.strip_prefix("tcp://") {
        serve_tcp(address, service).await
    } else if let Some(path) = listen.strip_prefix("unix://") {
        serve_unix(Path::new(path), service).await
    } else {
        bail!("listen target must begin with tcp:// or unix://, got {listen:?}")
    }
}

async fn serve_tcp<S>(bind_address: &str, service: S) -> Result<()>
where
    S: BotService,
{
    let listener = TcpListener::bind(bind_address).await?;
    let local_address = listener.local_addr()?;

    announce(&format!("tcp://{local_address}"))?;

    Server::builder()
        .add_service(BotServiceServer::new(service))
        .serve_with_incoming_shutdown(TcpListenerStream::new(listener), terminated())
        .await?;

    Ok(())
}

async fn serve_unix<S>(path: &Path, service: S) -> Result<()>
where
    S: BotService,
{
    clear_stale_socket(path).await?;

    let listener = UnixListener::bind(path)
        .with_context(|| format!("could not bind unix://{}", path.display()))?;

    // `bind` created the file with `0666 & ~umask`, whatever the umask
    // happens to be. Narrow it explicitly. Linux enforces this on `connect`;
    // some other Unixes do not, which is why the engine also locks the
    // containing directory.
    restrict_to_owner(path)?;

    // The guard, not the listener, owns the name in the filesystem. Closing a
    // Unix listener does not remove its socket file — the next bind would then
    // fail with EADDRINUSE against a socket nobody is listening on.
    let _socket_file = SocketFile(path.to_path_buf());

    announce(&format!("unix://{}", path.display()))?;

    Server::builder()
        .add_service(BotServiceServer::new(service))
        .serve_with_incoming_shutdown(UnixListenerStream::new(listener), terminated())
        .await?;

    Ok(())
}

/// The readiness line the engine parses, on stdout, flushed.
///
/// It carries the scheme as well as the address, so the engine dials what the
/// bot actually bound rather than what it hoped for.
fn announce(endpoint: &str) -> Result<()> {
    println!("LISTENING {endpoint}");
    std::io::stdout().flush()?;
    Ok(())
}
```

**The symmetry is the design.** `serve_with_incoming_shutdown` takes a *stream
of connections*, so tonic never learns what kind of listener produced them:
`TcpListenerStream` and `UnixListenerStream` are both `Stream<Item = io::Result<C>>`
where `C` implements tonic's `Connected` trait. The two functions differ by six
lines, and every one of those lines is about the filesystem rather than about
gRPC. If you ever add a third transport — a `socketpair`, a pipe, an in-process
duplex — this is the shape it plugs into.

**`_socket_file` and the leading underscore.** The binding exists solely to keep
the guard alive until the end of the function. Naming it `_socket_file` rather
than `_` is essential: `let _ = SocketFile(…)` drops it *immediately*, deleting
the socket before a single connection arrives. This is a real bug that compiles,
runs, and produces a bot that appears to work until something tries to connect.

### 2.2 Add the peer-identity helper

Append to `server.rs`:

```rust
/// Who is on the other end of this call?
///
/// This is the one place where the two transports genuinely differ in what
/// they can *tell* you, and it is the strongest argument for UDS that has
/// nothing to do with speed.
///
/// * Over TCP you get an address and a port. `127.0.0.1` proves the packet
///   came from this host and nothing more — any process, any user, any
///   container sharing the network namespace could have sent it.
/// * Over a Unix socket the kernel attaches the peer's pid, uid and gid to the
///   connection (`SO_PEERCRED`). The peer cannot lie about them: it never
///   supplies them, the kernel does, from the process it knows made the
///   `connect` call.
pub fn describe_peer<T>(request: &Request<T>) -> String {
    if let Some(info) = request.extensions().get::<UdsConnectInfo>() {
        return match info.peer_cred {
            Some(credentials) => format!(
                "unix pid={} uid={} gid={}",
                credentials
                    .pid()
                    .map(|pid| pid.to_string())
                    .unwrap_or_else(|| "?".to_string()),
                credentials.uid(),
                credentials.gid()
            ),
            None => "unix (credentials unavailable)".to_string(),
        };
    }

    match request.remote_addr() {
        Some(address) => format!("tcp {address}"),
        None => "unknown".to_string(),
    }
}
```

**How the credentials got there.** tonic's `Connected` trait has an associated
`ConnectInfo` type; its impl for `tokio::net::UnixStream` produces a
`UdsConnectInfo { peer_addr, peer_cred }` by calling `UnixStream::peer_cred()`,
which is `getsockopt(SO_PEERCRED)`. tonic inserts that value into every
request's **extensions** — the per-request typed side-channel that also carries
things like the timeout and the trace context. So `extensions().get::<T>()` is a
typed downcast: `Some` if this request arrived over a Unix socket, `None`
otherwise. That `Option` *is* the transport check, which is why this function
needs no `Transport` parameter and the bot needs no configuration.

**Why `pid()` returns an `Option`.** `UCred::pid` is `Option<pid_t>` because not
every platform reports it (macOS's `LOCAL_PEERCRED` gives uid and gid only). The
`unwrap_or_else(|| "?")` keeps the log line shaped the same everywhere instead
of hiding the whole line behind a `cfg`.

**Why this is a diagnostic and not yet a policy.** Ni logs the credentials; it
does not refuse a connection whose uid is wrong. That would be the right next
step for a daemon accepting bots it did not spawn (M6), and it is deliberately
not the right step for M5, where the engine spawned every bot itself and
`--socket-dir` is already `0700`. Log first, enforce when there is something to
enforce against.

### 2.3 Add the stale-socket handling and the guard

Append to `server.rs`:

```rust
/// A socket file that unlinks itself.
struct SocketFile(PathBuf);

impl Drop for SocketFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// Remove a socket file left behind by a dead process — and only that.
///
/// The naive version (`let _ = remove_file(path)`) is a footgun: run two bots
/// on one path and the second silently steals the name, leaving the first
/// holding a socket no client can reach. So: try to connect first. Somebody
/// answering means the socket is live and this is a configuration error;
/// `ECONNREFUSED` means the file outlived its process and can go.
async fn clear_stale_socket(path: &Path) -> Result<()> {
    if !path.exists() {
        return Ok(());
    }

    match UnixStream::connect(path).await {
        Ok(_) => bail!(
            "another process is already listening on unix://{}",
            path.display()
        ),
        Err(error) if error.kind() == std::io::ErrorKind::ConnectionRefused => {
            tracing::warn!(path = %path.display(), "removing a stale socket file");
            std::fs::remove_file(path)
                .with_context(|| format!("could not remove stale socket {}", path.display()))
        }
        Err(error) => Err(error)
            .with_context(|| format!("could not probe existing socket {}", path.display())),
    }
}

fn restrict_to_owner(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt as _;

    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
        .with_context(|| format!("could not restrict {} to its owner", path.display()))
}

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

`clear_stale_socket` is worth dwelling on, because "just delete it" is what
almost every tutorial does and it is wrong in a way that only shows up under
concurrency.

The stale-socket problem exists because **a socket file's lifetime is not tied
to the socket**. `bind` creates the inode; `close` destroys the socket and leaves
the inode. So after a crash you have a name that `bind` refuses (`EADDRINUSE`)
and `connect` refuses (`ECONNREFUSED`) — useless to everybody, and removable
only by whoever notices.

The distinction this function draws is the one the kernel already gives you for
free:

| What `UnixStream::connect` returns | What it means | What to do |
|---|---|---|
| `Ok(_)` | a process is listening right now | **stop** — two bots on one path is a bug in the caller |
| `Err(ECONNREFUSED)` | the inode exists, nobody is listening | remove it and bind |
| `Err(ENOENT)` | (filtered earlier by `path.exists()`) | bind |
| `Err(EACCES)` or anything else | you cannot tell | report it, do not delete |

The last row is why the final arm returns the error rather than assuming
staleness. Deleting a file you were not allowed to inspect is exactly the kind of
thing that makes a service occasionally destroy something it does not own.

`Ok(_)` immediately drops the probe connection, which is polite and harmless:
the live bot sees a connection open and close with no HTTP/2 preface, logs
nothing at INFO, and carries on.

### 2.4 Log the peer in `crates/ni-bot/src/lib.rs`

Change the export:

```rust
pub use server::{describe_peer, serve};
```

and in `identify`:

```rust
        let span = ni_telemetry::server_span("ni.v1.BotService/Identify", request.metadata());

        // The handshake is where a bot learns who it is talking to. Over a
        // Unix socket that is a kernel-verified fact; over TCP it is an
        // address anybody on the host could have connected from.
        let peer = describe_peer(&request);

        async move {
            info!(protocol = PROTOCOL_VERSION, %peer, "identified");
```

Two small Rust points. `describe_peer(&request)` has to run **before** the
`async` block, because the block moves what it captures and `request` would
otherwise be borrowed after the move — the same `borrow of moved value` error
M4's troubleshooting section describes. And the block becomes `async move` now
that it captures the owned `peer` `String`; `%peer` records it via `Display`.

`Identify` is the right handler for this. It is the one call every bot receives
exactly once, at the start, which makes it the handshake in practice as well as
in name — and one line per bot per match is a log volume you can live with.

### 2.5 Append the tests to `server.rs`

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_missing_socket_needs_no_clearing() {
        let path = std::env::temp_dir().join("ni-m5-absent.sock");
        let _ = std::fs::remove_file(&path);

        assert!(clear_stale_socket(&path).await.is_ok());
    }

    #[tokio::test]
    async fn a_stale_socket_file_is_removed() {
        let path = std::env::temp_dir().join("ni-m5-stale.sock");
        let _ = std::fs::remove_file(&path);

        // Bind and drop: the listener is gone, the name is not.
        {
            let _listener = UnixListener::bind(&path).expect("bind");
        }
        assert!(path.exists(), "closing a listener leaves the file behind");

        clear_stale_socket(&path)
            .await
            .expect("stale file is cleared");
        assert!(!path.exists());
    }

    #[tokio::test]
    async fn a_live_socket_is_left_alone_and_reported() {
        let path = std::env::temp_dir().join("ni-m5-live.sock");
        let _ = std::fs::remove_file(&path);

        let _listener = UnixListener::bind(&path).expect("bind");

        let error = clear_stale_socket(&path)
            .await
            .expect_err("a live socket is not ours to delete");

        assert!(error.to_string().contains("already listening"));
        assert!(path.exists());

        let _ = std::fs::remove_file(&path);
    }
}
```

The middle test doubles as documentation of the kernel's behaviour: bind inside
a block, leave the block, and assert the file is still there. If Linux ever
started unlinking sockets on close, this test would tell you — which is better
than a comment claiming it does not.

### 2.6 Run the checkpoint

```sh
cargo fmt
cargo test -p ni-bot
cargo build --workspace
```

Then look at the difference the whole checkpoint was for:

```sh
./target/debug/ni-engine run --bot-a target/debug/ni-bot --bot-b target/debug/ni-bot \
  --quiet --no-match-log 2>&1 | grep identified
```

```
INFO rpc{…/Identify…}: identified protocol=1 peer=tcp 127.0.0.1:52290
```

You cannot run the Unix version yet — the engine still hard-codes TCP, which is
the next two checkpoints. Note what that TCP line does and does not tell you:
there is a peer, it is on this host, and that is the end of the knowledge.
Checkpoint 4 prints the other one.

---

## Checkpoint 3 — the engine dials whatever the bot bound

Three changes to `crates/ni-engine/src/process.rs`, all in `spawn_with_args`.

### 3.1 Take a `Listen` and use it

Add the import:

```rust
use crate::transport::{dial, Listen};
```

Change both signatures to accept the listen target:

```rust
    pub async fn spawn(path: &Path, label: &str, listen: &Listen) -> Result<Self> {
        Self::spawn_with_args::<&OsStr>(path, label, &[], listen).await
    }

    pub async fn spawn_with_args<S: AsRef<OsStr>>(
        path: &Path,
        label: &str,
        extra_args: &[S],
        listen: &Listen,
    ) -> Result<Self> {
```

and pass it to the child instead of the hard-coded string:

```rust
        let mut child = Command::new(path)
            .arg("--listen")
            .arg(listen.argv())
            .args(extra_args)
```

### 3.2 Parse the scheme off the readiness line

Replace M4's prefix-stripping:

```rust
            // The readiness line carries the endpoint *including its scheme*,
            // so the engine never has to remember which transport it asked
            // for — it dials whatever the bot says it bound.
            let endpoint = ready
                .strip_prefix("LISTENING ")
                .with_context(|| format!("invalid bot readiness line: {ready:?}"))?
                .to_string();

            let client = tokio::time::timeout(STARTUP_DEADLINE, dial(&endpoint, STARTUP_DEADLINE))
                .await
                .with_context(|| format!("{label} did not accept a connection at {endpoint}"))?
                .with_context(|| format!("failed to connect to {label} at {endpoint}"))?;

            Ok((client, endpoint, lines))
```

M4 had `strip_prefix("LISTENING tcp://")` and then built `format!("http://{address}")`
at the dial site. Both of those assumptions are now gone, and the field
`BotProcess::endpoint` changes meaning slightly: it holds a fully-qualified
endpoint (`unix:///tmp/ni-0/m5/a.sock`) rather than a bare address, which is
strictly more useful in the `bot identified` log line.

**Why the bot reports the scheme rather than the engine remembering it.** The
engine told the bot what to bind, so it *could* keep that knowledge and skip the
parse. Two reasons not to. First, a bot is another team's binary: one that
ignores `--listen unix://…` and binds TCP anyway is a bug you want to see in the
readiness line rather than as a confusing dial failure. Second, the readiness
line is already the place where a TCP bot reports its kernel-assigned port —
extending it to carry the scheme keeps *one* answer to "where are you", rather
than one answer for the address and a separate assumption for the family.

That is a general protocol instinct worth keeping: when one side chooses and the
other side confirms, let the confirmation be complete. It costs six characters
on a line nobody reads twice.

### 3.3 Note the second reason `shutdown` is polite

`shutdown` already sends `SIGTERM` before `SIGKILL`, for M4's reason — a batch
exporter needs a chance to flush. Extend the comment, because there is now a
second reason and it is more visible than the first:

```rust
    /// Ask, wait, then insist.
    ///
    /// M3 killed bots outright. That was fine when a bot had nothing to say
    /// on the way out; a bot that exports telemetry has a batch queue, and
    /// `SIGKILL` throws it away. From M5 there is a second reason: a bot that
    /// is killed never unlinks its socket file, so the polite signal is what
    /// keeps `$XDG_RUNTIME_DIR/ni` from filling up with dead names.
    pub async fn shutdown(&mut self) -> Result<()> {
```

No code changes — but this is the moment to notice that M4's graceful-shutdown
work is what makes M5's cleanup story work at all. A `Drop` guard is only as
good as the process's chance to run it, and Checkpoint 8.2 shows what happens
when it does not get one.

### 3.4 Run the checkpoint

```sh
cargo fmt
cargo build -p ni-engine
```

Expect errors, and read them: `main.rs`, `failure_modes.rs` and (soon)
`ni-bench` all call `spawn`/`spawn_with_args` with the old arity. That is
normal mid-refactor — Checkpoint 4 fixes `main.rs`, Checkpoint 5 fixes the
tests. If you prefer to stay green, add `&Listen::Tcp` to every call site now
and change them properly later; the flag has to exist before it can be used.

---

## Checkpoint 4 — the flag, and the directory somebody has to own

### 4.1 Record the transport, and branch on nothing

In `crates/ni-engine/src/match_runner.rs`, add to the `use crate::{…}` block:

```rust
    transport::Transport,
```

Add the field to `RunOptions`:

```rust
pub struct RunOptions {
    pub delay: Duration,
    pub quiet: bool,
    pub match_id: String,
    pub time: TimeControl,
    /// `None` disables the JSONL file; the replay is built either way.
    pub match_log: Option<PathBuf>,
    /// Which socket family the bots were spawned on. The loop never reads
    /// this to make a decision — it is recorded so a measurement can be
    /// attributed, and that is the whole of M5's effect on the match loop.
    pub transport: Transport,
}
```

Put it on the match span, which makes every span in the trace filterable by
transport:

```rust
#[tracing::instrument(
    name = "match",
    skip_all,
    fields(
        otel.name = "match",
        ni.match_id = %options.match_id,
        ni.transport = options.transport.as_str(),
    )
)]
pub async fn run_match(
```

pass it to the log:

```rust
    let mut log = MatchLog::create(
        &options.match_id,
        &state,
        options.time,
        options.transport.as_str(),
        options.match_log.as_deref(),
    )?;
```

and add it to the opening event:

```rust
    info!(
        match_id = %options.match_id,
        transport = options.transport.as_str(),
        turn_deadline_ms = options.time.turn_deadline_ms(),
        strike_limit = options.time.strike_limit,
        "match begins"
    );
```

**That is the entire diff to the match loop.** Four references to
`options.transport`, all of them recording it, none of them reading it to decide
anything. When you write the blog post, this diff is the evidence: some 680 lines
of authoritative game loop, with deadlines, retries, recovery, forfeits and spans,
and swapping the transport underneath it required *zero* behavioural changes.

It is also the thing to watch for as the design evolves. The first `if
options.transport == Transport::Unix` that appears in this file will mean
something has leaked — either a genuine difference worth naming (a per-transport
deadline, say) or, more likely, a workaround for something that belongs in
`transport.rs`.

### 4.2 Add the transport to the match log

In `crates/ni-engine/src/log.rs`, take it as a parameter:

```rust
    pub fn create(
        match_id: &str,
        state: &MatchState,
        time: TimeControl,
        transport: &str,
        path: Option<&Path>,
    ) -> Result<Self> {
```

write it into the opening entry:

```rust
            &Entry::MatchStarted {
                ts_ms: now_ms(),
                match_id: &match_id,
                trace: TraceRef::current(),
                transport,
                turn_deadline_ms: time.turn_deadline_ms(),
```

and declare the field on the variant:

```rust
    MatchStarted {
        ts_ms: u128,
        match_id: &'a str,
        #[serde(flatten)]
        trace: TraceRef,
        /// `tcp` or `unix`: which socket family carried this match.
        transport: &'a str,
        turn_deadline_ms: u32,
```

The two unit tests in `log.rs` need the new argument (`"tcp"` and `"unix"` are
both fine).

**Why `&'a str` and not `Transport`.** `Entry` is the *serialized* shape, and it
already borrows `match_id` with the same lifetime. Putting the engine's enum in
here would mean deriving `Serialize` on `Transport` in `transport.rs`, which
makes a domain type responsible for its own JSON representation — the same
coupling M2 avoided by keeping proto types out of `ni-game`. One `as_str()` at
the call site keeps the log format owned by the logging module.

This is a one-word change with real downstream value: every match log line can
now be attributed to a transport, so `jq` over a directory of logs can answer
"were the slow turns all on TCP?" without anybody having remembered to write it
down.

### 4.3 Add the flags to `crates/ni-engine/src/main.rs`

Extend the import:

```rust
use ni_engine::{
    run_match, BotProcess, Listen, MatchConclusion, RunOptions, SocketDir, TimeControl, Transport,
};
```

Add two arguments to `Command::Run`:

```rust
        /// Socket family for the bots: `tcp` (loopback) or `unix`.
        #[arg(long, default_value = "tcp")]
        transport: Transport,

        /// Where Unix sockets are created. Defaults to `$XDG_RUNTIME_DIR/ni`,
        /// or `/tmp/ni-<uid>` when that is unset. Ignored for `--transport tcp`.
        #[arg(long)]
        socket_dir: Option<PathBuf>,
```

destructure them, put `transport` in the options, and pass `socket_dir`
separately:

```rust
        match_id,
        transport,
        socket_dir,
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
        transport,
    };

    let result = run(bot_a, bot_a_arg, bot_b, bot_b_arg, socket_dir, options).await;
```

**Why `socket_dir` is not in `RunOptions`.** `RunOptions` describes a *match*:
its id, its clock, its log. Where sockets live is a property of the
*deployment*, consumed entirely by `run` before `run_match` is called, and
`run_match` has no business knowing it. Passing it separately keeps the boundary
honest — and note that `Transport` *is* in `RunOptions`, because the match log
and the trace both want it recorded.

### 4.4 Give `run` the directory to own

```rust
async fn run(
    bot_a_path: PathBuf,
    bot_a_args: Vec<String>,
    bot_b_path: PathBuf,
    bot_b_args: Vec<String>,
    socket_dir: Option<PathBuf>,
    options: RunOptions,
) -> Result<MatchConclusion> {
    // Held for the whole match: dropping it removes the directory and every
    // socket in it. `None` for TCP, where there is nothing to clean up.
    let sockets = match options.transport {
        Transport::Tcp => None,
        Transport::Unix => {
            let base = socket_dir.unwrap_or_else(SocketDir::default_base);
            Some(SocketDir::create(&base, &options.match_id)?)
        }
    };

    let listen = |label: &str| -> Result<Listen> {
        match &sockets {
            Some(dir) => Ok(Listen::Unix(dir.socket(label)?)),
            None => Ok(Listen::Tcp),
        }
    };

    let mut bot_a =
        BotProcess::spawn_with_args(&bot_a_path, "bot A", &bot_a_args, &listen("a")?).await?;

    let mut bot_b =
        match BotProcess::spawn_with_args(&bot_b_path, "bot B", &bot_b_args, &listen("b")?).await {
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
```

The rest of `run` is unchanged.

**`sockets` is the lifetime.** It is a local binding in `run`, so it lives until
`run` returns — after `run_match`, after both `shutdown()` calls, after the
`?`s. Every exit path from this function, including the early return when bot B
fails to start and any panic, drops it and removes the directory. There is no
cleanup code anywhere in `run`, and that is the point of the guard.

**`listen` is a closure returning `Result`, and it has to be.** The path-length
check lives in `SocketDir::socket`, so producing a `Listen` can fail. Writing it
as a closure keeps the borrow of `sockets` short and local; writing it as a
method on something would have meant giving `SocketDir` an opinion about bot
labels. `&listen("a")?` reads awkwardly the first time and is exactly right: call
it, propagate the failure, pass a reference to the result.

**`unwrap_or_else(SocketDir::default_base)`, not `unwrap_or(…)`.** `unwrap_or`
evaluates its argument eagerly, so it would read `$XDG_RUNTIME_DIR` and build a
`PathBuf` even when `--socket-dir` was given. Function-as-argument here rather
than a closure because `default_base` already has the right signature.

### 4.5 Run the checkpoint

```sh
cargo fmt
cargo build --workspace
./target/debug/ni-engine run --help
```

```
      --transport <TRANSPORT>          Socket family for the bots: `tcp` (loopback) or `unix` [default: tcp]
      --socket-dir <SOCKET_DIR>        Where Unix sockets are created. Defaults to `$XDG_RUNTIME_DIR/ni`, or `/tmp/ni-<uid>` when that is unset. Ignored for `--transport tcp`
```

Now play the match this milestone exists for:

```sh
./target/debug/ni-engine run \
  --bot-a target/debug/ni-bot \
  --bot-b target/debug/ni-bot \
  --transport unix \
  --match-id m5-uds-demo \
  --no-match-log
```

The board renders, the knights advance, the match ends. Which is the entire
result: nothing about the game changed.

The interesting part is in the log lines. Compare the two transports side by
side:

```sh
for t in tcp unix; do
  ./target/debug/ni-engine run --bot-a target/debug/ni-bot --bot-b target/debug/ni-bot \
    --transport $t --match-id m5-$t --no-match-log --quiet 2>&1 |
    grep -E "identified|match begins" | head -3
done
```

```
INFO rpc{…/Identify…}: identified protocol=1 peer=tcp 127.0.0.1:52290
INFO match{… ni.transport="tcp"}: bot identified bot="A" pid=32634 endpoint="tcp://127.0.0.1:37357" name="reference-bot" …
INFO match{… ni.transport="tcp"}: match begins match_id=m5-tcp transport="tcp" turn_deadline_ms=500 strike_limit=3

INFO rpc{…/Identify…}: identified protocol=1 peer=unix pid=32650 uid=0 gid=0
INFO match{… ni.transport="unix"}: bot identified bot="A" pid=32658 endpoint="unix:///tmp/ni-0/m5-unix/a.sock" name="reference-bot" …
INFO match{… ni.transport="unix"}: match begins match_id=m5-unix transport="unix" turn_deadline_ms=500 strike_limit=3
```

Read the two `peer=` values carefully, because they are the checkpoint:

- `peer=tcp 127.0.0.1:52290` — an address and an ephemeral port. Nothing there
  identifies a process, a user, or even *which* local program connected.
- `peer=unix pid=32650 uid=0 gid=0` — and look at the pid: **32650 is the
  engine**, not the bot. The next line reports the bot as `pid=32658`. The bot
  learned its caller's process id from the kernel, and the engine could not have
  lied about it if it wanted to.

That is the capability difference, in two lines of real output. A bot that
wanted to refuse calls from anything but a uid it trusts now can. Over TCP there
is nothing to write that check against.

(`uid=0` because this transcript is from a container running as root, and
`/tmp/ni-0` rather than `$XDG_RUNTIME_DIR/ni` because that variable is unset
there. On a normal desktop session expect `uid=1000` and
`/run/user/1000/ni/…`.)

While the match runs, look at what the engine created:

```sh
./target/debug/ni-engine run --bot-a target/debug/ni-bot --bot-b target/debug/ni-bot \
  --transport unix --match-id watch-me --delay 300ms --no-match-log --quiet &
sleep 1 && ls -la /tmp/ni-0/watch-me
```

```
drwx------ 2 root root 4096 Aug 18 09:42 .
drwxr-xr-x 3 root root 4096 Aug 18 09:42 ..
srw------- 1 root root    0 Aug 18 09:42 a.sock
srw------- 1 root root    0 Aug 18 09:42 b.sock
```

Three things in that listing:

- `s` in the first column of the mode: this is a **socket** inode, not a regular
  file. `ls` has a type letter for it, `file` reports `socket`, and its size is
  and always will be 0 — a socket has no contents, only queues.
- `rw-------`, from `restrict_to_owner`, and `drwx------` on the directory, from
  `SocketDir::create`. Together they mean no other user on this host can reach
  either bot, enforced by the kernel's permission check before any Ni code runs.
- Both sockets in one per-match directory, so a second concurrent match cannot
  collide with this one and cleanup is one `remove_dir_all`.

And after the match:

```sh
ls -la /tmp/ni-0/
```

```
drwxr-xr-x  2 root root 4096 Aug 18 09:43 .
drwxrwxrwt 14 root root 4096 Aug 18 09:42 ..
```

Empty. Two `Drop` impls in two processes, and nothing left behind.

---

## Checkpoint 5 — the tests that make the claim falsifiable

"Nothing about the game changed" is an assertion. This checkpoint turns it into
six tests that would go red if it stopped being true.

### 5.1 Keep `crates/ni-engine/tests/failure_modes.rs` compiling

Extend the import:

```rust
use ni_engine::{
    policy::TimeControl, run_match, BotProcess, ForfeitReason, Listen, MatchConclusion, RunOptions,
    SocketDir, Transport,
};
```

Add the field to the shared `options` helper, and a socket-directory helper:

```rust
fn options(deadline_ms: u64, strike_limit: u32) -> RunOptions {
    RunOptions {
        delay: Duration::ZERO,
        quiet: true,
        match_id: "m5-test".to_string(),
        time: TimeControl {
            turn_deadline: Duration::from_millis(deadline_ms),
            strike_limit,
        },
        match_log: None,
        transport: Transport::Tcp,
    }
}

/// A socket directory unique to one test, under the system temp dir so the
/// path stays well inside the 108-byte `sun_path` limit.
fn sockets(name: &str) -> SocketDir {
    SocketDir::create(&std::env::temp_dir().join("ni-m5-tests"), name)
        .expect("socket directory is created")
}
```

and teach `play_against_roger` to honour whichever transport it was given:

```rust
async fn play_against_roger(flags: &[&str], options: RunOptions) -> MatchConclusion {
    // Every test that does not care about the transport gets TCP, exactly as
    // it did in M3 and M4. The transport-specific tests build their own.
    let sockets = match options.transport {
        Transport::Tcp => None,
        Transport::Unix => Some(sockets(&options.match_id)),
    };

    let listen = |label: &str| match &sockets {
        Some(dir) => Listen::Unix(dir.socket(label).expect("socket path fits")),
        None => Listen::Tcp,
    };

    let mut bot_a = BotProcess::spawn(&binary("ni-bot"), "bot A", &listen("a"))
        .await
        .expect("reference bot starts");

    let mut bot_b =
        BotProcess::spawn_with_args(&binary("roger-the-shrubber"), "bot B", flags, &listen("b"))
            .await
            .expect("roger starts");

    let conclusion = run_match(&mut bot_a, &mut bot_b, options).await;

    let _ = bot_a.shutdown().await;
    let _ = bot_b.shutdown().await;

    conclusion.expect("the engine survives a hostile bot")
}
```

Every M3/M4 test keeps passing untouched. That is worth pausing on: ten
integration tests about deadlines, strikes, crashes, recovery, illegal orders and
JSONL logging, and M5 changed the *arity of a constructor* in them and nothing
else.

### 5.2 Create `crates/ni-engine/tests/transports.rs`

```rust
//! Transport parity: the same match, the same failures, two socket families.
//!
//! These tests spawn the built binaries, so run `cargo build --workspace`
//! before `cargo test`. Their job is to make M5's central claim falsifiable —
//! if swapping the transport changed a *result*, one of these would go red.

use std::{path::PathBuf, time::Duration};

use ni_engine::{
    policy::TimeControl, run_match, transport::dial, BotProcess, ForfeitReason, Listen,
    MatchConclusion, RunOptions, SocketDir, Transport,
};
use ni_game::Chapter;
use serde_json::Value;

fn binary(name: &str) -> PathBuf {
    let mut path = std::env::current_exe().expect("test binary has a path");
    path.pop();
    if path.ends_with("deps") {
        path.pop();
    }

    let path = path.join(name);
    assert!(
        path.exists(),
        "{} is missing — run `cargo build --workspace` first",
        path.display()
    );
    path
}

fn sockets(name: &str) -> SocketDir {
    SocketDir::create(&std::env::temp_dir().join("ni-m5-transports"), name)
        .expect("socket directory is created")
}

fn options(match_id: &str, transport: Transport, deadline_ms: u64, strikes: u32) -> RunOptions {
    RunOptions {
        delay: Duration::ZERO,
        quiet: true,
        match_id: match_id.to_string(),
        time: TimeControl {
            turn_deadline: Duration::from_millis(deadline_ms),
            strike_limit: strikes,
        },
        match_log: None,
        transport,
    }
}

/// Play `ni-bot` against a Roger carrying `flags`, on whichever transport
/// `options` names. Returns the conclusion and the socket directory, so a
/// caller can inspect the filesystem before it is cleaned up.
async fn play(flags: &[&str], options: RunOptions) -> (MatchConclusion, Option<SocketDir>) {
    let dir = match options.transport {
        Transport::Tcp => None,
        Transport::Unix => Some(sockets(&options.match_id)),
    };

    let listen = |label: &str| match &dir {
        Some(dir) => Listen::Unix(dir.socket(label).expect("socket path fits")),
        None => Listen::Tcp,
    };

    let mut bot_a = BotProcess::spawn(&binary("ni-bot"), "bot A", &listen("a"))
        .await
        .expect("reference bot starts");

    let mut bot_b =
        BotProcess::spawn_with_args(&binary("roger-the-shrubber"), "bot B", flags, &listen("b"))
            .await
            .expect("roger starts");

    let conclusion = run_match(&mut bot_a, &mut bot_b, options).await;

    let _ = bot_a.shutdown().await;
    let _ = bot_b.shutdown().await;

    (
        conclusion.expect("the engine survives whichever transport it was given"),
        dir,
    )
}
```

`play` returns the `SocketDir` rather than dropping it, so a test can look at the
directory *before* the guard removes it. Returning a guard to extend its lifetime
is a normal and useful pattern; the caller binding it to `_dir` keeps it alive,
and binding it to `_` would delete the directory instantly — the same trap as
`_socket_file` in Checkpoint 2.

Now the six tests.

```rust
/// The milestone, as one assertion.
///
/// Same rules engine, same bots, same deadlines, different socket family —
/// and a deterministic game has no excuse for a different answer.
#[tokio::test]
async fn a_unix_socket_match_ends_exactly_as_the_loopback_match_did() {
    let (over_tcp, _) = play(&[], options("parity-tcp", Transport::Tcp, 500, 3)).await;
    let (over_unix, _) = play(&[], options("parity-unix", Transport::Unix, 500, 3)).await;

    assert!(matches!(over_tcp, MatchConclusion::Decided(_)));
    assert_eq!(
        over_tcp, over_unix,
        "the transport changed the outcome of a deterministic match"
    );
}

/// Post 6's most quotable finding, as a test: the ambiguity of a missed
/// deadline is a property of the *boundary*, not of the network.
#[tokio::test]
async fn the_ambiguous_case_survives_the_transport_swap() {
    let (conclusion, _) = play(
        &["--sleep-ms", "400"],
        options("slow-unix", Transport::Unix, 100, 2),
    )
    .await;

    assert_eq!(
        conclusion,
        MatchConclusion::Forfeit {
            loser: Chapter::B,
            reason: ForfeitReason::Timeout,
        },
        "a deadline over a Unix socket is still a deadline"
    );
}

#[tokio::test]
async fn a_bot_that_dies_on_a_unix_socket_is_still_merely_unreachable() {
    let (conclusion, _) = play(
        &["--crash-on-turn", "4"],
        options("crash-unix", Transport::Unix, 500, 3),
    )
    .await;

    assert_eq!(
        conclusion,
        MatchConclusion::Forfeit {
            loser: Chapter::B,
            reason: ForfeitReason::Crash,
        }
    );
}

#[tokio::test]
async fn the_match_log_records_which_transport_carried_the_match() {
    let path = std::env::temp_dir().join("ni-m5-transport-log.jsonl");
    let _ = std::fs::remove_file(&path);

    let mut options = options("logged-unix", Transport::Unix, 500, 3);
    options.match_log = Some(path.clone());

    let (_, _dir) = play(&[], options).await;

    let started: Value = serde_json::from_str(
        std::fs::read_to_string(&path)
            .expect("the engine wrote a match log")
            .lines()
            .next()
            .expect("the log has a first line"),
    )
    .expect("the first line is JSON");

    assert_eq!(started["kind"], "match_started");
    assert_eq!(started["transport"], "unix");
}

/// A port disappears when its socket closes. A path does not, so somebody has
/// to delete it — and this is the test that says who.
#[tokio::test]
async fn every_socket_file_is_gone_once_the_match_is_over() {
    let (_, dir) = play(&[], options("cleanup-unix", Transport::Unix, 500, 3)).await;
    let dir = dir.expect("a unix match has a socket directory");
    let path = dir.path().to_path_buf();

    // The bots unlink their own sockets on SIGTERM.
    assert!(
        std::fs::read_dir(&path)
            .expect("the directory still exists")
            .next()
            .is_none(),
        "a bot left its socket file behind: {}",
        path.display()
    );

    // And the engine owns the directory itself.
    drop(dir);
    assert!(!path.exists(), "the socket directory outlived the match");
}

/// The one place the transports genuinely differ in *shape*: what "nobody is
/// there" looks like before a single gRPC frame has been written.
#[tokio::test]
async fn dialling_a_path_with_no_listener_fails_instead_of_hanging() {
    let path = std::env::temp_dir().join("ni-m5-nobody-here.sock");
    let _ = std::fs::remove_file(&path);

    let error = dial(
        &format!("unix://{}", path.display()),
        Duration::from_millis(200),
    )
    .await
    .expect_err("there is no listener");

    // ENOENT, not ECONNREFUSED: a missing path is not a closed port.
    let text = format!("{error:#}");
    assert!(
        text.contains("could not connect to unix://"),
        "unexpected error: {text}"
    );
}
```

Three notes on how these are written.

**The parity test runs both matches in one test, not two.** A pair of tests
asserting "TCP decides" and "UDS decides" separately would both pass while
producing *different* winners. Comparing the two conclusions in one function is
what makes the assertion about equality rather than about liveness. It relies on
M1's determinism guarantee: no randomness anywhere, so identical inputs must
produce identical output.

**The cleanup test asserts twice, at two layers.** First that the *bots* removed
their own sockets (the directory is empty while the guard still exists), then
that the *engine* removed the directory (after an explicit `drop`). Merging them
into one assertion would pass even if only the engine's `remove_dir_all` worked,
which would hide a broken `SocketFile` guard — and a broken `SocketFile` guard is
exactly what leaves stale sockets around in production, where nobody calls
`remove_dir_all`.

**`{error:#}` and not `{error}`.** anyhow's alternate formatter prints the whole
error chain — `could not connect to unix://…: transport error: No such file or
directory (os error 2)` — while the plain one prints only the outermost context.
When you are asserting about an error produced three layers down, the `#` is what
lets you see it.

### 5.3 Run the checkpoint

```sh
cargo fmt
cargo build --workspace
cargo test --workspace
```

```
running 6 tests
test dialling_a_path_with_no_listener_fails_instead_of_hanging ... ok
test a_bot_that_dies_on_a_unix_socket_is_still_merely_unreachable ... ok
test every_socket_file_is_gone_once_the_match_is_over ... ok
test the_match_log_records_which_transport_carried_the_match ... ok
test a_unix_socket_match_ends_exactly_as_the_loopback_match_did ... ok
test the_ambiguous_case_survives_the_transport_swap ... ok

test result: ok. 6 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.23s
```

Note the total: 0.23 seconds for six integration tests that spawn twelve bot
processes and play five complete matches. Two of those matches run to a forfeit
that takes two 100 ms deadlines to reach. Sockets — either kind — are not what
makes a test suite slow.

---

## Checkpoint 6 — the measurement harness

Post 6 wants one harness, several transports, one service definition. This is
it, and the design constraint that matters most is that it measures the *real
RPCs* rather than a synthetic `Echo` method: `Identify` is the floor,
`GetOrders` is the hot path at its true size, and `SubmitReplay` is whatever
size you want it to be.

### 6.1 Create `crates/ni-bench/Cargo.toml`

```toml
[package]
name = "ni-bench"
version.workspace = true
edition.workspace = true
repository.workspace = true
description = "Measurement harness for Ni — one service definition, two transports, four payload sizes"

[dev-dependencies]
hyper-util = { workspace = true }
tower = { workspace = true }

[dependencies]
ni-proto = { workspace = true }
ni-game = { workspace = true }
ni-engine = { workspace = true }
ni-telemetry = { workspace = true }
anyhow = { workspace = true }
clap = { workspace = true }
prost = { workspace = true }
serde = { workspace = true }
serde_json = { workspace = true }
tonic = { workspace = true }
tokio = { workspace = true }
tracing = { workspace = true }
```

**Why it depends on `ni-engine`.** The harness needs to start a bot, wait for its
readiness line and dial it over either transport — which is `BotProcess` and
`transport::dial`, already written, already tested. A benchmark that reimplements
its own spawning is a benchmark that can be wrong in ways the real engine is not.
Reuse here is not laziness; it is the guarantee that the numbers describe the
code that plays matches.

### 6.2 Create `crates/ni-bench/src/lib.rs`

Everything that is arithmetic rather than I/O goes here, so it can be tested.

```rust
//! The parts of the measurement harness that have no I/O in them.
//!
//! Percentiles and payload construction are pure functions over numbers and
//! messages, so they are unit-testable — which matters more here than
//! elsewhere. A benchmark that reports the wrong number is worse than no
//! benchmark, and "the p99 was computed correctly" is not something you can
//! see by looking at the output.

use std::time::Duration;

use ni_proto::ni::v1::{
    BoardLayout, Chapter, KnightOrder, OrderOutcome, OrderResult, Position, Replay, Rules,
    TurnRecord,
};
use prost::Message as _;
use serde::Serialize;

/// One transport/payload combination's worth of timings.
#[derive(Debug, Default)]
pub struct Samples {
    latencies: Vec<Duration>,
}

impl Samples {
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            latencies: Vec::with_capacity(capacity),
        }
    }

    pub fn push(&mut self, latency: Duration) {
        self.latencies.push(latency);
    }

    pub fn len(&self) -> usize {
        self.latencies.len()
    }

    pub fn is_empty(&self) -> bool {
        self.latencies.is_empty()
    }

    /// Nearest-rank percentile: sort, then take the ceil(p × n)-th sample.
    ///
    /// No interpolation. Interpolating between two observed latencies invents
    /// a latency that was never measured, which is a strange thing to do to
    /// data you gathered specifically because you did not want to model it.
    pub fn percentile(&self, percentile: f64) -> Duration {
        if self.latencies.is_empty() {
            return Duration::ZERO;
        }

        let mut sorted = self.latencies.clone();
        sorted.sort_unstable();

        let rank = (percentile / 100.0 * sorted.len() as f64).ceil() as usize;
        let index = rank.saturating_sub(1).min(sorted.len() - 1);
        sorted[index]
    }

    pub fn mean(&self) -> Duration {
        if self.latencies.is_empty() {
            return Duration::ZERO;
        }

        let total: Duration = self.latencies.iter().sum();
        total / self.latencies.len() as u32
    }

    /// How many calls took longer than `threshold`.
    ///
    /// A percentile hides how many samples are in the tail — p99 of 5000 calls
    /// is one number standing in for fifty. This counts them, which is what
    /// you want when the interesting behaviour is rare and large rather than
    /// common and small.
    pub fn slower_than(&self, threshold: Duration) -> usize {
        self.latencies
            .iter()
            .filter(|latency| **latency > threshold)
            .count()
    }

    pub fn summarise(&self, transport: &'static str, case: &str, payload_bytes: usize) -> Summary {
        Summary {
            transport,
            case: case.to_string(),
            payload_bytes,
            calls: self.len(),
            min_us: self.percentile(0.0).as_micros(),
            p50_us: self.percentile(50.0).as_micros(),
            p90_us: self.percentile(90.0).as_micros(),
            p99_us: self.percentile(99.0).as_micros(),
            max_us: self.percentile(100.0).as_micros(),
            mean_us: self.mean().as_micros(),
            over_10ms: self.slower_than(Duration::from_millis(10)),
        }
    }
}
```

Three deliberate choices in that type.

**Nearest-rank, not linear interpolation.** Both are legitimate definitions of a
percentile, and the difference matters for a small sample set. Interpolation
answers "what latency would sit at the 99th percentile of a distribution shaped
like this?", which is a *model*. Nearest-rank answers "which observed call sits
at the 99th percentile?", which is a *fact*. For a latency harness where the
interesting samples are the strange ones, you want the fact.

**`sort_unstable`, on a clone.** Unstable sort (pattern-defeating quicksort) is
faster and needs no allocation for `Duration`, where equal elements are
indistinguishable so stability buys nothing. The clone is so that `percentile`
can take `&self` and be called five times from `summarise` without the caller
worrying about mutation order.

**`slower_than` exists because percentiles lie about counts.** p99 of 5000 calls
is *one* number representing 50 samples. If two of those 50 are 42 ms and the
rest are 400 µs, the p99 looks unremarkable. Counting calls over a fixed
threshold is how the most interesting result in this whole milestone became
visible — see Checkpoint 7.

Then the row type:

```rust
/// One row of the results table.
#[derive(Debug, Serialize)]
pub struct Summary {
    pub transport: &'static str,
    pub case: String,
    /// Encoded size of the *request* message, measured rather than assumed.
    pub payload_bytes: usize,
    pub calls: usize,
    pub min_us: u128,
    pub p50_us: u128,
    pub p90_us: u128,
    pub p99_us: u128,
    pub max_us: u128,
    pub mean_us: u128,
    /// Calls slower than 10ms — two orders of magnitude above the median for
    /// every case except the 1 MiB one, so on the small payloads this counts
    /// events that have an *explanation* rather than a distribution.
    pub over_10ms: usize,
}

impl Summary {
    /// Sequential round-trips per second, derived from the median.
    ///
    /// This is a latency harness, not a throughput harness: one call at a
    /// time, so this number is `1 / p50` and nothing more ambitious.
    pub fn calls_per_second(&self) -> f64 {
        if self.p50_us == 0 {
            return f64::INFINITY;
        }

        1_000_000.0 / self.p50_us as f64
    }

    /// Payload bytes per second at the median latency, in MiB/s.
    pub fn payload_mib_per_second(&self) -> f64 {
        if self.p50_us == 0 {
            return 0.0;
        }

        (self.payload_bytes as f64 * 1_000_000.0) / (self.p50_us as f64 * 1024.0 * 1024.0)
    }
}
```

`payload_bytes` is `encoded_len()` of the request, taken once before the loop:
**measured, not claimed**. When Checkpoint 7's table says `1048604`, that is the
exact protobuf length of the message that went over the socket, not "about a
meg". `#[derive(Serialize)]` is what makes `--json` work, so a run can be
appended to a file and compared against another machine's later.

The `calls_per_second` doc comment is the guard rail. A sequential harness
measures *latency*, and `1/p50` is the throughput you get from one caller doing
one thing at a time. Real throughput needs concurrent streams, HTTP/2 window
tuning and a completely different harness — and quoting `1/p50` as "requests per
second" is one of the most common ways benchmark numbers become dishonest.

### 6.3 Add the payload builder

```rust
/// Grow a replay until it encodes to at least `target_bytes`.
///
/// The point of doing it this way — rather than adding a `bytes padding = 99`
/// field to the proto — is that every byte measured is a byte the real
/// contract would carry. A 1 MiB replay is a plausible tournament artifact,
/// not a bag of zeroes, so protobuf is doing the same varint-and-tag work it
/// would do in production.
pub fn replay_of_at_least(target_bytes: usize) -> Replay {
    let mut replay = Replay {
        match_id: "ni-bench".to_string(),
        board: Some(BoardLayout {
            width: 10,
            height: 10,
            shrubbery: (0..12)
                .map(|index| Position {
                    x: index % 10,
                    y: index / 10,
                })
                .collect(),
        }),
        rules: Some(Rules {
            knight_hp: 10,
            move_range: 2,
            attack_range: 3,
            attack_damage: 3,
            cover_damage_reduction: 1,
            turn_cap: 100,
            turn_deadline_ms: 500,
            timeout_strike_limit: 3,
        }),
        turns: Vec::new(),
        winner: Chapter::A as i32,
        reason: 1,
    };

    // `encoded_len` walks the whole message, so calling it once per pushed
    // turn is quadratic — 70 seconds for a 1 MiB replay in a debug build.
    // Measure one turn, jump most of the way there, then top up.
    let empty = replay.encoded_len();
    replay.turns.push(turn_record(1));
    let per_turn = replay.encoded_len() - empty;

    let estimate = target_bytes.saturating_sub(empty) / per_turn.max(1);
    for turn in 2..=estimate as u32 {
        replay.turns.push(turn_record(turn));
    }

    // The estimate runs slightly short: a turn number past 127 needs a second
    // varint byte, so later records are a byte or two larger than the first.
    while replay.encoded_len() < target_bytes {
        replay
            .turns
            .push(turn_record(replay.turns.len() as u32 + 1));
    }

    replay
}

fn turn_record(turn: u32) -> TurnRecord {
    TurnRecord {
        turn,
        acting: if turn.is_multiple_of(2) {
            Chapter::B as i32
        } else {
            Chapter::A as i32
        },
        outcomes: (1..=4)
            .map(|unit| OrderOutcome {
                order: Some(KnightOrder {
                    unit_id: format!("A{unit}"),
                    move_to: Some(Position {
                        x: (turn + unit) % 10,
                        y: (turn * 3 + unit) % 10,
                    }),
                    attack_target: Some(format!("B{unit}")),
                }),
                result: OrderResult::Applied as i32,
                detail: String::new(),
                damage_dealt: 3,
            })
            .collect(),
        get_orders_latency_us: 420 + u64::from(turn),
        deadline_exceeded: false,
    }
}
```

That comment about quadratic behaviour is not hypothetical — the first version of
this function was `while replay.encoded_len() < target { push(…) }`, and its unit
test took **72 seconds**, because `encoded_len` walks every nested message every
time and a 1 MiB replay is about 9,500 turn records containing 38,000 orders. The
fix is the standard one for any "grow until a measured size" loop: measure the
increment once, extrapolate, then correct.

The correction loop is genuinely necessary, and the reason is a protobuf detail
worth knowing: `turn` is a `uint32` encoded as a **varint**, so turn 1 costs one
byte and turn 200 costs two. Records get slightly larger as the replay gets
longer, so a linear estimate always undershoots. This is the same property that
makes protobuf compact and makes its message sizes data-dependent — you cannot
compute an encoded length from a schema, only from a value.

Note also what the payload is *made of*: real `TurnRecord`s with real
`KnightOrder`s and real short strings. A 1 MiB replay therefore decodes into
about 38,000 heap-allocated `String`s, which turns out to matter a great deal in
Checkpoint 7.5.

### 6.4 Append the tests to `crates/ni-bench/src/lib.rs`

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn samples(millis: &[u64]) -> Samples {
        let mut samples = Samples::default();
        for value in millis {
            samples.push(Duration::from_millis(*value));
        }
        samples
    }

    #[test]
    fn percentiles_pick_real_observations() {
        // 100 samples, 1ms..100ms: the p-th percentile is the p-th sample.
        let values: Vec<u64> = (1..=100).collect();
        let samples = samples(&values);

        assert_eq!(samples.percentile(50.0), Duration::from_millis(50));
        assert_eq!(samples.percentile(90.0), Duration::from_millis(90));
        assert_eq!(samples.percentile(99.0), Duration::from_millis(99));
        assert_eq!(samples.percentile(100.0), Duration::from_millis(100));
        assert_eq!(samples.percentile(0.0), Duration::from_millis(1));
    }

    #[test]
    fn percentiles_do_not_care_about_arrival_order() {
        let ordered = samples(&[1, 2, 3, 4, 5]);
        let shuffled = samples(&[4, 1, 5, 2, 3]);

        assert_eq!(ordered.percentile(50.0), shuffled.percentile(50.0));
        assert_eq!(ordered.percentile(99.0), shuffled.percentile(99.0));
    }

    #[test]
    fn an_empty_sample_set_reports_zero_instead_of_panicking() {
        let empty = Samples::default();

        assert!(empty.is_empty());
        assert_eq!(empty.percentile(50.0), Duration::ZERO);
        assert_eq!(empty.mean(), Duration::ZERO);
    }

    #[test]
    fn a_single_sample_is_every_percentile() {
        let one = samples(&[7]);

        assert_eq!(one.percentile(0.0), Duration::from_millis(7));
        assert_eq!(one.percentile(50.0), Duration::from_millis(7));
        assert_eq!(one.percentile(100.0), Duration::from_millis(7));
    }

    #[test]
    fn a_replay_grows_to_the_requested_size_and_stays_close_to_it() {
        for target in [1_024, 64 * 1_024, 1_024 * 1_024] {
            let replay = replay_of_at_least(target);
            let encoded = replay.encoded_len();

            assert!(encoded >= target, "{encoded} < {target}");
            // One turn record is ~110 bytes, so overshoot is bounded by one.
            assert!(
                encoded < target + 256,
                "{encoded} overshoots {target} by more than one turn"
            );
        }
    }

    #[test]
    fn outliers_are_counted_not_just_ranked() {
        let mut samples = samples(&[1, 1, 1, 1, 1]);
        samples.push(Duration::from_millis(42));
        samples.push(Duration::from_millis(41));

        assert_eq!(samples.slower_than(Duration::from_millis(10)), 2);
        // Two outliers in seven samples do not reach the 50th percentile...
        assert_eq!(samples.percentile(50.0), Duration::from_millis(1));
        // ...but they are exactly what the top of the distribution is made of.
        assert_eq!(samples.percentile(100.0), Duration::from_millis(42));
    }

    #[test]
    fn a_summary_derives_rates_from_the_median() {
        let mut samples = Samples::default();
        samples.push(Duration::from_micros(1_000));
        let summary = samples.summarise("unix", "SubmitReplay", 1_048_576);

        assert_eq!(summary.p50_us, 1_000);
        assert!((summary.calls_per_second() - 1_000.0).abs() < 0.001);
        assert!((summary.payload_mib_per_second() - 1_000.0).abs() < 1.0);
    }
}
```

Seven tests on a benchmark's arithmetic may look like overkill. It is not.
`percentiles_pick_real_observations` uses the 1..=100 trick specifically so that
every expected value is obvious by inspection: with a hundred samples, the p-th
percentile *is* the p-th value, so an off-by-one in the rank calculation cannot
hide. `an_empty_sample_set_reports_zero_instead_of_panicking` covers the case
where a run is interrupted before the first measured call — a harness that panics
while reporting is a harness you stop trusting halfway through an experiment.

And `outliers_are_counted_not_just_ranked` is the test that documents *why*
`slower_than` exists: it asserts, in seven samples, the exact statistical
blindness that Checkpoint 7 runs into at 5,000.

### 6.5 Create `crates/ni-bench/src/main.rs`

```rust
//! `ni-bench` — the same service definition, over two transports.
//!
//! Post 6's whole argument fits in this binary: take one gRPC contract, run it
//! over loopback TCP and over a Unix domain socket, and see which costs belong
//! to the wire and which belong to the boundary.
//!
//! It deliberately reuses the *real* RPCs rather than a synthetic echo method:
//!
//! | case | RPC | what it measures |
//! |---|---|---|
//! | `Identify` | `Identify` | the floor: a call with almost no payload |
//! | `GetOrders` | `GetOrders` | the hot path, at its real size |
//! | `SubmitReplay 1KiB` | `SubmitReplay` | small payload |
//! | `SubmitReplay 1MiB` | `SubmitReplay` | large payload |
//!
//! Nothing here is a microbenchmark of protobuf or of tokio. Every number is a
//! full client-observed round trip: encode, write, read, decode, on both sides.

use std::{path::PathBuf, time::Instant};

use anyhow::{Context, Result};
use clap::Parser;
use ni_bench::{replay_of_at_least, Samples, Summary};
use ni_engine::{
    convert::{battlefield_view, new_match_request},
    BotProcess, Listen, SocketDir, TimeControl, Transport,
};
use ni_proto::{
    ni::v1::{GetOrdersRequest, IdentifyRequest, Replay, SubmitReplayRequest},
    PROTOCOL_VERSION,
};
use prost::Message as _;
use tonic::Request;

/// Generous on purpose. A deadline that fires mid-benchmark turns a latency
/// measurement into a timeout measurement.
const CALL_DEADLINE: std::time::Duration = std::time::Duration::from_secs(30);

#[derive(Parser)]
#[command(about = "Measure one Ni service definition over loopback TCP and Unix sockets")]
struct Cli {
    /// Bot binary to measure against.
    #[arg(long, default_value = "target/debug/ni-bot")]
    bot: PathBuf,

    /// Measured calls per case.
    #[arg(long, default_value_t = 2_000)]
    iterations: usize,

    /// Unmeasured calls first, so the numbers exclude connection warm-up,
    /// lazy allocation and the first-call HTTP/2 settings exchange.
    #[arg(long, default_value_t = 200)]
    warmup: usize,

    /// `tcp`, `unix`, or `both`.
    #[arg(long, default_value = "both")]
    transport: String,

    #[arg(long)]
    socket_dir: Option<PathBuf>,

    /// Emit one JSON object per row instead of a table.
    #[arg(long)]
    json: bool,
}

#[tokio::main]
async fn main() -> Result<()> {
    // The observer effect, handled before it can be measured. M4 gave every
    // handler an INFO event; at 2000 iterations that is 2000 formatted lines
    // per case, written to an inherited stderr, inside the timing loop. The
    // first version of this harness measured `tracing` as much as it measured
    // the transport. Bots inherit this variable when the engine spawns them.
    if std::env::var_os("RUST_LOG").is_none() {
        std::env::set_var("RUST_LOG", "warn");
    }

    let telemetry = ni_telemetry::init("ni-bench");
    let cli = Cli::parse();

    let transports: Vec<Transport> = match cli.transport.as_str() {
        "both" => vec![Transport::Tcp, Transport::Unix],
        other => vec![other
            .parse()
            .map_err(|error: String| anyhow::anyhow!(error))?],
    };

    let result = run(&cli, &transports).await;
    telemetry.shutdown();

    let rows = result?;

    if cli.json {
        for row in &rows {
            println!("{}", serde_json::to_string(row)?);
        }
    } else {
        print_table(&rows);
        print_comparison(&rows);
    }

    Ok(())
}
```

**The `RUST_LOG` block is the most important eight lines in the file**, and it
was written after the first run rather than before it. M4's reference bot logs an
INFO event per handler call. Ask it for 2,000 `Identify` calls and it produces
2,000 formatted, timestamped, span-decorated lines on a stderr inherited from the
benchmark — synchronously, inside the region being timed. The first table this
harness produced was substantially a measurement of `tracing_subscriber`'s
formatter.

Setting `RUST_LOG=warn` before `ni_telemetry::init` fixes it for the harness, and
because `BotProcess` spawns children that inherit the environment, it fixes it for
the bots too, with no new flag and no change to `process.rs`. The condition means
you can still say `RUST_LOG=info ni-bench …` when you want to see what is
happening and do not care about the numbers.

Generalise the lesson, because it is not about `tracing`: **instrumentation is
work, and work inside a timed region is part of the measurement.** M4 was right
to add those events and Checkpoint 6 is right to switch them off; what would be
wrong is doing neither deliberately.

### 6.6 Add the measurement loop

```rust
async fn run(cli: &Cli, transports: &[Transport]) -> Result<Vec<Summary>> {
    // Built once, outside the timing loops, and reused for every transport.
    // Two transports measuring two different payloads would measure nothing.
    let small_replay = replay_of_at_least(1_024);
    let large_replay = replay_of_at_least(1_024 * 1_024);

    let mut rows = Vec::new();

    for transport in transports {
        let sockets = match transport {
            Transport::Tcp => None,
            Transport::Unix => {
                let base = cli
                    .socket_dir
                    .clone()
                    .unwrap_or_else(SocketDir::default_base);
                Some(SocketDir::create(&base, "bench")?)
            }
        };

        let listen = match &sockets {
            Some(dir) => Listen::Unix(dir.socket("bench")?),
            None => Listen::Tcp,
        };

        let mut bot = BotProcess::spawn(&cli.bot, "bench bot", &listen)
            .await
            .with_context(|| {
                format!(
                    "could not start {} over {}",
                    cli.bot.display(),
                    transport.as_str()
                )
            })?;

        eprintln!(
            "measuring {} over {} ({} iterations, {} warmup)",
            bot.endpoint(),
            transport.as_str(),
            cli.iterations,
            cli.warmup
        );

        rows.push(measure_identify(&mut bot, *transport, cli).await?);
        rows.push(measure_get_orders(&mut bot, *transport, cli).await?);
        rows.push(
            measure_replay(
                &mut bot,
                *transport,
                cli,
                &small_replay,
                "SubmitReplay 1KiB",
            )
            .await?,
        );
        rows.push(
            measure_replay(
                &mut bot,
                *transport,
                cli,
                &large_replay,
                "SubmitReplay 1MiB",
            )
            .await?,
        );

        bot.shutdown().await?;
    }

    Ok(rows)
}

async fn measure_identify(
    bot: &mut BotProcess,
    transport: Transport,
    cli: &Cli,
) -> Result<Summary> {
    let message = IdentifyRequest {
        protocol_version: PROTOCOL_VERSION,
    };
    let bytes = message.encoded_len();

    let mut samples = Samples::with_capacity(cli.iterations);

    for iteration in 0..cli.warmup + cli.iterations {
        let mut request = Request::new(message);
        request.set_timeout(CALL_DEADLINE);

        let started = Instant::now();
        bot.client.identify(request).await?;
        let latency = started.elapsed();

        if iteration >= cli.warmup {
            samples.push(latency);
        }
    }

    Ok(samples.summarise(transport.as_str(), "Identify", bytes))
}

async fn measure_get_orders(
    bot: &mut BotProcess,
    transport: Transport,
    cli: &Cli,
) -> Result<Summary> {
    // The bot answers SHRUBBERY_REQUIRED for a match it has never heard of,
    // so the benchmark has to play by the contract's rules like anyone else.
    let state = ni_game::standard_match(ni_game::Rules::standard());
    let match_id = "ni-bench";

    bot.client
        .new_match(Request::new(new_match_request(
            &state,
            match_id,
            ni_game::Chapter::A,
            TimeControl::standard(),
        )))
        .await?;

    let message = GetOrdersRequest {
        match_id: match_id.to_string(),
        turn: state.turn,
        view: Some(battlefield_view(&state)),
    };
    let bytes = message.encoded_len();

    let mut samples = Samples::with_capacity(cli.iterations);

    for iteration in 0..cli.warmup + cli.iterations {
        let mut request = Request::new(message.clone());
        request.set_timeout(CALL_DEADLINE);

        let started = Instant::now();
        bot.client.get_orders(request).await?;
        let latency = started.elapsed();

        if iteration >= cli.warmup {
            samples.push(latency);
        }
    }

    Ok(samples.summarise(transport.as_str(), "GetOrders", bytes))
}

async fn measure_replay(
    bot: &mut BotProcess,
    transport: Transport,
    cli: &Cli,
    replay: &Replay,
    case: &str,
) -> Result<Summary> {
    let message = SubmitReplayRequest {
        replay: Some(replay.clone()),
    };
    let bytes = message.encoded_len();

    // A 1 MiB payload at 2000 iterations is 2 GiB of copying for a number
    // that is already stable after a few hundred calls.
    let iterations = if bytes > 512 * 1_024 {
        cli.iterations.min(200)
    } else {
        cli.iterations
    };
    let warmup = cli.warmup.min(iterations / 4);

    let mut samples = Samples::with_capacity(iterations);

    for iteration in 0..warmup + iterations {
        let mut request = Request::new(message.clone());
        request.set_timeout(CALL_DEADLINE);

        let started = Instant::now();
        bot.client.submit_replay(request).await?;
        let latency = started.elapsed();

        if iteration >= warmup {
            samples.push(latency);
        }
    }

    Ok(samples.summarise(transport.as_str(), case, bytes))
}
```

Six decisions in there that a benchmark lives or dies by:

**One bot process per transport, reused across all four cases.** Spawning is not
being measured, so it must not be inside a loop that is. It also means the
connection is established once and the HTTP/2 settings exchange happens once.

**Warm-up is discarded by index, not by a separate loop.** `if iteration >=
cli.warmup` keeps the hot path identical between warmed and measured calls —
same allocation, same code, same branch predictor state. A separate warm-up loop
tends to drift from the real one.

**The payload is built once, outside every loop, and shared between transports.**
If the 1 MiB replay were rebuilt per transport, the two rows would be measuring
different messages of coincidentally similar size. `message.clone()` per iteration
does cost something (Checkpoint 7.5 measures it: 3.9 ms for 1 MiB) — which is
exactly why it sits **before** `Instant::now()`.

**`message` for `Identify`, `message.clone()` for the others.**
`IdentifyRequest` is `Copy` — one `u32` — so clippy correctly objects to cloning
it. The larger messages own heap data and must be cloned because `Request::new`
takes ownership.

**The 1 MiB case self-limits to 200 iterations.** 2,000 × 1 MiB is 2 GiB of
copying per transport for a number that stops moving after a couple of hundred
calls. Note the honesty requirement that comes with this: the table prints the
`n` it actually used, so nobody has to guess whether a row is 200 or 5,000
samples.

**`CALL_DEADLINE` is 30 seconds.** Every other part of Ni sets a tight deadline
because a slow bot is a game event. Here a deadline that fires would silently
convert a latency sample into an error, so it is set high enough to never
participate — and it is still set, because a benchmark that hangs forever on a
wedged bot is worse than one that fails.

### 6.7 Add the output

```rust
fn print_table(rows: &[Summary]) {
    println!();
    println!(
        "{:<6} {:<20} {:>9} {:>6} {:>8} {:>8} {:>8} {:>8} {:>8} {:>9}",
        "trans", "case", "bytes", "n", "p50 us", "p90 us", "p99 us", "max us", ">10ms", "calls/s"
    );
    println!("{}", "-".repeat(101));

    for row in rows {
        println!(
            "{:<6} {:<20} {:>9} {:>6} {:>8} {:>8} {:>8} {:>8} {:>8} {:>9.0}",
            row.transport,
            row.case,
            row.payload_bytes,
            row.calls,
            row.p50_us,
            row.p90_us,
            row.p99_us,
            row.max_us,
            row.over_10ms,
            row.calls_per_second(),
        );
    }
}

/// The only number post 6 actually argues about: how much of the median call
/// the wire was responsible for.
fn print_comparison(rows: &[Summary]) {
    let cases: Vec<&str> = rows
        .iter()
        .filter(|row| row.transport == "tcp")
        .map(|row| row.case.as_str())
        .collect();

    if cases.is_empty() {
        return;
    }

    println!();
    println!(
        "{:<20} {:>10} {:>10} {:>10} {:>10}",
        "case", "tcp p50", "unix p50", "delta", "unix MiB/s"
    );
    println!("{}", "-".repeat(64));

    for case in cases {
        let tcp = rows
            .iter()
            .find(|row| row.transport == "tcp" && row.case == case);
        let unix = rows
            .iter()
            .find(|row| row.transport == "unix" && row.case == case);

        let (Some(tcp), Some(unix)) = (tcp, unix) else {
            continue;
        };

        let delta = if tcp.p50_us == 0 {
            0.0
        } else {
            (unix.p50_us as f64 - tcp.p50_us as f64) / tcp.p50_us as f64 * 100.0
        };

        println!(
            "{:<20} {:>10} {:>10} {:>9.1}% {:>10.1}",
            case,
            tcp.p50_us,
            unix.p50_us,
            delta,
            unix.payload_mib_per_second(),
        );
    }
}
```

`let (Some(tcp), Some(unix)) = (…) else { continue };` is a **let-else** over a
tuple: bind both or skip this row. It is what makes `--transport unix` alone
print a table with no comparison section rather than a panic or a table of
misleading zeroes.

### 6.8 Add the two examples

`crates/ni-bench/examples/boundary_cost.rs` — Checkpoint 7.5 needs it:

```rust
//! How much of a round trip is protobuf, with no wire involved at all?
//!
//! `ni-bench` measures the whole boundary. This measures the part of it that
//! no choice of socket can remove: encoding on one side and decoding on the
//! other. Subtract these from the harness's numbers and what is left is
//! everything the transport, HTTP/2 and the scheduler contribute together.
//!
//! Run it with `cargo run --release -p ni-bench --example boundary_cost`.
use std::time::Instant;

use ni_bench::replay_of_at_least;
use ni_proto::ni::v1::SubmitReplayRequest;
use prost::Message as _;

fn main() {
    for target in [1024usize, 1024 * 1024] {
        let message = SubmitReplayRequest {
            replay: Some(replay_of_at_least(target)),
        };
        let bytes = message.encode_to_vec();

        // Best-of, not mean: this is a floor measurement, and the fastest
        // observed run is the one least polluted by everything else on the box.
        let mut encode = std::time::Duration::MAX;
        let mut decode = std::time::Duration::MAX;
        let mut clone = std::time::Duration::MAX;

        for _ in 0..200 {
            let started = Instant::now();
            let buffer = message.encode_to_vec();
            encode = encode.min(started.elapsed());
            std::hint::black_box(buffer);

            let started = Instant::now();
            let decoded = SubmitReplayRequest::decode(bytes.as_slice()).unwrap();
            decode = decode.min(started.elapsed());
            std::hint::black_box(decoded);

            // Not part of the boundary — but `ni-bench` clones this message
            // once per iteration, so it is worth knowing it is cheaper than
            // the encode it feeds. (It is measured outside the timed region
            // there; this is how we know that was worth doing.)
            let started = Instant::now();
            let copied = message.clone();
            clone = clone.min(started.elapsed());
            std::hint::black_box(copied);
        }

        println!(
            "{:>9} bytes  encode {:>9?}  decode {:>9?}  clone {:>9?}",
            bytes.len(),
            encode,
            decode,
            clone
        );
    }
}
```

`std::hint::black_box` is the only reason this measures anything. Without it, the
optimiser is entitled to notice that `buffer` and `decoded` are never used and
delete the work entirely — and in release mode it will. `black_box` is an opaque
identity function: the compiler must assume its argument is observed, so the
computation has to happen. Any Rust microbenchmark without it is measuring
whether LLVM noticed.

`crates/ni-bench/examples/dial_errors.rs` — Checkpoint 8.5 needs it:

```rust
//! What "nobody is there" looks like on each transport, before gRPC starts.
use std::time::Duration;

use ni_engine::transport::dial;

#[tokio::main]
async fn main() {
    let missing = std::env::temp_dir().join("ni-missing.sock");
    let _ = std::fs::remove_file(&missing);

    let stale = std::env::temp_dir().join("ni-stale.sock");
    let _ = std::fs::remove_file(&stale);
    {
        let _listener = tokio::net::UnixListener::bind(&stale).expect("bind");
    }

    for endpoint in [
        format!("unix://{}", missing.display()),
        format!("unix://{}", stale.display()),
        "tcp://127.0.0.1:1".to_string(),
    ] {
        match dial(&endpoint, Duration::from_millis(500)).await {
            Ok(_) => println!("{endpoint}: connected (unexpected)"),
            Err(error) => println!("{endpoint}\n    {error:#}"),
        }
    }

    let _ = std::fs::remove_file(&stale);
}
```

The two examples need `tower` and `hyper-util` only if you extend them; the
`[dev-dependencies]` block in 6.1 is there so you can, without touching the
binary's dependency list.

### 6.9 Run the checkpoint

```sh
cargo fmt
cargo test -p ni-bench
cargo build --workspace --release
./target/release/ni-bench --bot target/release/ni-bot --iterations 2000 --warmup 200
```

Expected: 7 unit tests pass, and a table appears. **Use `--release`.** A debug
build measures `Vec` bounds checks and prost's unoptimised decode loop, which is
a real thing to know about debug builds and not what this harness is for.

---

## Checkpoint 7 — read the numbers honestly

This is the checkpoint that turns M5 into blog post 6, and the work in it is
mostly *not* writing code. It is refusing to over-read a table.

### 7.1 The table

5,000 iterations, 500 warm-up, `--release`, on the machine named at the top of
this workbook (4 vCPU Xeon @ 2.10 GHz, kernel 6.18, Ubuntu 24.04):

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

Before interpreting any of it, note two things about the shape of the table.
`bytes` is the measured `encoded_len` of each request, so `Identify` really is a
two-byte message and the 1 MiB case really is 1,048,604 bytes. And the `>10ms`
column is meaningless for the 1 MiB rows — the median there is 27 ms, so of
course all 200 calls exceed 10 ms. It is the small-payload rows where that column
says something.

### 7.2 The median says nothing, and that is the finding

The delta column above reads: UDS is 6% slower on the empty call, identical on
`GetOrders`, 11% faster on 1 KiB, 4% faster on 1 MiB. Run it again and the signs
move around. Across eight runs on this machine, the `Identify` p50 was 156–180 µs over TCP
and 166–207 µs over UDS: **two overlapping ranges**, with the direction of the
difference depending on which run you quote.

So the honest statement is not "UDS is 8% faster" or "UDS is 6% slower". It is:

> On this machine, swapping loopback TCP for a Unix domain socket did not move
> the median latency of a gRPC call by more than the run-to-run variance.

That is a *much* more interesting sentence than a percentage, because it is the
blog trace's thesis arriving as data:

> The expensive part of a service boundary was never the wire.

An empty gRPC call costs about 160 µs here. Removing the entire TCP/IP stack from
underneath it changes that by a few microseconds. Whatever those 160 µs are, they
are not the network — they are two userspace processes, two HTTP/2
implementations, a serializer, a scheduler, and two context switches, and every
one of those survives the transport swap intact.

**A warning about generalising this.** Your machine may well show a real,
repeatable UDS win, and plenty of published comparisons do. The variables that
matter most are how expensive a syscall is (spectre/meltdown mitigations,
virtualisation) and how much of the loopback path your kernel elides. This
transcript is from a 4-vCPU VM where syscalls are relatively costly and the
per-call work therefore dominates; on bare metal with a cheaper syscall path, the
TCP state machine is a larger fraction of a smaller number. The only defensible
report is the one post 6 already plans to make: *my machine, this payload, this
many iterations*. Ship the harness with the number and let readers re-run it.

### 7.3 The tail says something, and it repeats

Look again at the `max us` and `>10ms` columns for the small payloads:

| | TCP | UDS |
|---|---|---|
| `GetOrders` max | **41,873 µs** | 1,008 µs |
| `GetOrders` calls > 10 ms | 2 | 0 |
| `SubmitReplay 1KiB` max | **44,027 µs** | 1,075 µs |
| `SubmitReplay 1KiB` calls > 10 ms | 15 | 0 |

That is a call taking **250 times the median**, and it is not a fluke of one run.
Three runs carrying the `>10ms` counter, 10,000 small-payload calls each:

| run | TCP calls > 10 ms | UDS calls > 10 ms |
|---|---|---|
| 1 | 17 (2 + 15) | 0 |
| 2 | 16 (1 + 15) | 1 |
| 3 | 16 (1 + 15) | 0 |

Fifteen slow `SubmitReplay 1KiB` calls per 5,000 on TCP, in every run, is not
noise — it is a rate. And across all eight runs of the harness, the TCP maximum
on those two rows landed between **40.8 ms and 44.4 ms** every single time —
fifteen separate measurements inside a 3.6 ms band — while the worst UDS outlier
over the same period was 21.8 ms, once. (TCP's `Identify` row is not part of
this: its maxima ranged from 0.5 ms to 13 ms with no clustering at all.)

A latency that clusters at 40-something milliseconds, on TCP, on loopback, is not
random jitter. It is a **timer**. The obvious candidate is the delayed-ACK
mechanism: a receiver may hold an acknowledgement back hoping to piggyback it on
data of its own, and Linux's ACK timeout (`TCP_DELACK_MAX`) is 200 ms with a
minimum around 40 ms — and when a sender is waiting on that ACK before it can
send more, the caller sees exactly this shape. A Unix socket has no
acknowledgements to delay, no retransmit timer, and no such class of stall.

### 7.4 Verify it with the kernel's own counters

Do not take the previous paragraph on faith — Linux keeps counters for exactly
this. `/proc/net/netstat` has a `TcpExt:` section; snapshot it around a run and
diff:

```sh
python3 - <<'PY'
import subprocess
def snap():
    lines = open('/proc/net/netstat').read().splitlines()
    header = [l for l in lines if l.startswith('TcpExt:')]
    keys = header[0].split()[1:]
    vals = [int(v) for v in header[1].split()[1:]]
    return dict(zip(keys, vals))

for transport in ('tcp', 'unix'):
    before = snap()
    subprocess.run(['./target/release/ni-bench', '--bot', 'target/release/ni-bot',
                    '--transport', transport, '--iterations', '5000', '--warmup', '500'],
                   stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    after = snap()
    moved = {k: after[k] - before[k] for k in before
             if after[k] - before[k]
             and any(x in k.lower() for x in ('delayed', 'retrans', 'timeout', 'lost'))}
    print(f"--transport {transport}: {moved or 'no TcpExt movement'}")
PY
```

```
--transport tcp: {'DelayedACKs': 50, 'DelayedACKLost': 2}
--transport unix: no TcpExt movement
```

There it is. Fifty delayed ACKs during a TCP run of the harness; two of them
"lost", meaning the ACK timer actually fired before the data it was waiting to
ride on appeared. During the Unix run, not one TCP counter moved — because there
is no TCP.

This is the most quotable result in the milestone, and note *why* it was
findable: the `>10ms` counter in Checkpoint 6.2 made a rare event visible, and
the kernel's counters made it explicable. A p99 alone would have shown neither.

**Be careful what you claim from it.** The counters prove delayed ACKs happened
during the TCP run and cannot happen during the UDS run. Attributing the
particular 42 ms calls to them is a strong inference, not a proof — the next step
if you wanted certainty would be `bpftrace` on `tcp_delack_timer`, or a run with
`TCP_QUICKACK`/`TCP_NODELAY` toggled. That is a good afternoon and it is post 6's
job, not M5's. What M5 has to say is already sound: *the transport did not change
the median, and it did change the shape of the tail, and here is the mechanism
that can only exist on one side.*

### 7.5 Where the 1 MiB milliseconds actually go

The 1 MiB row is the strangest number in the table: ~27 ms for one megabyte, in
release mode, over a socket in the same kernel. That is about 37 MiB/s, which is
orders of magnitude below what a `memcpy` on this machine can do. And the
transport barely changes it: 27,721 µs over TCP against 26,561 µs over UDS.

So where does the time go? Run the example from Checkpoint 6.8, which does nothing
but encode and decode:

```sh
cargo run --release -p ni-bench --example boundary_cost
```

```
     1044 bytes  encode   4.437µs  decode   6.605µs  clone   1.602µs
  1048604 bytes  encode 3.789318ms  decode 7.863298ms  clone 3.87208ms
```

Now the round trip is accountable:

| Component of one 1 MiB `SubmitReplay` | Time | Share of a 27 ms call |
|---|---|---|
| Client encodes the request (prost) | ~3.8 ms | 14% |
| Server decodes the request (prost) | ~7.9 ms | 29% |
| *(both vary run to run: encode 3.4–4.4 ms, decode 7.4–7.9 ms)* | | |
| **Serialization subtotal** | **~11.7 ms** | **~43%** |
| Everything else: HTTP/2 framing and flow control, socket writes and reads, wakeups, scheduling, the response trip | ~15 ms | ~57% |
| Difference the socket family made | ~1 ms | ~4%, within noise |

**Protobuf is the single largest line item, and decode is twice encode.** That
asymmetry is not a prost defect; it is what decoding *is*. Encoding walks a tree
of values that already exist and appends bytes. Decoding allocates that tree:
this replay is ~9,500 `TurnRecord`s holding ~38,000 `KnightOrder`s, each with two
short `String`s — roughly 76,000 heap allocations, plus the `Vec` growth to hold
them. Serialization cost is dominated by allocation, and allocation does not care
what kind of socket delivered the bytes.

Which is, word for word, the claim the blog trace opens with:

> Serialization stays. The failure model stays. What goes away is the hop.

Post 6 gets to make that argument with a table instead of an assertion.

**A hypothesis worth recording because it was wrong.** The obvious suspect for
"1 MB takes 27 ms" is HTTP/2 flow control: the default initial stream window is
64 KiB, so a megabyte needs sixteen windows' worth of `WINDOW_UPDATE`
round trips. Testing it takes ten minutes — build a client with
`Endpoint::initial_stream_window_size` and `initial_connection_window_size` set,
and vary them:

| window | best of 100 calls |
|---|---|
| tonic default (64 KiB) | 28.3 ms |
| 256 KiB | 30.5 ms |
| 1 MiB | 27.6 ms |
| 4 MiB | 27.5 ms |

No effect worth the name. The window is not the bottleneck here, because the
bottleneck is upstream of it: at ~37 MiB/s the sender is not producing bytes fast
enough for a 64 KiB window to be the constraint. Write this down for the post
anyway — a rejected hypothesis with a measurement attached is more persuasive
than three confirmed ones, and it is the difference between "I profiled this" and
"I guessed and got lucky".

### 7.6 How to report it

Three rules, all learned the hard way in this checkpoint:

1. **Name the machine, the payload, the iteration count and the build profile.**
   "UDS is faster than loopback" is unfalsifiable and therefore worthless. "On a
   4-vCPU Xeon VM, 5,000 sequential calls, release build, the medians were within
   run-to-run variance and TCP produced 16–17 stalls over 10 ms per 10,000 small
   calls where UDS produced 0–1" is a claim somebody can check.
2. **Run it more than once, and report the spread.** One table is an anecdote.
   The single most useful thing in this whole checkpoint was noticing that the
   sign of the median difference flipped between runs.
3. **Say which numbers you cannot explain.** `unix Identify max 2,348 µs` is
   fifteen times its own median and there is no delayed-ACK story for it —
   probably scheduler preemption on a 4-vCPU box, but "probably" is what should
   appear in the post.

### 7.7 What to do with the harness now

`--json` exists so the harness outlives the afternoon:

```sh
./target/release/ni-bench --bot target/release/ni-bot --iterations 5000 --json >> bench.jsonl
```

One object per row, one file per machine, and `jq` will happily compare a laptop
against a server against a container six months later. That is the real payoff of
building the harness during M5 rather than while writing post 6: the numbers
accumulate for free, and post 6 becomes mostly writing — exactly as the blog
notes predicted.

---

## Checkpoint 8 — watch each thing happen

Six experiments. None of them needs new code; all of them are things that will
happen to you in production, so it is better to see them now, deliberately.

### 8.1 A bot's socket, from `bind` to gone

```sh
mkdir -p /tmp/ni-demo
./target/debug/ni-bot --listen unix:///tmp/ni-demo/a.sock &
BOT=$!
until [ -S /tmp/ni-demo/a.sock ]; do sleep 0.1; done
ls -l /tmp/ni-demo
```

```
LISTENING unix:///tmp/ni-demo/a.sock
total 0
srw------- 1 root root 0 Aug 18 09:30 a.sock
```

Then ask it to leave politely:

```sh
kill -TERM $BOT
wait $BOT
ls -la /tmp/ni-demo
```

```
total 8
drwxr-xr-x 2 root root 4096 Aug 18 09:30 .
drwxrwxrwt 12 root root 4096 Aug 18 09:30 ..
```

`SIGTERM` → `terminated()` completes → `serve_with_incoming_shutdown` returns →
`serve_unix` returns → `SocketFile::drop` unlinks the file. Four steps, all of
them ordinary control flow, and the last one only happens because the guard was
still alive when the function ended.

`ls -S` is worth a glance too: the socket's size is 0 and always will be. It has
no contents — only kernel-side queues that no filesystem operation can see.

### 8.2 A bot that is killed leaves its name behind

Now the case the guard cannot help with:

```sh
./target/debug/ni-bot --listen unix:///tmp/ni-demo/a.sock >/dev/null 2>&1 &
BOT=$!
until [ -S /tmp/ni-demo/a.sock ]; do sleep 0.1; done
kill -KILL $BOT
wait $BOT 2>/dev/null
ls -l /tmp/ni-demo
```

```
total 0
srw------- 1 root root 0 Aug 18 09:31 a.sock
```

`SIGKILL` cannot be caught, blocked or handled. No `Drop` runs, no destructor,
no cleanup — the process ceases to exist between two instructions and its socket
file becomes garbage that outlives it.

This is the whole reason `SocketDir` exists in the engine and the whole reason
M4's `SIGTERM`-before-`SIGKILL` shutdown matters more than it looked like it did.
There is a general principle here worth taking to any resource that lives outside
your process: **a destructor is a cleanup strategy for the ordinary case, and you
also need one that does not depend on your process being alive.** For Ni that
second layer is the parent's `remove_dir_all`. For a daemon it would be a sweep at
start-up.

Now restart on that path and watch the recovery:

```sh
./target/debug/ni-bot --listen unix:///tmp/ni-demo/a.sock
```

```
2026-08-18T09:32:21.983812Z  WARN removing a stale socket file path=/tmp/ni-demo/a.sock
LISTENING unix:///tmp/ni-demo/a.sock
```

One `WARN`, and then business as usual. The warning is deliberate rather than
silent: a stale socket means something died badly, and that is worth a line in the
log even though it is recoverable.

### 8.3 Two bots, one path

Leave that bot running and start another on the same socket:

```sh
./target/debug/ni-bot --listen unix:///tmp/ni-demo/a.sock
```

```
Error: another process is already listening on unix:///tmp/ni-demo/a.sock
```

`clear_stale_socket` connected, got an answer, and refused to touch the file. Had
it done the naive `remove_file` first, the *first* bot would still be running,
still holding a socket whose name now belongs to the second — invisible, reachable
by nobody, and impossible to diagnose from either process's logs. Compare with
TCP, where the kernel gives you `EADDRINUSE` for free and this class of bug cannot
be written.

That asymmetry is worth remembering as a general property: **filesystem-named
resources let you overwrite somebody else's registration; port-named ones do
not.** Every UDS service needs the check that Checkpoint 2 wrote; almost none of
the tutorials have it.

### 8.4 A path that does not fit

```sh
./target/debug/ni-engine run \
  --bot-a target/debug/ni-bot --bot-b target/debug/ni-bot \
  --transport unix \
  --socket-dir "/tmp/$(python3 -c 'print("d"*90)')" \
  --no-match-log --quiet
```

```
Error: socket path is 110 bytes, over the 100-byte limit:
/tmp/dddd…dddd/m5-demo/a.sock — pass a shorter --socket-dir
```

That error came from `SocketDir::socket`, before any syscall. Without the check
you would get `EINVAL` from `bind` with no mention of length, on a path that looks
perfectly reasonable to a human. The scenario is not contrived: nested temp
directories in CI, per-test unique names, and container mount paths all push past
108 bytes easily, and this is *the* classic Unix-socket production surprise.

Worth knowing about the workaround people reach for: `chdir` to the directory and
`bind("a.sock")` with a relative path, since the limit applies to the string you
pass rather than the resolved path. It works, and it makes your process's working
directory load-bearing, which is usually a worse problem than the one it solves.

### 8.5 Nobody home: two different silences

```sh
cargo run --release -p ni-bench --example dial_errors
```

```
unix:///tmp/ni-missing.sock
    could not connect to unix:///tmp/ni-missing.sock: transport error: No such file or directory (os error 2): No such file or directory (os error 2)
unix:///tmp/ni-stale.sock
    could not connect to unix:///tmp/ni-stale.sock: transport error: Connection refused (os error 111): Connection refused (os error 111)
tcp://127.0.0.1:1
    could not connect to tcp://127.0.0.1:1: transport error: tcp connect error: tcp connect error: Connection refused (os error 111)
```

Three failures, three distinct meanings, and the Unix socket carries *more*
information than the TCP one:

| Situation | errno | What you learn |
|---|---|---|
| UDS path does not exist | `ENOENT` (2) | The service was never here, or its directory is not mounted |
| UDS file exists, nobody listening | `ECONNREFUSED` (111) | The service *was* here and died — a stale socket |
| TCP port closed | `ECONNREFUSED` (111) | Something is listening on the interface; nothing on that port |

The first two are the same situation over TCP — a closed port is a closed port —
and over UDS they are different diagnoses with different fixes. "Did you forget to
bind-mount `/run/containerd`?" and "did containerd crash?" are distinguishable by
errno, which is a small practical gift you get for free from having a filesystem
in the middle.

### 8.6 The same failures, over a Unix socket

M3 built Roger to misbehave and M3's whole taxonomy was derived over loopback
TCP. Re-run two of those experiments over a socket:

```sh
./target/debug/ni-engine run \
  --bot-a target/debug/ni-bot --bot-b target/debug/roger-the-shrubber \
  --bot-b-arg --sleep-ms --bot-b-arg 400 \
  --turn-deadline 100ms --transport unix --no-match-log --quiet
```

```
WARN …ni.transport="unix"…rpc.method="GetOrders"…ni.latency_us=101060}: GetOrders failed turn=2 chapter="B" code=Cancelled
WARN …: turn forfeited turn=2 chapter="B" reason=Timeout detail=no answer within 100ms (Cancelled: Timeout expired) strike=1 limit=3
…
result: chapter A wins - chapter B forfeits (missed deadline)
```

`Cancelled: Timeout expired`, at 101 ms, three strikes, forfeit — **identical** to
M3's transcript over TCP. The deadline still travelled as `grpc-timeout` in the
request metadata, the client-side timer still fired first, and the engine still
cannot tell whether Roger decided anything. Post 4's ambiguity is completely
intact.

And the crash:

```sh
./target/debug/ni-engine run \
  --bot-a target/debug/ni-bot --bot-b target/debug/roger-the-shrubber \
  --bot-b-arg --crash-on-turn --bot-b-arg 4 \
  --transport unix --no-match-log --quiet
```

```
WARN …ni.transport="unix"…: GetOrders failed turn=4 chapter="B" latency_us=1554 code=Unknown
WARN …: chapter cannot continue turn=4 chapter="B" reason=Crash detail=Unknown: transport error (process exited with code 101)
result: chapter A wins - chapter B forfeits (unreachable)
```

Run the same command with `--transport tcp` and compare line for line:

```
WARN …ni.transport="tcp"…: GetOrders failed turn=4 chapter="B" latency_us=1625 code=Unknown
WARN …: chapter cannot continue turn=4 chapter="B" reason=Crash detail=Unknown: transport error (process exited with code 101)
```

The same status, the same message, the same forfeit reason, the same exit code.
A server dying mid-call is a property of *there being a server*, and taking the
network away does not make a process immortal.

This is the experiment the blog trace asked for by name — "the same failure
experiments from post 4, re-run over UDS — the ambiguous case should still be
there". It is still there.

### 8.7 Who is on the other end

The one place the answers differ. Run both and read the `peer=` field:

```sh
for t in tcp unix; do
  ./target/debug/ni-engine run --bot-a target/debug/ni-bot --bot-b target/debug/ni-bot \
    --transport $t --match-id peer-$t --no-match-log --quiet 2>&1 |
    grep -m1 identified
done
```

```
INFO rpc{…/Identify…}: identified protocol=1 peer=tcp 127.0.0.1:52290
INFO rpc{…/Identify…}: identified protocol=1 peer=unix pid=32650 uid=0 gid=0
```

Then try to do something with each. Over TCP: nothing. There is no local API that
turns `127.0.0.1:52290` into a process — the port is ephemeral, it will be reused,
and by the time you looked it up the connection may belong to someone else.

Over UDS: `/proc/32650/cmdline` tells you it is `ni-engine`, `uid=0` is a policy
input, and neither value came from the caller. A production bot could refuse any
connection whose uid is not the one it expects, in four lines, with no shared
secret, no certificate and no token — because the kernel already knows and is
willing to say.

If you want to see the socket from the outside while a match runs, the tools are
different from the TCP ones:

```sh
ss -xl | grep ni          # unix sockets, listening (not `ss -tln`)
lsof -U | grep ni         # by process
cat /proc/net/unix | grep ni
```

`tcpdump` cannot help you here at all. That is the honest cost of the security
property: traffic that no packet capture can see is also traffic no packet capture
can debug, and you reach for `strace -f -e trace=network`, an eBPF tool, or a
proxy in the middle instead.

---

## What tonic actually returns over a Unix socket

M3 built this table over loopback TCP with the instruction to observe rather than
assume. Observed again, with tonic 0.13 over both transports:

| Situation | Over loopback TCP | Over UDS | `classify` |
|---|---|---|---|
| `set_timeout` deadline elapses | `Cancelled: Timeout expired` | **same** | `Timeout` |
| Bot process exits during a call | `Unknown: transport error` | **same** | `Unreachable` |
| Call to an already-dead process | `Unavailable` | **same** | `Unreachable` |
| Bot returns `FAILED_PRECONDITION` / `SHRUBBERY_REQUIRED` | as sent | **same** | `NeedsShrubbery` |
| Bot returns any other status | as sent | **same** | `Protocol` |
| **Dial** with no listener at the target | `ECONNREFUSED` | **`ENOENT`** — path missing | (start-up failure) |
| **Dial** at a stale socket file | n/a | `ECONNREFUSED` | (start-up failure) |

Every row that describes a *gRPC* failure is identical. The only rows that differ
are the ones that happen before gRPC exists, at `connect` time, where the
difference is not a status code but an errno — and, as 8.5 argued, the extra errno
is a small gain rather than a complication.

The practical consequence: **`policy.rs` needed no changes in M5.** Not one line.
The failure taxonomy M3 derived from watching tonic over TCP turned out to be a
taxonomy of the *boundary*, and it transferred to a different transport without
anyone checking that it would. That is worth a paragraph in post 6 — it is the
same claim as the medians, arriving from the failure-handling side.

## Follow one `GetOrders` through both transports

Turn 6, chapter B, 500 ms deadline. Left column TCP, right column UDS; the
identical steps are stated once.

1. `run_match` picks `bot_b`, builds the turn span, calls `request_orders`.
2. `call_get_orders` builds `GetOrdersRequest { turn: 6, view }`, calls
   `set_timeout(500ms)`, injects `traceparent`, starts an `Instant`.
3. tonic serializes the message with prost, prepends the 5-byte gRPC length
   prefix, and writes HTTP/2 HEADERS + DATA frames — including
   `grpc-timeout: 500m`, `:authority`, and the trace context.
4. **The one different step**:

   | TCP | UDS |
   |---|---|
   | hyper writes to a `TcpStream`; the kernel segments to MSS, adds TCP and IP headers, looks up the route to `127.0.0.1`, traverses netfilter, delivers up the `lo` device, strips headers, queues to the listener's socket | hyper writes to a `UnixStream` via `TokioIo`; the kernel copies the payload into the peer socket's receive queue |

5. The bot's tonic server reads the frames, decodes the message, and — over UDS
   only — has `UdsConnectInfo` in the request extensions.
6. `server_span` extracts the trace context; the handler picks orders; tonic
   serializes the response and writes it back the way it came.
7. `call_get_orders` stops the `Instant`, records `ni.latency_us`, logs
   `GetOrders answered`.
8. `orders_or_protocol_error` checks the echoed turn; `apply_orders` resolves the
   turn in `ni-game`; `MatchLog` writes a JSONL line carrying
   `"transport": "unix"`.

**Eight steps, one of which differs, and it is not a step the application can
observe.** Every deadline, status, span, retry and rule in the system sits above
step 4. That is the diagram post 5 should open with.

## Who owns what

| Concern | Owner | Where |
|---|---|---|
| `AF_UNIX`, `SO_PEERCRED`, permissions, `sun_path` | the kernel | not your code |
| `UnixListener`, `UnixStream`, signals | Tokio | `server.rs`, `transport.rs` |
| Adapting tokio IO to hyper IO | hyper-util | `TokioIo` in `dial_unix` |
| Turning a closure into a connector | tower | `service_fn` in `dial_unix` |
| Erasing the transport behind one type | tonic | `Channel`, `Connected`, `*ListenerStream` |
| Which family a match uses | Ni | `--transport`, `Transport` |
| What a socket is *called* | Ni | `SocketDir`, `Listen` |
| Who deletes it | Ni, at two layers | `SocketFile` (bot), `SocketDir` (engine) |
| What a status *means* | Ni (M3) | `policy.rs`, unchanged |
| Whether an order is legal | Ni (M1) | `ni-game`, still untouched |

The interesting rows are the last three. M5 added a transport and changed nothing
about meaning — and the two rows that *are* new (naming and deleting) are both
consequences of the same fact: a Unix socket is a file, and files need owners.

---

## Beginner troubleshooting

### `the trait bound ... UnixStream: hyper::rt::Read ... is not satisfied`

You passed the `UnixStream` to `connect_with_connector` directly. Wrap it:
`TokioIo::new(UnixStream::connect(path).await?)`. hyper 1.0 has its own IO
traits and `hyper-util` exists to bridge them.

### `type annotations needed` inside the connector closure

The `async` block's error type is unconstrained. Keep the turbofish:
`Ok::<_, std::io::Error>(TokioIo::new(…))`. This is the single most common
compile failure in a hand-written tonic UDS client.

### `cannot find function service_fn in crate tower`

The `util` feature is missing: `tower = { version = "0.5", features = ["util"] }`.
Same shape of problem if `hyper_util::rt` is missing — that needs
`features = ["tokio"]`.

### `Address already in use (os error 98)` on a socket that nobody is using

A stale socket file. That is what `clear_stale_socket` is for; if you are seeing
this, either it is not being called or the file was created by another user and
`remove_file` failed. `ls -l` the path and check the owner.

### `Invalid argument (os error 22)` from `bind`

Almost always path length. `SocketDir::socket` should have caught it — unless you
bypassed it, or you are on macOS where the limit is 104 rather than 108. Print
`path.as_os_str().len()` and compare.

### `No such file or directory` when dialling a socket that exists

Check the *directory* permissions, not the socket's. Reaching a socket needs
execute permission on every directory in the path, and `SocketDir` sets `0700` —
so a second user, or a process in a different container without the bind mount,
gets `ENOENT` or `EACCES` rather than a connection. This is the access control
working.

### The bot binds but the engine never connects, and there is no error

The socket file was deleted between `bind` and `connect`. The usual cause is
writing `let _ = SocketFile(path)` instead of `let _socket_file = SocketFile(path)`
— `let _ =` drops the value immediately, and the guard unlinks the file on drop.

### `Error: listen target must begin with tcp:// or unix://`

Two slashes, then an absolute path: `unix:///tmp/ni/a.sock`. Three slashes total.
`unix://tmp/ni/a.sock` parses to the relative path `tmp/ni/a.sock`, which will
bind somewhere surprising.

### Sockets pile up in `$XDG_RUNTIME_DIR/ni`

Something is being `SIGKILL`ed — the engine, or the bots without going through
`shutdown()`. Check for orphans with `pgrep -f ni-bot`, and remember that
Checkpoint 8.2 is the expected behaviour rather than a bug. A `--socket-dir` under
`/tmp` gets swept eventually; `$XDG_RUNTIME_DIR` is cleared at logout.

### `error[E0061]: this function takes 4 arguments but 3 were supplied`

`spawn_with_args` gained the `listen` parameter. Every call site needs
`&Listen::Tcp` or a real path — `main.rs`, `failure_modes.rs`, `transports.rs`,
`ni-bench`.

### The benchmark prints thousands of INFO lines

`RUST_LOG` is set in your shell, so the harness's `var_os` check leaves it alone.
Unset it, or set it to `warn` explicitly. And do not trust numbers from a run that
logged per call — see Checkpoint 6.5.

### The 1 MiB benchmark row takes minutes

You are on a debug build. `--release`, always, for anything in Checkpoint 6 or 7.
If a *unit test* is what takes minutes, you have the quadratic
`replay_of_at_least` — see Checkpoint 6.3.

### UDS looks slower than TCP on my machine

That is a legitimate result and this workbook's own transcripts show it happening.
Run it several times, check whether the difference exceeds the run-to-run spread,
and look at the `>10ms` column rather than only the median. Checkpoint 7 is
entirely about not over-reading one table.

### `SocketDir` deletes a directory I did not expect

Check what you passed as `--match-id`. It becomes a path component (sanitised to
`[A-Za-z0-9-]`), and `SocketDir::create` will happily `create_dir_all` a name that
already exists — then remove it on drop. Point `--socket-dir` at a scratch
location if you are experimenting.

## Notes to collect for blog posts 5 and 6

Write these down while the work is fresh; they are the parts you will not
remember later.

**For post 5 — "gRPC without the network":**

- The twelve-line connector, and why each line is there: the fake `:authority`,
  the discarded `Uri`, the two clones, the `TokioIo` wrapper, the turbofish. This
  is the code readers came for.
- That tonic makes you write it while grpc-go and grpc-java parse `unix:` targets
  for you — a fair observation about the Rust ecosystem's preference for explicit
  seams over convenient defaults.
- The client/server symmetry: `serve_with_incoming_shutdown` takes a stream of
  connections and does not care what produced them, which is why the two `serve`
  functions differ by six lines that are all about the filesystem.
- The whole lifecycle argument. A port is allocated and reclaimed by the kernel; a
  path is named and deleted by you. Two `Drop` guards in two processes, and a
  `SIGKILL` demonstration showing why one of them is not enough.
- `clear_stale_socket`, and the observation that "just `remove_file` first" —
  which most examples do — can silently steal a live service's name. TCP's
  `EADDRINUSE` makes that bug unwritable.
- Peer credentials as the argument that has nothing to do with speed, with the
  two log lines side by side and the note that the pid in the bot's log is the
  engine's. Then the containerd/CRI/device-plugin pattern, which is the same
  shape as a Ni bot.
- The 108-byte limit, because everybody hits it eventually and the error message
  does not mention length.
- `ENOENT` versus `ECONNREFUSED` as two distinguishable diagnoses that TCP
  collapses into one.

**For post 6 — "what's left when the wire goes away":**

- The medians, with the spread across runs, and the sentence that the difference
  did not exceed the variance. Resist the urge to find a percentage.
- The tail: 16–17 calls over 10 ms per 10,000 on TCP against 0–1 on UDS,
  repeatable across runs, clustering at 41–44 ms, and `DelayedACKs +50` versus no
  TCP counter moving at all. This is the strongest single result in the milestone.
- The 1 MiB attribution table: ~40–45% of the round trip is prost, decode costs
  twice encode because decode allocates ~76,000 strings, and the socket family
  accounts for about 4% — inside the noise.
- The HTTP/2 window hypothesis that was wrong, with its four measurements. A
  rejected hypothesis is evidence of method.
- The observer effect: the first version of the harness measured `tracing`.
  Instrumentation is work, and work inside the timed region is part of the
  measurement.
- `policy.rs` needed zero changes, and the failure taxonomy transferred untouched.
  The failure model is a property of the boundary, not of the wire.
- The diff size. One new module, one rewritten `serve`, four recorded fields in the
  match loop, and a new crate that exists only to measure. The rules engine has
  not been touched since M1.
- The honest closing caveat: this is one machine, and the harness ships so readers
  can produce their own row.

## M5 completion checklist

```sh
cargo fmt --check
cargo clippy --workspace --all-targets
cargo build --workspace
cargo test --workspace
cargo build --workspace --release
./target/release/ni-bench --bot target/release/ni-bot --iterations 5000 --warmup 500
```

- [ ] `transport.rs` owns every transport decision, and nothing else does
- [ ] `--transport tcp` behaves exactly as M4 did
- [ ] `--transport unix` plays a complete match
- [ ] The bot announces its scheme, and the engine dials what the bot announced
- [ ] A bot refuses to bind over a socket somebody is listening on
- [ ] A bot clears a stale socket, with a warning
- [ ] A bot unlinks its own socket on `SIGTERM`
- [ ] The engine's socket directory is `0700` and disappears with the match
- [ ] An over-long socket path fails with a message that says "too long"
- [ ] `Identify` logs kernel-supplied peer credentials over UDS
- [ ] `RunOptions.transport` is recorded four times and read for a decision zero times
- [ ] `policy.rs` is unchanged
- [ ] `ni-game` is unchanged, for the fourth milestone running
- [ ] `proto/ni/v1/ni.proto` is unchanged; `buf breaking` is quiet
- [ ] The match log's first line carries `"transport"`
- [ ] `cargo test --workspace` includes 6 passing transport-parity tests
- [ ] `ni-bench` runs both transports and prints measured payload sizes
- [ ] You have run the harness at least three times before believing any number
- [ ] You can state the median result *and* the tail result in one sentence each

M6 next, and it is now gated on real decisions rather than on code: daemon mode
with `/run/ni/*.sock` glob discovery, gRPC health checking, a tournament
round-robin, and a Python bot to make the contract argument concrete. The peer
credentials this milestone logs become an authorization check the moment the
engine stops spawning every bot itself — which is exactly what daemon mode means.
