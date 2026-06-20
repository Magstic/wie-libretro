use alloc::{boxed::Box, format};
use core::{array, mem::size_of};

use arm32_cpu::{Cpu, Memory, Mode, reg};

use wie_util::{Result, WieError};

use crate::engine::{ArmEngine, ArmRegister, EngineRunResult, MemoryPermission};

pub struct Arm32CpuEngine {
    cpu: Cpu,
    mem: EmulatedMemory,
}

impl Arm32CpuEngine {
    pub fn new() -> Self {
        Self {
            cpu: Cpu::new(),
            mem: EmulatedMemory::new(),
        }
    }

    fn is_svc_exception(&self) -> bool {
        self.cpu.reg_get(Mode::User, reg::PC) == 0x08 && (self.cpu.reg_get(Mode::User, reg::CPSR) & 0x1f) == 0x13
    }

    fn read_svc_result(&mut self) -> Result<EngineRunResult> {
        let lr = self.cpu.reg_get(Mode::Supervisor, reg::LR);
        let spsr = self.cpu.reg_get(Mode::Supervisor, reg::SPSR);

        let svc_address = lr.checked_sub(2).ok_or(WieError::InvalidMemoryAccess(lr))?;
        let mut svc_bytes = [0u8; 2];
        self.mem.read_range(svc_address, 2, &mut svc_bytes)?;
        let instruction = u16::from_le_bytes(svc_bytes);
        if instruction & 0xff00 != 0xdf00 {
            return Err(WieError::FatalError(format!("Invalid Thumb SVC instruction {instruction:#06x}")));
        }

        let category = instruction as u32 & 0xff;

        Ok(EngineRunResult::Svc { category, lr, spsr })
    }
}

impl ArmEngine for Arm32CpuEngine {
    fn run(&mut self, end: u32, mut count: u32) -> Result<EngineRunResult> {
        loop {
            let pc = self.cpu.reg_get(Mode::User, reg::PC);

            if self.is_svc_exception() {
                return self.read_svc_result();
            }

            if pc < 0x1000 {
                return Err(WieError::InvalidMemoryAccess(pc));
            }

            if pc == end {
                return Ok(EngineRunResult::End);
            }

            if count == 0 {
                return Ok(EngineRunResult::CountExhausted);
            }

            let mut arm32cpu_memory = self.mem.as_arm32cpu_memory();

            if !(self.cpu.step(&mut arm32cpu_memory)) {
                return Err(WieError::FatalError("Undefined instruction".into()));
            }
            count -= 1;

            if let Some(x) = arm32cpu_memory.memory_error() {
                return Err(WieError::InvalidMemoryAccess(x));
            }
        }
    }

    fn reg_write(&mut self, reg: ArmRegister, value: u32) {
        if reg == ArmRegister::PC && value % 2 == 1 {
            self.cpu.reg_set(Mode::User, reg.into_armv4t(), value - 1);

            let cpsr = self.cpu.reg_get(Mode::User, reg::CPSR);
            self.cpu.reg_set(Mode::User, reg::CPSR, cpsr | (1 << 5)); // T bit

            return;
        }
        self.cpu.reg_set(Mode::User, reg.into_armv4t(), value);
    }

    fn reg_read(&self, reg: ArmRegister) -> u32 {
        self.cpu.reg_get(Mode::User, reg.into_armv4t())
    }

    fn mem_map(&mut self, address: u32, size: usize, _permission: MemoryPermission) {
        self.mem.map(address, size);
    }

    fn mem_write(&mut self, address: u32, data: &[u8]) -> Result<()> {
        self.mem.write_range(address, data)
    }

    fn mem_read(&mut self, address: u32, size: usize, result: &mut [u8]) -> Result<usize> {
        self.mem.read_range(address, size, result)
    }

    fn is_mapped(&self, address: u32, size: usize) -> bool {
        self.mem.is_mapped(address, size)
    }
}

impl ArmRegister {
    fn into_armv4t(self) -> u8 {
        match self {
            ArmRegister::R0 => 0,
            ArmRegister::R1 => 1,
            ArmRegister::R2 => 2,
            ArmRegister::R3 => 3,
            ArmRegister::R4 => 4,
            ArmRegister::R5 => 5,
            ArmRegister::R6 => 6,
            ArmRegister::R7 => 7,
            ArmRegister::R8 => 8,
            ArmRegister::SB => 9,
            ArmRegister::SL => 10,
            ArmRegister::FP => 11,
            ArmRegister::IP => 12,
            ArmRegister::SP => reg::SP,
            ArmRegister::LR => reg::LR,
            ArmRegister::PC => reg::PC,
            ArmRegister::Cpsr => reg::CPSR,
        }
    }
}

const TOTAL_MEMORY: u64 = 0x100000000;
const PAGE_SIZE: usize = 0x10000;
const PAGE_MASK: u32 = (PAGE_SIZE - 1) as _;
const PAGE_COUNT: usize = (TOTAL_MEMORY / PAGE_SIZE as u64) as usize;

struct EmulatedMemory {
    mapped: [bool; PAGE_COUNT],
    pages: [Option<Box<[u8; PAGE_SIZE]>>; PAGE_COUNT],
}

impl EmulatedMemory {
    fn new() -> Self {
        Self {
            mapped: [false; PAGE_COUNT],
            pages: array::from_fn(|_| None),
        }
    }

    fn as_arm32cpu_memory(&mut self) -> Arm32CpuMemory<'_> {
        Arm32CpuMemory::new(self)
    }

    fn map(&mut self, address: u32, size: usize) {
        let Some(page_range) = Self::page_range(address, size) else {
            debug_assert!(false, "invalid memory mapping: address={address:#x}, size={size:#x}");
            return;
        };

        for page_index in page_range {
            self.mapped[page_index] = true;
        }
    }

    fn page_range(address: u32, size: usize) -> Option<core::ops::Range<usize>> {
        let start = address as u64;
        if size == 0 {
            let page = start as usize / PAGE_SIZE;
            return Some(page..page);
        }

        let end = start.checked_add(size as u64)?;
        if end > TOTAL_MEMORY {
            return None;
        }

        let first_page = start as usize / PAGE_SIZE;
        let page_end = end.div_ceil(PAGE_SIZE as u64) as usize;
        Some(first_page..page_end)
    }

    fn read_range(&self, address: u32, size: usize, result: &mut [u8]) -> Result<usize> {
        let mut remaining_size = size;
        let mut current_address = address;

        while remaining_size > 0 {
            let page_address = current_address & !PAGE_MASK;
            let page_index = page_address as usize / PAGE_SIZE;
            if !self.mapped[page_index] {
                return Err(WieError::InvalidMemoryAccess(current_address));
            }

            let offset = (current_address - page_address) as usize;
            let available_bytes = (PAGE_SIZE - offset).min(remaining_size);
            let destination = &mut result[size - remaining_size..size - remaining_size + available_bytes];

            if let Some(page_data) = self.pages[page_index].as_ref() {
                destination.copy_from_slice(&page_data[offset..offset + available_bytes]);
            } else {
                destination.fill(0);
            }
            remaining_size -= available_bytes;
            current_address = current_address.wrapping_add(available_bytes as u32);
        }

        Ok(size)
    }

    fn write_range(&mut self, address: u32, data: &[u8]) -> Result<()> {
        let mut current_address = address;
        let mut data_index = 0;

        while data_index < data.len() {
            let page_address = current_address & !PAGE_MASK;
            let page_index = page_address as usize / PAGE_SIZE;
            if !self.mapped[page_index] {
                return Err(WieError::InvalidMemoryAccess(current_address));
            }

            let page_data = self.pages[page_index].get_or_insert_with(|| Box::new([0; PAGE_SIZE]));
            let offset = (current_address - page_address) as usize;
            let available_bytes = (PAGE_SIZE - offset).min(data.len() - data_index);

            page_data[offset..offset + available_bytes].copy_from_slice(&data[data_index..data_index + available_bytes]);
            data_index += available_bytes;
            current_address = current_address.wrapping_add(available_bytes as u32);
        }

        Ok(())
    }

    fn is_mapped(&self, address: u32, size: usize) -> bool {
        Self::page_range(address, size).is_some_and(|page_range| page_range.into_iter().all(|page_index| self.mapped[page_index]))
    }
}

struct Arm32CpuMemory<'a> {
    emulated_memory: &'a mut EmulatedMemory,
    memory_error: Option<u32>,
}

impl<'a> Arm32CpuMemory<'a> {
    fn new(emulated_memory: &'a mut EmulatedMemory) -> Self {
        Self {
            emulated_memory,
            memory_error: None,
        }
    }

    fn memory_error(&self) -> Option<u32> {
        self.memory_error
    }

    fn read_page(&mut self, addr: u32) -> Option<Option<&[u8; PAGE_SIZE]>> {
        let page_index = addr as usize / PAGE_SIZE;
        if !self.emulated_memory.mapped[page_index] {
            self.memory_error = Some(addr);
            return None;
        }

        Some(self.emulated_memory.pages[page_index].as_deref())
    }

    fn write_page(&mut self, addr: u32) -> Option<&mut [u8; PAGE_SIZE]> {
        let page_index = addr as usize / PAGE_SIZE;
        if !self.emulated_memory.mapped[page_index] {
            self.memory_error = Some(addr);
            return None;
        }

        Some(self.emulated_memory.pages[page_index].get_or_insert_with(|| Box::new([0; PAGE_SIZE])))
    }
}

impl Memory for Arm32CpuMemory<'_> {
    fn r8(&mut self, addr: u32) -> u8 {
        let offset = (addr & PAGE_MASK) as usize;
        self.read_page(addr).flatten().map_or(0, |page| page[offset])
    }

    fn r16(&mut self, addr: u32) -> u16 {
        let offset = (addr & PAGE_MASK) as usize;
        if offset + size_of::<u16>() <= PAGE_SIZE {
            return self
                .read_page(addr)
                .flatten()
                .map_or(0, |page| u16::from_le_bytes([page[offset], page[offset + 1]]));
        }

        u16::from_le_bytes([self.r8(addr), self.r8(addr.wrapping_add(1))])
    }

    fn r32(&mut self, addr: u32) -> u32 {
        let offset = (addr & PAGE_MASK) as usize;
        if offset + size_of::<u32>() <= PAGE_SIZE {
            return self.read_page(addr).flatten().map_or(0, |page| {
                u32::from_le_bytes([page[offset], page[offset + 1], page[offset + 2], page[offset + 3]])
            });
        }

        u32::from_le_bytes([
            self.r8(addr),
            self.r8(addr.wrapping_add(1)),
            self.r8(addr.wrapping_add(2)),
            self.r8(addr.wrapping_add(3)),
        ])
    }

    fn w8(&mut self, addr: u32, val: u8) {
        let offset = (addr & PAGE_MASK) as usize;
        if let Some(page) = self.write_page(addr) {
            page[offset] = val;
        }
    }

    fn w16(&mut self, addr: u32, val: u16) {
        let offset = (addr & PAGE_MASK) as usize;
        if offset + size_of::<u16>() > PAGE_SIZE {
            let bytes = val.to_le_bytes();
            self.w8(addr, bytes[0]);
            self.w8(addr.wrapping_add(1), bytes[1]);
            return;
        }

        if let Some(page) = self.write_page(addr) {
            page[offset..offset + size_of::<u16>()].copy_from_slice(&val.to_le_bytes());
        }
    }

    fn w32(&mut self, addr: u32, val: u32) {
        let offset = (addr & PAGE_MASK) as usize;
        if offset + size_of::<u32>() > PAGE_SIZE {
            for (index, byte) in val.to_le_bytes().into_iter().enumerate() {
                self.w8(addr.wrapping_add(index as u32), byte);
            }
            return;
        }

        if let Some(page) = self.write_page(addr) {
            page[offset..offset + size_of::<u32>()].copy_from_slice(&val.to_le_bytes());
        }
    }
}

#[cfg(test)]
mod tests {
    use arm32_cpu::Memory;

    use super::EmulatedMemory;

    #[test]
    fn test_memory_basic() {
        let mut memory = EmulatedMemory::new();

        memory.map(0x10000, 0x1000);
        memory.map(0x11000, 0x1000);
        memory.map(0x20000, 0x10000);

        let mut zeroes = [0xff; 16];
        memory.read_range(0x20000, zeroes.len(), &mut zeroes).unwrap();
        assert_eq!(zeroes, [0; 16]);
        assert!(memory.pages[0x20000 / super::PAGE_SIZE].is_none());

        memory.write_range(0x10000, &[123; 0x1000]).unwrap();

        let mut buf = [0; 0x1000];
        memory.read_range(0x10000, 0x1000, &mut buf).unwrap();
        assert_eq!(buf, [123; 0x1000]);

        memory.write_range(0x10900, &[100; 0x1000]).unwrap();

        memory.read_range(0x10900, 0x1000, &mut buf).unwrap();
        assert_eq!(buf, [100; 0x1000]);

        let mut arm32cpu_memory = memory.as_arm32cpu_memory();

        let r8 = arm32cpu_memory.r8(0x10000);
        assert_eq!(r8, 123);

        let r16 = arm32cpu_memory.r16(0x10000);
        assert_eq!(r16, 123 | (123 << 8));

        let r32 = arm32cpu_memory.r32(0x10000);
        assert_eq!(r32, 123 | (123 << 8) | (123 << 16) | (123 << 24));

        arm32cpu_memory.w8(0x10000, 12);
        let r8 = arm32cpu_memory.r8(0x10000);
        assert_eq!(r8, 12);

        arm32cpu_memory.w16(0x10000, 0x1234);
        let r16 = arm32cpu_memory.r16(0x10000);
        assert_eq!(r16, 0x1234);

        arm32cpu_memory.w32(0x10000, 0x12345678);
        let r32 = arm32cpu_memory.r32(0x10000);
        assert_eq!(r32, 0x12345678);
    }

    #[test]
    fn test_instruction_memory_access_across_page_boundary() {
        let mut memory = EmulatedMemory::new();
        memory.map(0x10000, 0x20000);

        let mut arm32cpu_memory = memory.as_arm32cpu_memory();
        arm32cpu_memory.w32(0x1fffe, 0x12345678);

        assert_eq!(arm32cpu_memory.r16(0x1ffff), 0x3456);
        assert_eq!(arm32cpu_memory.r32(0x1fffe), 0x12345678);
        assert_eq!(arm32cpu_memory.memory_error(), None);
    }

    #[test]
    fn test_large_mapping_allocates_pages_on_first_write() {
        let mut memory = EmulatedMemory::new();
        memory.map(0x40000000, 0x10000000);

        assert_eq!(memory.pages.iter().filter(|page| page.is_some()).count(), 0);
        memory.write_range(0x48000000, &[1, 2, 3, 4]).unwrap();
        assert_eq!(memory.pages.iter().filter(|page| page.is_some()).count(), 1);
    }

    #[test]
    fn test_memory_unmapped_read() {
        let mut memory = EmulatedMemory::new();

        memory.map(0x10000, 0x10000);

        let mut buf = [0; 0x1000];
        assert!(memory.read_range(0x1f500, 0x1000, &mut buf).is_err());
    }

    #[test]
    fn test_memory_unmapped_write() {
        let mut memory = EmulatedMemory::new();

        memory.map(0x10000, 0x10000);

        assert!(memory.write_range(0x1f500, &[12; 0x1000]).is_err());
    }
}
