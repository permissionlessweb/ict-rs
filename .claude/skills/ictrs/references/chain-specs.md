# Built-in Chain Specifications

`builtin_chain_config(name)` in `src/spec.rs` returns pre-configured `ChainConfig`.

## Available Chains

| Name | Binary | Prefix | Denom | Genesis Style |
|------|--------|--------|-------|---------------|
| `gaia` / `cosmoshub` | `gaiad` | `cosmos` | `uatom` | Legacy |
| `osmosis` | `osmosisd` | `osmo` | `uosmo` | Legacy |
| `terp` / `terpnetwork` | `terpd` | `terp` | `uterp` | Legacy |
| `juno` | `junod` | `juno` | `ujuno` | Legacy |
| `akash` | `akash` | `akash` | `uakt` | Modern |
| `anvil` / `ethereum` | `anvil` | (none) | `wei` | — |

Genesis Styles: **Legacy** = `init`, `add-genesis-account`, `collect-gentxs`. **Modern** (SDK 0.50+) = `genesis init`, `genesis add-account`, `genesis collect`.

## Usage

```rust
let spec = ChainSpec {
    name: "terp".into(),
    version: Some("v5.1.3".into()),
    num_validators: Some(2),
    ..Default::default()
};
let chain = spec.build_cosmos_chain(runtime)?;
```

Override fields on `ChainSpec` to customize. Unset fields keep built-in defaults. See `src/chain/mod.rs` for the full `ChainConfig` definition.
