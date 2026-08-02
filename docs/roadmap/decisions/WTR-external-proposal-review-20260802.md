# Review — external water-roadmap proposal (2026-08-02)

**Source:** proposal supplied by Oliver Kay, authored by another AI assistant.
**Reviewed against:** the canonical roadmap's `WTR-` tickets, the
`.agents/CWR-CE Water System Master Plan.md` phase list, the water
implementation under `engine/WgpuRenderer/rust/src/water/`, and
[`RND-030-renderer-plan-reconciliation-20260802.md`](RND-030-renderer-plan-reconciliation-20260802.md).

**Verdict: adopt the substance, renumber every identifier.** The proposal is
sound and fills real gaps. The identifiers cannot be used as offered — but the
reason is a pre-existing defect in this repository, not a fault in the
proposal.

## 1. The real problem: two conflicting `WTR-` namespaces

This repository maintains **two independent, contradictory `WTR-` numbering
schemes**, and five identifiers already mean two different things:

| ID | Canonical roadmap | `.agents` Water Master Plan |
| --- | --- | --- |
| `WTR-100` | WaterBody registry and backend expansion | Underwater classification and waterline |
| `WTR-110` | Generalise current FFT | Underwater optics |
| `WTR-120` | Anti-repetition | Underwater god rays |
| `WTR-130` | Persistent foam | Surface-derived caustics |
| `WTR-140` | Reflection ownership | Underwater bubbles and local aeration |

Full inventories:

- **Canonical roadmap** (the authoritative ticket namespace): `WTR-001`,
  `WTR-GATE-1`, `WTR-GATE-2`, `WTR-100`, `WTR-110`, `WTR-120`, `WTR-130`,
  `WTR-140`, `WTR-200`, `WTR-210`, `WTR-220`, `WTR-221`, `WTR-230`.
- **Water Master Plan** (design document): `WTR-000` … `WTR-180` in steps of
  ten, plus sub-IDs.

**The `WTR-` tags in the source code follow the Master Plan, not the canonical
roadmap.** Tags such as `WTR-031`, `WTR-052`, `WTR-085` have no canonical
ticket at all. So a `WTR-` reference in this repository is ambiguous unless the
reader already knows which document was meant.

This is the finding that matters most here. It is exactly the class of defect
`RND-030` exists to surface, it predates the proposal, and it will keep
generating collisions until it is resolved.

## 2. Consequence for the proposal

The proposal's `WTR-150` / `WTR-160` / `WTR-170` / `WTR-180` are **free in the
canonical roadmap** but **occupied in the Master Plan** (volumetric fluid
rendering, weather integration, gameplay/buoyancy, FFT optimisation
respectively).

So the identifiers are usable in the strict sense — and unusable in practice,
because a reader hitting `WTR-170` cannot tell whether it means the new
interaction interface or the Master Plan's buoyancy phase.

**Recommendation: assign identifiers that are free in *both* namespaces**, so
they are unambiguous regardless of which document a reader has open.

### Correction to an earlier draft of this review

An earlier draft asserted the proposal's premise was inaccurate because "no
phase covers rivers, coasts or waterfalls". **That was wrong** — it checked the
Master Plan while the proposal was describing the canonical roadmap, which does
cover them (`WTR-220` Expanded river and coast integration, `WTR-221` Advanced
breakers and waterfalls). The proposal's summary of existing coverage was
accurate. The error is recorded rather than quietly removed, because it is the
same two-namespace confusion described above, made by a reader who had both
documents available.

## 3. Coverage assessment against the canonical roadmap

| Proposal | Canonical roadmap | Master Plan | Code | Verdict |
| --- | --- | --- | --- | --- |
| Optical model + underwater transition | Not covered | `WTR-050`, `WTR-100`–`WTR-120` | Absorption/scattering terms in `water/water.wgsl` | **New as a ticket**; scope against the Master Plan phases and shipped optics |
| Shoreline contact, wetness, waterline | `WTR-220` covers coast *inputs* (shore distance, bathymetry, slope, material) | Not covered | — | **Partly new**: the rendering side (wet terrain, object waterlines, run-up) is genuinely absent |
| Interaction event interface | Not covered | `WTR-060`, `WTR-070` | `water/interaction.rs`, `interaction.wgsl`, `water-interaction-emitters.md` | **New as a ticket**, but it is a *revision* of a shipped system, not greenfield |
| Temporal / lifecycle / invalidation | Not covered | Not covered | — | **Clean gap** |
| Water reference / conformance pack | Not covered | Not covered | — | **Clean gap**; correctly proposed as an extension of `TEST-002` |

Nothing in the proposal is redundant at ticket level. Two items overlap
existing design work and shipped code, and must be scoped as revisions so they
do not become a second implementation of a system that already exists — the
risk `RND-030` names explicitly.

## 4. Caveat that bounds this review

Per RND-030, **the Water Master Plan carries no status markers** — 113 phase
IDs, zero completion states. This review can say what each phase *is*, never
whether it is done. "Covered" below means *a phase exists*, not *it works*.

## Recommendation

1. **Adopt all five**, renumbered to identifiers free in both namespaces:
   `WTR-190`, `WTR-240`, `WTR-250`, `WTR-260`, and `TEST-WTR-001`.
2. Keep the proposal's acceptance criteria; only the identifiers change.
3. Scope the optics and interaction tickets **as revisions** of the Master Plan
   phases and shipped code named above.
4. Register them as `NON_BLOCKING_VALIDATION` / `CONDITIONAL_DEPENDENCY`, not
   activated during Preview 0 — matching the proposal's own scheduling advice.
5. **Resolve the two-namespace collision.** Until one scheme is authoritative
   and the other is renamed or cross-referenced, every future water proposal —
   human or AI — will keep landing on occupied identifiers.
