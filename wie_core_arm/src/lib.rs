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
    context::ArmCoreContext,
    core::{ArmCore, RUN_FUNCTION_LR, RunFunctionResult},
    function::{EmulatedFunction, EmulatedFunctionParam, JumpTo, RegisteredFunction, RegisteredFunctionHolder, ResultWriter, SvcId},
};

static HOOKS_ENABLED: ::core::sync::atomic::AtomicBool = ::core::sync::atomic::AtomicBool::new(true);

pub fn set_hooks_enabled(enabled: bool) {
    HOOKS_ENABLED.store(enabled, ::core::sync::atomic::Ordering::Relaxed);
}

#[cfg(feature = "hooks")]
pub fn install_binary_patches(core: &mut ArmCore, data: &[u8], scan_ranges: &[(u32, u32)]) -> wie_util::Result<usize> {
    if !HOOKS_ENABLED.load(::core::sync::atomic::Ordering::Relaxed) {
        return Ok(0);
    }
    self::binary_patches::install_binary_patches(core, data, scan_ranges)
}

#[cfg(not(feature = "hooks"))]
pub fn install_binary_patches(_core: &mut ArmCore, _data: &[u8], _scan_ranges: &[(u32, u32)]) -> wie_util::Result<usize> {
    Ok(0)
}
