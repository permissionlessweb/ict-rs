# Terp Network Modules

Feature-gated behind `terp`. Source: `ict-rs/src/cosmos/modules/`.

| Module | File | Purpose |
|--------|------|---------|
| tokenfactory | `tokenfactory.rs` | Create/mint/burn native denoms |
| feeshare | `feeshare.rs` | Fee distribution to contract devs |
| drip | `drip.rs` | Periodic token distribution |
| clock | `clock.rs` | Scheduled contract execution |
| hashmerchant | `hashmerchant.rs` | Hash-locked token trading |
| smartaccount | `smartaccount.rs` | Account abstraction |
| globalfee | `globalfee.rs` | Network-wide minimum fees |
| bme | `bme.rs` | BME module |

Chain extension: `src/chain/terp.rs` (feature-gated `#[cfg(feature = "terp")]`).

Config helpers: `TestEnv::terp_config()`, `TestEnv::terp_localterp_config()`.
