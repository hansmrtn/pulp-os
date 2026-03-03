// status bar constants and stack-measurement utilities
// system stats are emitted via log::info! in the scheduler

pub const BAR_HEIGHT: u16 = 4;
pub const CONTENT_TOP: u16 = BAR_HEIGHT;

const STACK_PAINT_WORD: u32 = 0xDEAD_BEEF;

// paint the unused stack with a sentinel word so stack_high_water_mark
// can later measure peak usage; call very early in boot
pub fn paint_stack() {
    #[cfg(target_arch = "riscv32")]
    {
        let sp: usize;
        unsafe {
            core::arch::asm!("mv {}, sp", out(reg) sp);
        }

        unsafe extern "C" {
            static _stack_end_cpu0: u8;
        }
        let bottom = (&raw const _stack_end_cpu0) as usize;

        let guard_skip = 256;
        let paint_bottom = bottom + guard_skip;

        let paint_top = sp.saturating_sub(256);

        if paint_top <= paint_bottom {
            return;
        }

        let start = (paint_bottom + 3) & !3;

        let mut addr = start;
        while addr + 4 <= paint_top {
            unsafe {
                core::ptr::write_volatile(addr as *mut u32, STACK_PAINT_WORD);
            }
            addr += 4;
        }
    }
}

pub fn free_stack_bytes() -> usize {
    #[cfg(target_arch = "riscv32")]
    {
        let sp: usize;
        unsafe {
            core::arch::asm!("mv {}, sp", out(reg) sp);
        }

        unsafe extern "C" {
            static _stack_end_cpu0: u8;
        }
        let stack_bottom = (&raw const _stack_end_cpu0) as usize;
        sp.saturating_sub(stack_bottom)
    }

    #[cfg(not(target_arch = "riscv32"))]
    {
        0
    }
}

pub fn stack_high_water_mark() -> usize {
    #[cfg(target_arch = "riscv32")]
    {
        unsafe extern "C" {
            static _stack_end_cpu0: u8;
            static _stack_start_cpu0: u8;
        }
        let bottom = (&raw const _stack_end_cpu0) as usize;
        let top = (&raw const _stack_start_cpu0) as usize;

        let guard_skip = 256;
        let scan_bottom = bottom + guard_skip;

        let start = (scan_bottom + 3) & !3;

        let mut addr = start;
        while addr + 4 <= top {
            let val = unsafe { core::ptr::read_volatile(addr as *const u32) };
            if val != STACK_PAINT_WORD {
                break;
            }
            addr += 4;
        }

        top.saturating_sub(addr)
    }

    #[cfg(not(target_arch = "riscv32"))]
    {
        0
    }
}
