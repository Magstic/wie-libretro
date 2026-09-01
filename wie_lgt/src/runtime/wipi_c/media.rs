use alloc::{boxed::Box, vec, vec::Vec};
use core::mem::size_of;

use bytemuck::{Pod, Zeroable};
use wie_backend::AudioHandle;
use wie_core_arm::{Allocator, ArmCore};
use wie_util::{Result, WieError, read_generic, read_null_terminated_string_bytes, write_generic};
use wie_wipi_c::{MethodBody, WIPICContext, WIPICResult};
use wipi_types::wipic::WIPICWord;

const MEDIA_STATE_ROOT: u32 = 0x7fff100c;
const PLAYER_ALLOCATED: u32 = 0x1000;
const PLAYING: u32 = 0x01;
const STOPPING: u32 = 0x02;

#[derive(Clone, Copy, Pod, Zeroable)]
#[repr(C)]
struct MediaState {
    first_clip: u32,
    last_clip: u32,
    clip_count: u32,
}

#[derive(Clone, Copy, Pod, Zeroable)]
#[repr(C)]
struct LgtClip {
    next: u32,
    previous: u32,
    device_id: i32,
    player_handle: i32,
    device_capabilities: u32,
    state_flags: u32,
    media_kind: u32,
    buffer_capacity: i32,
    available_bytes: i32,
    watermark_percent: i32,
    read_offset: i32,
    callback: u32,
    copied_type_string: u32,
    callback_user_data: u32,
    primary_buffer: u32,
    tone_buffer_0: u32,
    tone_buffer_1: u32,
    tone_buffer_2: u32,
    duration_buffer: u32,
    owning_process: u32,
    allocator_interface: u32,
    optional_vam_metadata: u32,
}

const _: [(); 88] = [(); size_of::<LgtClip>()];

pub fn init_process_state(core: &mut ArmCore) -> Result<()> {
    let state = Allocator::alloc(core, size_of::<MediaState>() as u32)?;
    write_generic(core, state, MediaState::zeroed())?;
    write_generic(core, MEDIA_STATE_ROOT, state)
}

fn state(context: &dyn WIPICContext) -> Result<(u32, MediaState)> {
    let state_ptr: u32 = read_generic(context, MEDIA_STATE_ROOT)?;
    if state_ptr == 0 {
        return Err(WieError::FatalError("LGT media state is not initialized".into()));
    }
    Ok((state_ptr, read_generic(context, state_ptr)?))
}

fn write_state(context: &mut dyn WIPICContext, state_ptr: u32, state: MediaState) -> Result<()> {
    write_generic(context, state_ptr, state)
}

fn read_clip(context: &dyn WIPICContext, clip: u32) -> Result<LgtClip> {
    read_generic(context, clip)
}

fn write_clip(context: &mut dyn WIPICContext, clip: u32, value: LgtClip) -> Result<()> {
    write_generic(context, clip, value)
}

fn find_clip(context: &dyn WIPICContext, wanted: u32) -> Result<Option<LgtClip>> {
    if wanted == 0 {
        return Ok(None);
    }
    let (_, state) = state(context)?;
    let mut current = state.first_clip;
    let mut remaining = state.clip_count;
    while current != 0 && remaining != 0 {
        let clip = match read_clip(context, current) {
            Ok(clip) => clip,
            Err(_) => return Ok(None),
        };
        if current == wanted {
            return Ok(Some(clip));
        }
        current = clip.next;
        remaining -= 1;
    }
    Ok(None)
}

fn alloc_zeroed(context: &mut dyn WIPICContext, size: u32) -> Result<u32> {
    let address = context.alloc_raw(size)?;
    if size != 0 {
        context.write_bytes(address, &vec![0; size as usize])?;
    }
    Ok(address)
}

fn try_alloc_zeroed(context: &mut dyn WIPICContext, size: u32) -> Option<u32> {
    alloc_zeroed(context, size).ok()
}

fn link_clip(context: &mut dyn WIPICContext, state_ptr: u32, mut state: MediaState, clip_ptr: u32, mut clip: LgtClip) -> Result<()> {
    clip.previous = state.last_clip;
    if state.last_clip != 0 {
        let mut last = read_clip(context, state.last_clip)?;
        last.next = clip_ptr;
        write_clip(context, state.last_clip, last)?;
    } else {
        state.first_clip = clip_ptr;
    }
    state.last_clip = clip_ptr;
    state.clip_count = state.clip_count.saturating_add(1);
    write_clip(context, clip_ptr, clip)?;
    write_state(context, state_ptr, state)
}

fn unlink_clip(context: &mut dyn WIPICContext, state_ptr: u32, mut state: MediaState, clip_ptr: u32, clip: LgtClip) -> Result<()> {
    if clip.previous != 0 {
        let mut previous = read_clip(context, clip.previous)?;
        previous.next = clip.next;
        write_clip(context, clip.previous, previous)?;
    } else {
        state.first_clip = clip.next;
    }
    if clip.next != 0 {
        let mut next = read_clip(context, clip.next)?;
        next.previous = clip.previous;
        write_clip(context, clip.next, next)?;
    } else {
        state.last_clip = clip.previous;
    }
    state.clip_count = state.clip_count.saturating_sub(1);
    write_state(context, state_ptr, state)?;
    context.free_raw(clip_ptr, size_of::<LgtClip>() as u32)
}

fn close_player(context: &mut dyn WIPICContext, clip: &mut LgtClip) {
    if clip.player_handle >= 0 {
        let handle = clip.player_handle as AudioHandle;
        let mut audio = context.system().audio();
        let _ = audio.close(handle);
    }
    clip.player_handle = -1;
    clip.state_flags &= !PLAYER_ALLOCATED;
}

fn circular_data(context: &dyn WIPICContext, clip: &LgtClip) -> Result<Vec<u8>> {
    let length = clip.available_bytes.max(0) as usize;
    let mut data = vec![0; length];
    if length == 0 {
        return Ok(data);
    }
    let capacity = clip.buffer_capacity as usize;
    let offset = clip.read_offset.max(0) as usize;
    for (index, byte) in data.iter_mut().enumerate() {
        let position = (offset + index) % capacity;
        context.read_bytes(clip.primary_buffer + position as u32, core::slice::from_mut(byte))?;
    }
    Ok(data)
}

fn schedule_callback(context: &mut dyn WIPICContext, callback: u32, user_data: u32, event: u32) {
    if callback == 0 {
        return;
    }

    struct Callback {
        callback: u32,
        user_data: u32,
        event: u32,
    }

    #[async_trait::async_trait]
    impl MethodBody<WieError> for Callback {
        async fn call(&self, context: &mut dyn WIPICContext, _: Box<[WIPICWord]>) -> Result<WIPICResult> {
            context.call_function(self.callback, &[self.user_data, self.event]).await?;
            Ok(WIPICResult { results: Vec::new() })
        }
    }

    let due = context.system().platform().now() + 1;
    context.set_timer(due, Box::new(Callback { callback, user_data, event }));
}

pub async fn clip_create(context: &mut dyn WIPICContext, ptr_type: WIPICWord, buffer_size: WIPICWord, callback: WIPICWord) -> Result<WIPICWord> {
    tracing::debug!("LGT MC_mdaClipCreate({ptr_type:#x}, {buffer_size:#x}, {callback:#x})");
    if ptr_type == 0 || (buffer_size as i32) < 0 {
        return Ok(0);
    }
    let type_bytes = read_null_terminated_string_bytes(context, ptr_type)?;
    let capacity = buffer_size;
    let type_ptr = match try_alloc_zeroed(context, type_bytes.len() as u32 + 1) {
        Some(ptr) => ptr,
        None => return Ok(0),
    };
    context.write_bytes(type_ptr, &type_bytes)?;
    context.write_bytes(type_ptr + type_bytes.len() as u32, &[0])?;
    let primary = if capacity == 0 {
        0
    } else {
        match try_alloc_zeroed(context, capacity) {
            Some(ptr) => ptr,
            None => return Ok(0),
        }
    };
    let clip_ptr = match try_alloc_zeroed(context, size_of::<LgtClip>() as u32) {
        Some(ptr) => ptr,
        None => return Ok(0),
    };
    let (state_ptr, state) = state(context)?;
    let clip = LgtClip {
        device_id: 3,
        player_handle: -1,
        device_capabilities: 0x0e,
        buffer_capacity: capacity as i32,
        watermark_percent: 100,
        callback,
        copied_type_string: type_ptr,
        primary_buffer: primary,
        callback_user_data: clip_ptr,
        ..LgtClip::zeroed()
    };
    link_clip(context, state_ptr, state, clip_ptr, clip)?;
    Ok(clip_ptr)
}

pub async fn clip_free(context: &mut dyn WIPICContext, clip_ptr: WIPICWord) -> Result<i32> {
    let Some(mut clip) = find_clip(context, clip_ptr)? else { return Ok(-9) };
    if clip.state_flags & (PLAYING | STOPPING) != 0 {
        return Ok(-8);
    }
    close_player(context, &mut clip);
    let (state_ptr, state) = state(context)?;
    if clip.copied_type_string != 0 {
        let type_size = read_null_terminated_string_bytes(context, clip.copied_type_string)
            .map(|bytes| bytes.len() as u32 + 1)
            .unwrap_or(0);
        if type_size != 0 {
            let _ = context.free_raw(clip.copied_type_string, type_size);
        }
    }
    if clip.primary_buffer != 0 {
        let _ = context.free_raw(clip.primary_buffer, clip.buffer_capacity.max(0) as u32);
    }
    unlink_clip(context, state_ptr, state, clip_ptr, clip)?;
    Ok(0)
}

pub async fn clip_put_data(context: &mut dyn WIPICContext, clip_ptr: WIPICWord, source: WIPICWord, requested: WIPICWord) -> Result<i32> {
    let Some(mut clip) = find_clip(context, clip_ptr)? else { return Ok(-9) };
    if source == 0 {
        return Ok(-9);
    }
    if requested == 0 {
        return Ok(0);
    }
    if clip.primary_buffer == 0 || clip.buffer_capacity <= 0 {
        return Ok(-17);
    }
    let capacity = clip.buffer_capacity as u32;
    let available = clip.available_bytes.max(0) as u32;
    let accepted = requested.min(capacity.saturating_sub(available));
    let mut copied = 0;
    while copied < accepted {
        let write_offset = (clip.read_offset.max(0) as u32 + available + copied) % capacity;
        let chunk = (accepted - copied).min(capacity - write_offset).min(1024);
        let mut bytes = vec![0; chunk as usize];
        let source_address = source.checked_add(copied).ok_or(WieError::AllocationFailure)?;
        context.read_bytes(source_address, &mut bytes)?;
        context.write_bytes(clip.primary_buffer + write_offset, &bytes)?;
        copied += chunk;
    }
    if accepted != 0 && clip.player_handle >= 0 {
        let handle = clip.player_handle as AudioHandle;
        let _ = context.system().audio().close(handle);
        clip.player_handle = -1;
        clip.state_flags &= !PLAYING;
    }
    clip.available_bytes = (available + accepted) as i32;
    write_clip(context, clip_ptr, clip)?;
    Ok(accepted as i32)
}

pub async fn clip_clear_data(context: &mut dyn WIPICContext, clip_ptr: WIPICWord) -> Result<i32> {
    let Some(mut clip) = find_clip(context, clip_ptr)? else { return Ok(-9) };
    clip.read_offset = 0;
    clip.available_bytes = 0;
    let result = if clip.state_flags & (PLAYING | STOPPING) == 0 { 0 } else { -1 };
    write_clip(context, clip_ptr, clip)?;
    Ok(result)
}

pub async fn clip_alloc_player(context: &mut dyn WIPICContext, clip_ptr: WIPICWord, _parameter: WIPICWord) -> Result<i32> {
    let Some(mut clip) = find_clip(context, clip_ptr)? else { return Ok(-9) };
    if clip.state_flags & PLAYER_ALLOCATED != 0 {
        return Ok(clip.player_handle);
    }
    clip.media_kind = 0;
    clip.callback_user_data = clip_ptr;
    clip.state_flags |= PLAYER_ALLOCATED;
    write_clip(context, clip_ptr, clip)?;
    Ok(0)
}

pub async fn clip_free_player(context: &mut dyn WIPICContext, clip_ptr: WIPICWord) -> Result<i32> {
    let Some(mut clip) = find_clip(context, clip_ptr)? else { return Ok(-9) };
    close_player(context, &mut clip);
    write_clip(context, clip_ptr, clip)?;
    Ok(0)
}

pub async fn play(context: &mut dyn WIPICContext, clip_ptr: WIPICWord, repeat: WIPICWord) -> Result<i32> {
    let Some(mut clip) = find_clip(context, clip_ptr)? else { return Ok(-9) };
    if clip.state_flags & PLAYER_ALLOCATED == 0 {
        return Ok(-1);
    }
    if clip.state_flags & (PLAYING | STOPPING) != 0 {
        return Ok(-8);
    }
    let handle = if clip.player_handle >= 0 {
        clip.player_handle as AudioHandle
    } else {
        let data = circular_data(context, &clip)?;
        match context.system().audio().load_smaf(&data) {
            Ok(handle) => handle,
            Err(_) => return Ok(-1),
        }
    };
    if context.system().audio().play(handle, repeat != 0).is_err() {
        let _ = context.system().audio().close(handle);
        clip.player_handle = -1;
        clip.state_flags &= !PLAYING;
        write_clip(context, clip_ptr, clip)?;
        return Ok(-1);
    }
    clip.player_handle = handle as i32;
    clip.state_flags |= PLAYING;
    write_clip(context, clip_ptr, clip)?;
    schedule_callback(context, clip.callback, clip.callback_user_data, 2);
    Ok(0)
}

pub async fn stop(context: &mut dyn WIPICContext, clip_ptr: WIPICWord) -> Result<i32> {
    let Some(mut clip) = find_clip(context, clip_ptr)? else { return Ok(-9) };
    if clip.state_flags & (PLAYING | STOPPING) == 0 {
        return Ok(-1);
    }
    if clip.media_kind == 4 || clip.media_kind == 5 {
        return Ok(-16);
    }
    if clip.player_handle >= 0 {
        context.system().audio().stop(clip.player_handle as AudioHandle);
    }
    clip.state_flags &= !(PLAYING | STOPPING);
    write_clip(context, clip_ptr, clip)?;
    schedule_callback(context, clip.callback, clip.callback_user_data, 3);
    Ok(0)
}

pub async fn clip_get_volume(context: &mut dyn WIPICContext, clip: WIPICWord) -> Result<i32> {
    if find_clip(context, clip)?.is_none() {
        return Ok(-9);
    }
    Ok(0)
}

pub async fn clip_set_volume(context: &mut dyn WIPICContext, clip: WIPICWord, _volume: WIPICWord) -> Result<i32> {
    if find_clip(context, clip)?.is_none() {
        return Ok(-9);
    }
    Ok(0)
}

pub async fn vibrator(context: &mut dyn WIPICContext, level: i32, timeout: i32) -> Result<i32> {
    context
        .system()
        .platform()
        .vibrate(timeout.max(0) as u64, (level.clamp(0, 10) * 10) as u8);
    Ok(0)
}

pub async fn get_mute_state(_context: &mut dyn WIPICContext, _source: WIPICWord) -> Result<i32> {
    Ok(0)
}

pub async fn set_mute_state(_context: &mut dyn WIPICContext, _source: i32, _mute: i32) -> Result<i32> {
    Ok(0)
}

#[cfg(test)]
mod tests {
    use alloc::{boxed::Box, collections::BTreeMap, vec::Vec};

    use bytemuck::Zeroable;
    use test_utils::TestPlatform;
    use wie_backend::{DefaultTaskRunner, Instant, System};
    use wie_util::{ByteRead, ByteWrite, Result, WieError, read_generic, write_generic};
    use wie_wipi_c::{WIPICContext, WIPICMethodBody};

    use super::{LgtClip, MEDIA_STATE_ROOT, MediaState, clip_clear_data, clip_create, clip_free, clip_put_data};

    struct TestContext {
        memory: BTreeMap<u32, u8>,
        next: u32,
        system: System,
    }

    impl TestContext {
        fn new() -> Self {
            Self {
                memory: BTreeMap::new(),
                next: 0x1000,
                system: System::new(Box::new(TestPlatform::new()), "test", "test", DefaultTaskRunner),
            }
        }

        fn initialise_media_state(&mut self) -> Result<()> {
            let state = self.alloc_raw(core::mem::size_of::<MediaState>() as u32)?;
            write_generic(self, state, MediaState::zeroed())?;
            write_generic(self, MEDIA_STATE_ROOT, state)
        }
    }

    #[async_trait::async_trait]
    impl WIPICContext for TestContext {
        fn alloc_raw(&mut self, size: u32) -> Result<u32> {
            let address = self.next;
            self.next = self.next.checked_add(size.max(1)).ok_or(WieError::AllocationFailure)?;
            Ok(address)
        }

        fn alloc(&mut self, size: u32) -> Result<wipi_types::wipic::WIPICIndirectPtr> {
            Ok(wipi_types::wipic::WIPICIndirectPtr(self.alloc_raw(size)?))
        }

        fn free(&mut self, _memory: wipi_types::wipic::WIPICIndirectPtr) -> Result<()> {
            Ok(())
        }

        fn free_raw(&mut self, _address: u32, _size: u32) -> Result<()> {
            Ok(())
        }

        fn data_ptr(&self, memory: wipi_types::wipic::WIPICIndirectPtr) -> Result<u32> {
            Ok(memory.0)
        }

        async fn call_function(&mut self, _address: u32, _args: &[u32]) -> Result<u32> {
            Ok(0)
        }

        fn system(&mut self) -> &mut System {
            &mut self.system
        }

        fn spawn(&mut self, _callback: WIPICMethodBody) -> Result<()> {
            Ok(())
        }

        async fn get_resource_size(&self, _name: &str) -> Result<Option<usize>> {
            Ok(None)
        }

        async fn read_resource(&self, _name: &str) -> Result<Vec<u8>> {
            Err(WieError::FatalError("test resource is unavailable".into()))
        }

        fn set_timer(&mut self, _due: Instant, _callback: WIPICMethodBody) {}
    }

    impl ByteRead for TestContext {
        fn read_bytes(&self, address: u32, result: &mut [u8]) -> Result<usize> {
            for (index, byte) in result.iter_mut().enumerate() {
                *byte = *self
                    .memory
                    .get(&address.checked_add(index as u32).ok_or(WieError::AllocationFailure)?)
                    .ok_or(WieError::InvalidMemoryAccess(address))?;
            }
            Ok(result.len())
        }
    }

    impl ByteWrite for TestContext {
        fn write_bytes(&mut self, address: u32, data: &[u8]) -> Result<()> {
            for (index, byte) in data.iter().enumerate() {
                let target = address.checked_add(index as u32).ok_or(WieError::AllocationFailure)?;
                self.memory.insert(target, *byte);
            }
            Ok(())
        }
    }

    #[futures_test::test]
    async fn clip_record_and_circular_data_are_guest_backed() -> Result<()> {
        let mut context = TestContext::new();
        context.initialise_media_state()?;
        let type_ptr = context.alloc_raw(12)?;
        context.write_bytes(type_ptr, b"Yamaha_MA3\0")?;
        let clip_ptr = clip_create(&mut context, type_ptr, 4, 0x1234).await?;
        let clip: LgtClip = read_generic(&context, clip_ptr)?;
        assert_eq!(core::mem::size_of::<LgtClip>(), 88);
        assert_eq!(clip.buffer_capacity, 4);
        assert_eq!(clip.available_bytes, 0);
        assert_eq!(clip.callback, 0x1234);

        let source = context.alloc_raw(6)?;
        context.write_bytes(source, b"abcdef")?;
        assert_eq!(clip_put_data(&mut context, clip_ptr, source, 6).await?, 4);
        let clip: LgtClip = read_generic(&context, clip_ptr)?;
        assert_eq!(clip.available_bytes, 4);
        let mut data = [0; 4];
        context.read_bytes(clip.primary_buffer, &mut data)?;
        assert_eq!(&data, b"abcd");

        assert_eq!(clip_clear_data(&mut context, clip_ptr).await?, 0);
        let clip: LgtClip = read_generic(&context, clip_ptr)?;
        assert_eq!((clip.read_offset, clip.available_bytes), (0, 0));
        assert_eq!(clip_free(&mut context, clip_ptr).await?, 0);
        assert_eq!(clip_put_data(&mut context, clip_ptr, source, 1).await?, -9);
        Ok(())
    }
}
