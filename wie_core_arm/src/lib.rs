#![no_std]
extern crate alloc;

mod allocator;
#[cfg(feature = "hooks")]
mod binary_patches;
mod context;
mod core;
mod engine;
mod function;
pub mod stdlib;
mod thread;
mod thread_wrapper;

#[cfg(not(target_arch = "wasm32"))]
mod gdb;

pub type ThreadId = usize;

pub use self::{
    allocator::Allocator,
    core::{ArmCore, RUN_FUNCTION_LR, RunFunctionResult},
    function::{EmulatedFunction, EmulatedFunctionParam, RegisteredFunction, RegisteredFunctionHolder, ResultWriter, SvcId},
};

#[cfg(feature = "hooks")]
pub use self::binary_patches::install_binary_patches;

#[cfg(not(feature = "hooks"))]
pub fn install_binary_patches(_core: &mut ArmCore, _data: &[u8], _scan_ranges: &[(u32, u32)]) -> wie_util::Result<usize> {
    Ok(0)
}
