# Rust API Guidelines checklist (C-*)

The official, authoritative checklist for idiomatic, interoperable Rust. Source:
<https://rust-lang.github.io/api-guidelines/checklist.html> (topical chapters at
<https://rust-lang.github.io/api-guidelines/>). Not every item applies to a binary
app, but the naming, error, docs, and type-safety items apply to all Rust.

## Naming
- **C-CASE** — Casing conforms to RFC 430 (`CamelCase` types, `snake_case` values/fns/modules, `SCREAMING_SNAKE_CASE` consts).
- **C-CONV** — Ad-hoc conversions follow `as_` (cheap borrow), `to_` (expensive/owned), `into_` (consuming) conventions.
- **C-GETTER** — Getter names follow Rust convention (no `get_` prefix; `get` only for indexed/unchecked access).
- **C-ITER** — Methods on collections that produce iterators are named `iter`, `iter_mut`, `into_iter`.
- **C-ITER-TY** — Iterator type names match the methods that produce them (`iter()` → `Iter`, `into_iter()` → `IntoIter`).
- **C-FEATURE** — Feature names are free of placeholder words ("use-", "with-").
- **C-WORD-ORDER** — Names use a consistent word order across the crate.

## Interoperability
- **C-COMMON-TRAITS** — Types eagerly implement `Copy`, `Clone`, `Eq`, `PartialEq`, `Ord`, `PartialOrd`, `Hash`, `Debug`, `Display`, `Default` where sensible.
- **C-CONV-TRAITS** — Conversions use the standard `From`, `TryFrom`, `AsRef`, `AsMut` traits.
- **C-COLLECT** — Collections implement `FromIterator` and `Extend`.
- **C-SERDE** — Data structures implement serde's `Serialize`/`Deserialize` (this repo does, for `Config`).
- **C-SEND-SYNC** — Types are `Send` and `Sync` where possible.
- **C-GOOD-ERR** — Error types are meaningful and well-behaved (implement `Error`, `Display`, `Debug`; not just strings).
- **C-NUM-FMT** — Binary number types provide `Hex`, `Octal`, `Binary` formatting.
- **C-RW-VALUE** — Generic reader/writer fns take `R: Read`/`W: Write` by value.

## Macros
- **C-EVOCATIVE** — Input syntax is evocative of the output.
- **C-MACRO-ATTR** — Macros compose well with attributes.
- **C-ANYWHERE** — Item macros work anywhere items are allowed.
- **C-MACRO-VIS** — Item macros support visibility specifiers.
- **C-MACRO-TY** — Type fragments are flexible.

## Documentation
- **C-CRATE-DOC** — Crate-level docs are thorough and include examples.
- **C-EXAMPLE** — All items have a rustdoc example.
- **C-QUESTION-MARK** — Examples use `?`, not `try!`, not `unwrap`.
- **C-FAILURE** — Function docs include error, panic, and safety considerations.
- **C-LINK** — Prose contains intra-doc hyperlinks to relevant items (e.g. `[`Config::save`]`).
- **C-METADATA** — `Cargo.toml` includes authors, description, license, homepage, documentation, repository, keywords, categories.
- **C-RELNOTES** — Release notes document all significant changes.
- **C-HIDDEN** — Rustdoc does not show unhelpful implementation details.

## Predictability
- **C-SMART-PTR** — Smart pointers do not add inherent methods.
- **C-CONV-SPECIFIC** — Conversions live on the most specific type involved.
- **C-METHOD** — Functions with a clear receiver are methods.
- **C-NO-OUT** — Functions do not take out-parameters (return tuples/structs instead).
- **C-OVERLOAD** — Operator overloads are unsurprising.
- **C-DEREF** — Only smart pointers implement `Deref`/`DerefMut`.
- **C-CTOR** — Constructors are static, inherent methods (`new`, `with_*`).

## Flexibility
- **C-INTERMEDIATE** — Functions expose intermediate results to avoid duplicate work.
- **C-CALLER-CONTROL** — Caller decides where to copy and place data (take references, don't force clones).
- **C-GENERIC** — Functions minimize assumptions by using generics (`impl AsRef<str>`, etc.).
- **C-OBJECT** — Traits are object-safe if they may be useful as a trait object.

## Type safety
- **C-NEWTYPE** — Newtypes provide static distinctions.
- **C-CUSTOM-TYPE** — Arguments convey meaning through types, not bare `bool`/`Option`.
- **C-BITFLAG** — Sets of flags are `bitflags`, not enums.
- **C-BUILDER** — Builders enable construction of complex values.

## Dependability
- **C-VALIDATE** — Functions validate their arguments.
- **C-DTOR-FAIL** — Destructors never fail.
- **C-DTOR-BLOCK** — Destructors that may block have alternatives.

## Debuggability
- **C-DEBUG** — All public types implement `Debug`.
- **C-DEBUG-NONEMPTY** — Debug representation is never empty.

## Future proofing
- **C-SEALED** — Sealed traits protect against downstream implementations.
- **C-STRUCT-PRIVATE** — Structs have private fields (except plain data carriers).
- **C-NEWTYPE-HIDE** — Newtypes encapsulate implementation details.
- **C-STRUCT-BOUNDS** — Data structures do not duplicate derived trait bounds on the struct.

## Necessities
- **C-STABLE** — Public dependencies of a stable crate are stable.
- **C-PERMISSIVE** — Crate and its dependencies have a permissive license (this repo: MIT).
