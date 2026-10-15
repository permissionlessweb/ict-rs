//! Contract used by `ict-rs` example `bulk_memory_gas`.

#![feature(asm_experimental_arch)]

use cosmwasm_std::{
    entry_point, to_json_binary, Binary, Deps, DepsMut, Env, MessageInfo, Response, StdResult,
};
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct InstantiateMsg {}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ExecuteMsg {
    Copy { n: u32 },
    Fill { n: u32 },
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct QueryMsg {}

#[entry_point]
pub fn instantiate(
    _deps: DepsMut,
    _env: Env,
    _info: MessageInfo,
    _msg: InstantiateMsg,
) -> StdResult<Response> {
    Ok(Response::new().add_attribute("method", "instantiate"))
}

#[entry_point]
pub fn execute(
    _deps: DepsMut,
    _env: Env,
    _info: MessageInfo,
    msg: ExecuteMsg,
) -> StdResult<Response> {
    match msg {
        ExecuteMsg::Copy { n } => bulk_copy(n),
        ExecuteMsg::Fill { n } => bulk_fill(n),
    }
}

#[entry_point]
pub fn query(_deps: Deps, _env: Env, _msg: QueryMsg) -> StdResult<Binary> {
    to_json_binary(&"ok")
}

fn bulk_copy(n: u32) -> StdResult<Response> {
    // Address 0 is a null pointer and rustc rejects the copy. The length is what the meter sees.
    let dest: u32 = 64;
    let src: u32 = 8;
    let n = core::hint::black_box(n);
    let observed: u8;
    #[cfg(target_arch = "wasm32")]
    unsafe {
        core::ptr::copy_nonoverlapping(src as *const u8, dest as *mut u8, n as usize);
        observed = core::ptr::read_volatile(dest as *const u8);
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        observed = 0;
    }
    Ok(Response::new()
        .add_attribute("op", "copy")
        .add_attribute("n", n.to_string())
        .add_attribute("b", (observed as u32).to_string()))
}

fn bulk_fill(n: u32) -> StdResult<Response> {
    let dest: u32 = 0;
    let val: u32 = 0xAB;
    let n = core::hint::black_box(n);
    #[cfg(target_arch = "wasm32")]
    unsafe {
        core::arch::asm!(
            "local.get {dest}",
            "local.get {val}",
            "local.get {len}",
            "memory.fill 0",
            dest = in(local) dest,
            val = in(local) val,
            len = in(local) n,
            options(nostack),
        );
    }
    Ok(Response::new()
        .add_attribute("op", "fill")
        .add_attribute("n", n.to_string()))
}
