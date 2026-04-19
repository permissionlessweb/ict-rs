# Derive Macros & Codegen

## ict-rs-derive

Proc macro crate providing `ExecuteFns` and `QueryFns` derive macros for typed chain interactions.

### ExecuteFns

Auto-generates typed execute message functions from an enum:

```rust
use ict_rs::prelude::*;

#[derive(ExecuteFns)]
pub enum ExecuteMsg {
    Transfer { to: String, amount: u128 },
    Approve { spender: String, amount: u128 },
}

// Generates: chain.transfer(to, amount) and chain.approve(spender, amount)
```

### QueryFns

Auto-generates typed query functions:

```rust
#[derive(QueryFns)]
pub enum QueryMsg {
    Balance { address: String },
    TokenInfo {},
}

// Generates: chain.balance(address) and chain.token_info()
```

### Source Files

- `ict-rs-derive/src/lib.rs` — Proc macro entry points
- `ict-rs-derive/src/attrs.rs` — Attribute parsing
- `ict-rs-derive/src/execute.rs` — ExecuteFns implementation
- `ict-rs-derive/src/query.rs` — QueryFns implementation

## ict-rs-codegen

Proto-to-Rust code generation crate. Parses protobuf definitions and generates typed Rust interaction code.

### Source Files

| File | Purpose |
|------|---------|
| `src/lib.rs` | Library root |
| `src/bin/generate.rs` | CLI binary for code generation |
| `src/proto_parser.rs` | Parse .proto files into intermediate representation |
| `src/msg_codegen.rs` | Generate execute message types and functions |
| `src/query_codegen.rs` | Generate query message types and functions |
| `src/naming.rs` | Name conversion utilities (snake_case, CamelCase) |
| `src/cli_mapping.rs` | Map proto definitions to CLI command structures |
| `src/text_parser.rs` | Text parsing utilities |

## ict-rs-cw-orch

Adapter crate bridging ict-rs and cw-orch (CosmWasm Orchestrator).

| File | Purpose |
|------|---------|
| `src/lib.rs` | Module root, trait implementations |
| `src/convert.rs` | Type conversions between ict-rs and cw-orch types |
| `src/error.rs` | Error type bridging |

Depends on `cw-orch-core` (zk-mvp branch).
