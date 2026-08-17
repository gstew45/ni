# Ni — design outline

> A grid-battle arena where bot processes fight over gRPC. They demand shrubbery.

Two rival chapters of the Knights Who Say Ni, mirrored armies, one grid. Bots are separate processes; the engine calls them over gRPC. Engine sends a view of the battlefield, bot returns orders for its knights.

Ni is the running example for the **Wires & Boundaries** trace, so it exists to make boundary problems concrete: deadlines, cancellation, partially-applied calls, contract evolution, and running the whole thing over a Unix socket instead of a network.

This is a shape, not a spec. Everything below is meant to be argued with.

---

## Theme

Both sides are Knights Who Say Ni. Rival chapters, same doctrine, same units, same win condition. The armies are mirrored.

This is deliberate. Symmetry means any bot can play either seat, a tournament is just a round-robin, and there's no balance problem to solve instead of writing about gRPC. Asymmetric factions — knights versus a passerby, say — are a genuinely good later mode, and `NewMatch` already assigns armies so the protocol supports it. But as v1 it buys flavour at the cost of the thing being built.

Two identical groups of pedants shouting at each other across a shrubbery is also the funnier premise.

**Naming conventions to hold to:** binaries `ni-engine` and `ni-bot`, proto package `ni.v1`, sockets at `/run/ni/*.sock`. Short enough to type constantly, which is the real test.

---

## The game

A small tactical battle. Two armies on a grid, alternating turns, last chapter standing wins.

**Starting ruleset — deliberately minimal:**

- **Board:** 10x10 grid. Shrubbery tiles, symmetrically placed.
- **Armies:** 4 knights per side, mirrored starting positions.
- **Units:** position, HP, movement range, attack range, damage. One unit type in v1.
- **A turn:** each of your knights may move up to its range and then attack once, in an order you choose. Knights that don't act simply don't.
- **Combat:** deterministic. Attack in range, target loses HP. No randomness in v1 — you want reproducible matches while debugging a distributed system.
- **Victory:** eliminate the enemy chapter. Draw on a turn cap, decided on surviving HP.

**Shrubbery.** Terrain, and the theme earning its keep as a mechanic. Shrubbery tiles block line of sight and grant cover to a unit standing in them. Attacks across shrubbery are blocked; attacks into shrubbery are weakened.

This is doing real work: shrubbery placement is what makes positioning interesting, and once fog of war arrives, shrubbery is the thing that creates it. A mechanic that happens to be a joke rather than a joke bolted onto a mechanic.

**Why a grid battle carries the trace better than a board game:** the state is naturally large enough that serialization measurements mean something, a turn is *several* actions rather than one, and the ruleset has obvious expansion room without redesigning the protocol.

**Deliberately deferred — these are the future-concepts hooks:**

- *Fog of war* — the engine builds a per-bot view rather than shipping world state. Shrubbery already defines the sight rules, so this is mostly a question of what the engine chooses to send. Big one: it makes the observation message genuinely different from engine state, and it's where cheating prevention becomes real.
- *Shrubbery as objective* — capture-and-hold tiles as an alternative win condition. One that looks nice, and is not too expensive.
- *Unit types* — melee, ranged, support. Contract evolution with actual stakes.
- *A passerby* — a neutral non-combatant wandering the grid that either side can accost for points. Neutral-entity mechanics without the balance cost of full asymmetry.
- *Simultaneous turns* — both bots called concurrently, orders resolved together. Changes the concurrency story completely and is the natural excuse for streaming later.
- *Randomness with a seeded RNG* — engine-side only, so matches stay replayable.

**Constraint to hold onto:** the engine is the sole authority on legality and the sole holder of truth. Bots propose, engine disposes. If a bot could be trusted to validate its own orders, most of the interesting failure cases disappear — and once fog of war lands, a bot that self-validates is a bot that can cheat.

---

## Architecture

Three roles:

**Engine** (`ni-engine`) — owns match state, resolves orders, enforces rules and time limits, records results. This is the gRPC *client*: it calls out to bots.

**Bot** (`ni-bot`) — a process exposing a small service. Receives a view, returns orders. Untrusted by assumption: may be slow, may crash, may return garbage, may be written in another language.

**Runner / CLI** — starts a match, picks two bots, prints or streams the result. Thin on purpose.

**Note the direction of the call.** The engine calls the bot, not the other way round. This is worth being deliberate about — it's what makes the bot a plugin rather than a participant, and it's what makes the Unix socket ending natural (post 5's prior art, HashiCorp go-plugin and the Kubernetes device plugin API, works exactly this way).

### Two launch models

**Spawn-per-match (build this first).** Runner forks two bot processes, hands each a socket path on argv, waits for them to listen, plays the match, kills them. Bot lifetime equals match lifetime. Discovery is trivial, crash semantics are clean, state is per-match because there is no other kind. Costs process startup on every match, which starts to matter across a 200-game tournament.

**Long-running daemon (add second).** A bot starts independently, binds a socket in `/run/ni/`, and serves many matches. This is where things stop being free:

- *Discovery becomes a component.* A directory convention — drop your socket in `/run/ni/`, engine globs it — is cheap and probably right.
- *Concurrency arrives.* One daemon plays several matches at once, so `NewMatch` stops being a formality and becomes per-match state allocation keyed by match ID. Now there's a leak to worry about: a match that dies without `MatchEnded` leaves an entry forever.
- *Crash semantics get worse, which is good material.* A daemon crash takes out every in-flight match it was serving. On restart it has forgotten everything — see `SHRUBBERY_REQUIRED` below.
- *Health checking becomes necessary.* With spawn-per-match, a live process is a working process. With a daemon up for three days, you want to know it still answers before scheduling a tournament round. gRPC has a standard health checking protocol for exactly this.

The engine shouldn't care which model it's talking to. That contrast is a post in itself.

---

## Protocol surface

Keep it small. Five calls is enough for the whole trace.

| Call | Shape | Why it's here |
|---|---|---|
| `Identify` | unary | Bot announces name, version, supported protocol version. Gives post 2 a handshake to evolve. |
| `NewMatch` | unary | Match ID, army assignment, board layout, rules config. |
| `GetOrders` | unary | Battlefield view in, list of knight orders out. The hot path. |
| `MatchEnded` | unary | Result and reason. Somewhere to report forfeits. |
| `SubmitReplay` | unary | Full match log. Exists mainly to give post 6 a large payload to measure. |

**Message shapes worth sketching early:**

- `BattlefieldView` — board dimensions, shrubbery tiles, all visible knights with IDs and stats, whose turn, turn number, remaining time budget. Later: only what this chapter can see.
- `Orders` — a repeated list of `{unit_id, move_to, attack_target}`, each field optional. This being a *list* is the design's most useful property.

**Deliberately not in v1:** streaming. A bidi stream for the whole match is the obvious "better" design and it's the wrong first move — unary `GetOrders` is what makes deadlines and the ambiguous-retry case clean. Streaming is a later post that revisits this decision, likely alongside simultaneous turns.

**The list-of-orders property.** Because a turn is several actions, a bot can submit orders that are individually plausible but collectively invalid, or valid up to order 3 and illegal at order 4. That forces a real decision — reject the whole turn, or apply what's legal and stop? — and it's a much better example than a single illegal move. It also means a retried `GetOrders` could partially double-apply, which is exactly the ambiguity post 4 is about.

### `SHRUBBERY_REQUIRED`

Maps to gRPC's `FAILED_PRECONDITION` — the status meaning "the system isn't in a state where this call makes sense." It means: *you have not brought me a shrubbery, and I cannot proceed until you do.* Which is both the joke and a precise description of the status code.

Two triggers:

1. **Daemon restart.** Bot restarts, forgets everything, then `GetOrders` arrives for match 47. It has never heard of match 47. It replies `FAILED_PRECONDITION` / `SHRUBBERY_REQUIRED`, meaning *send me a `NewMatch` first*. The engine re-sends `NewMatch`, replays the position, and carries on — possible only because the engine holds all the truth. This is the recovery story, and the error name is doing real work.
2. **Version handshake.** `Identify` reveals the bot's protocol version is too old — it doesn't understand the current shrubbery. Refuse the match with the same code.

---

## Features, roughly in build order

**Core loop**
- Engine runs a match to completion between two bot processes
- Orders validated and resolved engine-side; invalid orders are a defined outcome, not a crash
- Shrubbery blocks line of sight and grants cover
- Match result printed by the runner
- Text rendering of the board per turn, so you can actually watch it

**Time control**
- Per-turn time limit, enforced as a gRPC deadline
- Bot exceeding it forfeits the turn (or the match — decide which, it matters)
- Optional: whole-match time budget, so a bot can think hard about the opening

**Failure handling**
- Bot crashes mid-match — engine records a forfeit rather than dying
- Bot returns malformed or illegal orders — same
- Bot hangs — deadline covers it, but check what the engine actually observes
- Bot returns `SHRUBBERY_REQUIRED` — engine re-sends `NewMatch` and replays
- **The interesting one:** timeout on `GetOrders`. Did the bot decide? If the engine retries, can orders partially double-apply? Design the answer in — a turn number echoed in request and response makes `GetOrders` safely retryable and gives post 4 its punchline.

**Observability**
- Trace context propagated engine → bot
- One span per `GetOrders`, so a slow bot is visible as a slow span
- Match log with per-turn timings and order counts

**Transport**
- Loopback TCP first, because it's the least setup
- Unix domain socket second, once the rest works
- Same service definition for both — the swap should be a config change, which is itself the point

**Nice-to-have, only if the work wants it**
- Tournament mode — round-robin, standings table
- Replay format and a way to step through a match
- `roger-the-shrubber` — the deliberately hostile test bot. Sleeps, crashes, returns nonsense, orders knights it doesn't own.
- A bot in a second language, to make the contract argument real
- gRPC health checking, once daemon mode exists

---

## Open questions to resolve before building

1. **Partial validity:** reject the whole turn on one bad order, or apply orders until the first illegal one? Strictness is easier to reason about; partial application creates better failure stories.
2. **Timeout = lose the turn or lose the match?** Losing the match is simpler and harsher; losing the turn keeps games alive and creates more interesting retry questions.
3. **Is `GetOrders` retryable?** Decide deliberately. Echoing a turn number makes it idempotent; leaving it non-idempotent gives post 4 a sharper example. You could do both and write about the difference.
4. **Do bots hold state between turns?** Stateless is the better default in spawn-per-match. Daemon mode answers this for you — stateless per call, but the daemon holds a match-ID map, with the leak that implies.
5. **How much does the view send?** Full world state now and fog of war later is the easy path, but designing the view as *already* a projection — even when it happens to be complete — saves a breaking change.
6. **Shrubbery placement:** fixed symmetric layouts, or generated? Fixed keeps matches reproducible and comparable, which matters for benchmarking.

---

## What each trace post takes from this

- **1 — Why gRPC exists:** the bot contract as the thing REST would make awkward. Two languages, one definition, a non-trivial message shape.
- **2 — Contracts with teeth:** add a unit type, or a cover value to shrubbery tiles. Do old bots still run?
- **3 — Context propagation:** turn time limit *is* a deadline. Trace context engine → bot is a real debugging need.
- **4 — Everything fails differently:** timeout ambiguity on `GetOrders`, with partially-applicable actions behind it. `SHRUBBERY_REQUIRED` as the worked example of a status code that means something specific.
- **5 — gRPC without the network:** untrusted bot code, separate process, same machine. The plugin architecture, and the daemon-vs-spawn contrast.
- **6 — What's left when the wire goes away:** same match, three transports, measured. Views and replays give you payloads worth measuring.
- **7 — When not to use gRPC:** what a browser-based bot would have cost you.
