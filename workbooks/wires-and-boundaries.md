# Trace: wires-and-boundaries — "Wires & Boundaries"

gRPC, and what's left of it when you take the network away.

**The argument:** strip the network from a service boundary and you find out which costs were the network's and which were the boundary's. Serialization stays. The failure model stays — the server can still die mid-call, deadlines still fire, you still get "I don't know if it happened." What goes away is the hop. The expensive part of a service boundary was never the wire.

Starts with one committed piece; the rest are candidates that unlock as the work does.

---

## 1. Why gRPC exists

REST as the entry point, framed as problems-solved rather than a feature grid. Contract-first definitions, binary encoding, streaming as a first-class concept rather than a bolt-on.

*First up — this piece is planned.*

**To learn:** how protobuf encodes on the wire (varints, tag-length-value); what HTTP/2 contributes (multiplexing, header compression) and where people mistake HTTP/2 wins for gRPC wins.

**Ends on:** the boundary is expensive — so what does it buy?

---

## 2. Contracts with teeth

Protobuf field numbers, evolution rules, backward and forward compatibility as a discipline rather than a hope. Reserved fields. What actually breaks a consumer and what doesn't.

**To learn:** the compatibility rules in the protobuf language guide; what happens to unknown fields on round-trip.

---

## 3. Context propagation for real

Metadata, deadlines, cancellation — and how OTel trace context rides along. The post where the blog's metaphor stops being a metaphor.

**To learn:** deadline propagation across hops; how W3C `traceparent` travels in gRPC metadata.

---

## 4. Everything fails differently out here

Status codes and which of them tell you whether the work happened. Why retries need idempotency. How a retry storm forms. The ambiguous case — "definitely didn't happen," "definitely happened but you didn't hear," "who knows" — which has no equivalent in a local function call.

**To learn:** the gRPC status code list; `DEADLINE_EXCEEDED` semantics specifically, as the cleanest example of ambiguity.

**To build:** kill the server mid-call; add a sleep longer than the client deadline. Report what the client actually received.

---

## 5. gRPC without the network

Unix domain sockets (`unix:///path/to.sock`), named pipes on Windows. How you actually wire it up, what changes in the client target string, and what doesn't change at all.

**Prior art to cite:** containerd, the Kubernetes CRI, and the device plugin API all run gRPC over Unix sockets between processes on one host.

---

## 6. What's left when the wire goes away

The payoff. Measure UDS vs loopback TCP vs a real remote hop against the same service definition. Show that serialization cost and the failure model survive, and the hop doesn't.

**To build:** one measurement harness, three transports, one service definition.

- Empty call on each transport — the floor cost of the boundary itself
- Small vs large payload (1KB vs 1MB) — separates serialization scaling from per-call overhead
- Same failure experiments from post 4, re-run over UDS — the ambiguous case should still be there

Report as "my machine, this payload, this many iterations." That framing is unattackable and more interesting than a generic benchmark.

**Note:** build this harness while writing post 1 and post 1's numbers come for free — post 6 then becomes mostly writing.

---

## 7. When not to use gRPC *(optional closer)*

Browsers, public APIs, debuggability, the tooling tax. The post that makes the rest of the trace credible — a gRPC trace that never says where it's a bad fit reads like marketing.

---

## Notes on sequencing

- Posts 5 and 6 can slip without breaking the spine — useful, since they're the ones gated on the work.
- Streaming was cut. It fits as a later addition if the work surfaces it; forcing it in early turns post 1 into a feature tour.
- Consider ending each post with the question that opens the next. Makes the sequence read as discovered rather than pre-planned — which is what it is.

**Attributes:** `topic: grpc`
