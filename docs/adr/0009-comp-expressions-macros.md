# ADR 0009: Comp parameter expressions, macros and templates (fork, C4)

- **Status:** accepted for the fork (2026-10-22, C4 step 1; written by the integrator in a FULL /
  Opus run without a separate designer: **provisional**, open to the owner's change like any §1a
  choice). Scope set by [`COMPOSITOR-PLAN.md`](../../COMPOSITOR-PLAN.md) §"Phase C4" and FORK-PLAN
  §1a.126. (HANDOFF called this "ADR 0008"; 0008 is taken by the nearby transfer, so 0009.)
- **Builds on:** ADR 0007 (node graph), C1's `Comp` / `CompNode` / `CompKind`
  (`crates/project/src/comp.rs`), `Param` (`crates/project/src/keyframe.rs`).

## Context

Compositors (Fusion, Nuke, After Effects) let a parameter be a small formula: "rotate with time",
"this blur follows that node's size", "wiggle". Formulas come from project files and from agents, so
they are hostile input: they must never crash, hang, allocate without bound or poison a render
with NaN. Users also want to package a few nodes as one ("macro") with a handful of exposed
controls, and to save a node setup to reuse in other comps ("template").

## Decision

### 1. Expressions

1. **Where.** A pure module `filmcraft_project::expr` (no new crate, no dependencies beyond std;
   wasm-safe). `Param` gains `expression: Option<String>` (`#[serde(default, skip_serializing_if =
   "Option::is_none")]`), so old projects load unchanged and saving a project without expressions
   is byte-identical. C4 evaluates expressions on **comp node parameters only**; timeline effect
   parameters keep the stored field but ignore it (a later phase can opt in through the same seam).
2. **Language** (our own small grammar, Fusion / AE-like, no loops, no assignment, no I/O):
   numbers (f64), vectors `[a, b]` / `[a, b, c, d]`, booleans as 0 / 1; operators `+ - * / %`,
   `^` (power), unary `-` / `!`, comparisons, `&&` / `||`, `cond ? a : b`, indexing `v[0]`,
   `.x .y .z .w` / `.r .g .b .a`. Vector arithmetic is element-wise, scalars broadcast.
   **Names:** `time` (comp seconds as f64, for formulas only, never edit math), `frame` (integer
   frame from the Tick and the comp rate), `fps`, `width`, `height`, `value` (this parameter's
   keyframed value at this time), `pi`, `e`; `self.<param>` and `node("<label or id>").<param>`
   read other parameters at the same time. **Functions:** `sin cos tan asin acos atan atan2 sqrt
   abs sign min max clamp floor ceil round fract exp ln log10 pow lerp smoothstep step`, `noise(x
   [, seed])` (deterministic 1-D gradient noise, −1..1), `random(seed)` (hash → 0..1, no state),
   `wiggle(freq, amp [, seed])` (= `value + amp × noise(time × freq, seed)` per component).
3. **Bounds (never crash, never hang).** Source ≤ 4 096 bytes; ≤ 1 024 AST nodes; parser nesting
   ≤ 64 (iterative-safe recursive descent with an explicit depth counter); evaluation is one walk
   of the AST (no loops in the language, so it is O(nodes)) with a step counter as a belt-and-braces
   cap; `node()` / `self.` references resolve recursively with depth ≤ 8 and a visited set, so
   cycles are an error, not a stack overflow. *(C4.2:)* the AST is also at most 256 levels
   tall, and the heights of the expressions being evaluated one inside another through references
   add up to at most 768 (each level is one evaluator frame), so a reference chain of tall
   expressions is an error, never a stack overflow; one top-level read makes at most 256
   evaluations. Every function guards its domain (`sqrt(-1)`,
   `ln(0)`, `x / 0`, huge `pow`) and a **non-finite result is an error**.
4. **Errors.** Parse errors carry a byte span and a plain message ("expected `)` after `sin(time`"
   at column 9). When an expression fails to parse or evaluate, the parameter uses its keyframed /
   static value and the error is reported (inspector field, `comp.explain`, command result); a
   render never fails because of an expression.
5. **Types.** The result is coerced to the parameter's `ParamValue`: Float ← scalar (or `v[0]`),
   Vec2 ← 2-vector (scalar broadcasts; 3 or 4 parts are an error), Color ← 4-vector (3-vector keeps
   alpha; 2 parts are an error), Bool ← ≠ 0, Choice ← rounded and clamped to the schema's range.
   Text / Curve / Path / Mask parameters refuse expressions (`comp.setExpression` says so).
   *Auto points (C4.2 review):* a point parameter may hold NaN ("auto"). An expression reads such a
   coordinate as the comp centre's, which is what auto means for every comp node (their inputs
   are comp-sized, and the render resolves an auto point to the frame centre). The safe rule on
   the way back: if the expression returns exactly the point it read (e.g. `value`), the parameter
   stays auto (NaN), so nothing that resolves auto its own way is overridden; any other result is
   a plain point.
6. **Evaluation seam.** `CompNode::value_at(key, t)` stays the keyframe-only read; a new
   `Comp::param_at(node, key, t) -> (ParamValue, Option<ExprError>)` applies the expression with a
   resolver over the comp's nodes. The render's comp evaluation calls `param_at`. *(C4.2:)* the
   render evaluates every node's expressions through one `ExprFrame` per frame (each parameter
   evaluated once however many others read it), against the whole comp even when it compiles a
   subgraph (clone sources); parsed ASTs come from a bounded (512-entry) cache keyed by source in
   `expr::parse_cached` (pure, process-wide), not stored in the model.
   Animated-by-expression parameters (using `time` / `frame` / `wiggle` / animated references)
   make the node time-varying for the static-node cache; so do references to other nodes and
   `fps` / `width` / `height` (none of them is in the cached node's key).
7. **Commands.** `comp.setExpression { item, node, param, expression }` (empty string clears; one
   undo step; refuses bad params and over-long sources; returns the parse error, if any, without
   storing a broken expression unless `allowError: true`), `comp.clearExpression`. Inspector: an
   "=" toggle next to each keyframable comp parameter opens a one-line field with the error shown
   underneath; automation ids `comp.inspector.<param>.expr` / `.expr.toggle`.

### 2. Macros (groups)

1. `CompKind::Group { comp: Box<Comp>, exposed: Vec<ExposedParam> }`: an inner comp (its own nodes
   and edges, same size / rate as the outer comp) whose `MediaIn { source: GroupInput { index } }`
   nodes read the group node's inputs, and whose output is the group's output. `ExposedParam {
   id, label, node, param }` maps a group-level parameter (stored in the group node's own `params`)
   onto an inner node's parameter; the inner value is overridden by the group's at evaluation.
2. **Bounds:** nesting depth ≤ 4, inner nodes count toward the comp's node cap, exposed ≤ 64.
   Expressions inside a group resolve `node()` inside that group first, then the parent.
3. **Commands:** `comp.group` (selected node ids → one Group node, edges rewired, one undo step),
   `comp.ungroup`, `comp.group.expose` / `unexpose`, `comp.group.enter` / `exit` (UI navigation
   state only). A macro is a Group plus `comp.template.save`.

### 3. Templates

A template is a JSON file `<data dir>/templates/comp/<name>.fctemplate` holding `{ version, name,
nodes, edges, exposed? }` with node ids renumbered from 1 and no item references (`MediaIn {
Item }` nodes are dropped or turned into a group input). `comp.template.save { item, nodes, name }`,
`comp.template.list`, `comp.template.insert { item, name, at }` (fresh ids, one undo step),
`comp.template.delete`. Loading a template goes through the same `sanitize` path as a project, size
≤ 4 MiB.

## Consequences

- Project files stay compatible both ways for comps without expressions or groups; older builds
  ignore the `expression` field (serde default) but reject a `group` node kind (they fail to load
  that comp, like any unknown node kind).
- No grading or effect maths is duplicated: groups evaluate their inner comp through the same
  evaluator.
- **Alternatives rejected:** embedding an existing scripting engine (Rhai, Lua, JS): too large a
  surface for hostile files, harder to bound, and not needed for parameter formulas; a
  Python-compatible language: no pure-Rust bounded implementation.
