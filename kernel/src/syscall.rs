use core::arch::{asm, global_asm};
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use nexos_abi::{ABI_VERSION, Error, Syscall, SystemInfoSelector};

use crate::gdt;
use crate::memory::FrameAllocator;
use crate::paging::PagingInfo;

const EFER_MSR: u32 = 0xc000_0080;
const STAR_MSR: u32 = 0xc000_0081;
const LSTAR_MSR: u32 = 0xc000_0082;
const FMASK_MSR: u32 = 0xc000_0084;
const EFER_SYSTEM_CALL_ENABLE: u64 = 1;
const SYSCALL_STACK_SIZE: usize = 16 * 1024;
const USER_CODE_ADDRESS: u64 = 0x0000_0000_4000_0000;
const USER_STACK_ADDRESS: u64 = 0x0000_0000_7fff_0000;
const PAGE_SIZE: usize = 4096;

#[repr(align(16))]
struct SyscallStack([u8; SYSCALL_STACK_SIZE]);

static mut SYSCALL_STACK: SyscallStack = SyscallStack([0; SYSCALL_STACK_SIZE]);
static INITIALIZED: AtomicBool = AtomicBool::new(false);
static USER_TEST_MAPPED: AtomicBool = AtomicBool::new(false);
static USER_CODE_PHYSICAL: AtomicU64 = AtomicU64::new(0);
static SYSCALL_COUNT: AtomicU64 = AtomicU64::new(0);
static LAST_SYSCALL: AtomicU64 = AtomicU64::new(u64::MAX);
static LAST_EXIT_STATUS: AtomicU64 = AtomicU64::new(0);

#[unsafe(no_mangle)]
static mut NEXOS_SYSCALL_STACK_TOP: u64 = 0;
#[unsafe(no_mangle)]
static mut NEXOS_SYSCALL_USER_RSP: u64 = 0;
#[unsafe(no_mangle)]
static mut NEXOS_USER_RETURN_RSP: u64 = 0;
#[unsafe(no_mangle)]
static mut NEXOS_USER_RETURN_RIP: u64 = 0;
#[unsafe(no_mangle)]
static mut NEXOS_KERNEL_RETURN_FLAGS: u64 = 0;
#[unsafe(no_mangle)]
static mut NEXOS_SYSCALL_RETURN_TO_KERNEL: u8 = 0;

#[repr(C)]
struct SyscallFrame {
    r15: u64,
    r14: u64,
    r13: u64,
    r12: u64,
    rbx: u64,
    rbp: u64,
    number: u64,
    argument_0: u64,
    argument_1: u64,
    argument_2: u64,
    argument_3: u64,
    argument_4: u64,
    argument_5: u64,
    user_rip: u64,
    user_flags: u64,
}

global_asm!(
    r#"
    .section .text
    .global nexos_syscall_entry
    .type nexos_syscall_entry, @function
nexos_syscall_entry:
    mov qword ptr [rip + NEXOS_SYSCALL_USER_RSP], rsp
    mov rsp, qword ptr [rip + NEXOS_SYSCALL_STACK_TOP]

    push r11
    push rcx
    push r9
    push r8
    push r10
    push rdx
    push rsi
    push rdi
    push rax
    push rbp
    push rbx
    push r12
    push r13
    push r14
    push r15

    sub rsp, 8
    lea rdi, [rsp + 8]
    call nexos_syscall_dispatch
    add rsp, 8

    pop r15
    pop r14
    pop r13
    pop r12
    pop rbx
    pop rbp
    add rsp, 8
    pop rdi
    pop rsi
    pop rdx
    pop r10
    pop r8
    pop r9
    pop rcx
    pop r11

    cmp byte ptr [rip + NEXOS_SYSCALL_RETURN_TO_KERNEL], 0
    jne .Lreturn_to_kernel
    mov rsp, qword ptr [rip + NEXOS_SYSCALL_USER_RSP]
    sysretq

.Lreturn_to_kernel:
    mov byte ptr [rip + NEXOS_SYSCALL_RETURN_TO_KERNEL], 0
    mov rsp, qword ptr [rip + NEXOS_USER_RETURN_RSP]
    push qword ptr [rip + NEXOS_KERNEL_RETURN_FLAGS]
    popfq
    jmp qword ptr [rip + NEXOS_USER_RETURN_RIP]
    .size nexos_syscall_entry, . - nexos_syscall_entry

    .global nexos_enter_user
    .type nexos_enter_user, @function
nexos_enter_user:
    mov qword ptr [rip + NEXOS_USER_RETURN_RSP], rsp
    lea rax, [rip + .Luser_return]
    mov qword ptr [rip + NEXOS_USER_RETURN_RIP], rax
    pushfq
    pop rax
    mov qword ptr [rip + NEXOS_KERNEL_RETURN_FLAGS], rax
    mov byte ptr [rip + NEXOS_SYSCALL_RETURN_TO_KERNEL], 0

    mov r8, rdi
    mov r9, rsi
    movzx eax, cx
    push rax
    push r9
    pushfq
    pop rax
    or rax, 0x200
    push rax
    movzx eax, dx
    push rax
    push r8
    iretq

.Luser_return:
    ret
    .size nexos_enter_user, . - nexos_enter_user
"#
);

unsafe extern "C" {
    fn nexos_syscall_entry();
    fn nexos_enter_user(entry: u64, stack: u64, code_selector: u16, data_selector: u16) -> i64;
}

pub fn init() -> bool {
    if INITIALIZED.load(Ordering::Acquire) {
        return true;
    }
    let selectors = gdt::selectors();
    if selectors.kernel_code == 0
        || selectors.kernel_data != selectors.kernel_code + 8
        || selectors.user_code == 0
        || selectors.user_data == 0
    {
        return false;
    }

    // SAFETY: Taking a raw address does not create a reference to the mutable
    // static; the stack is exclusively owned by syscall entry.
    let stack_top =
        unsafe { core::ptr::addr_of!(SYSCALL_STACK.0) as u64 } + SYSCALL_STACK_SIZE as u64;
    // SAFETY: Initialization runs once on the boot CPU before any userspace
    // transition. The stack has static storage for the kernel lifetime.
    unsafe {
        core::ptr::write_volatile(&raw mut NEXOS_SYSCALL_STACK_TOP, stack_top);
    }

    let user_star_base = u64::from((selectors.user_data & !3).saturating_sub(8));
    let star = (u64::from(selectors.kernel_code) << 32) | (user_star_base << 48);
    let lstar = nexos_syscall_entry as *const () as u64;
    let fmask = (1_u64 << 8) | (1_u64 << 9) | (1_u64 << 10);
    // SAFETY: These architectural MSRs configure SYSCALL/SYSRET on the boot
    // CPU. GDT selectors and the entry address remain valid permanently.
    unsafe {
        write_msr(STAR_MSR, star);
        write_msr(LSTAR_MSR, lstar);
        write_msr(FMASK_MSR, fmask);
        write_msr(EFER_MSR, read_msr(EFER_MSR) | EFER_SYSTEM_CALL_ENABLE);
    }
    INITIALIZED.store(true, Ordering::Release);
    true
}

#[must_use]
pub fn statistics() -> (u64, Option<Syscall>, i32) {
    let number = LAST_SYSCALL.load(Ordering::Relaxed);
    (
        SYSCALL_COUNT.load(Ordering::Relaxed),
        Syscall::try_from(number).ok(),
        LAST_EXIT_STATUS.load(Ordering::Relaxed) as i32,
    )
}

pub fn run_ring3_self_test(
    paging: &mut PagingInfo,
    allocator: &mut FrameAllocator,
) -> Result<i64, UserTestError> {
    if !init() {
        return Err(UserTestError::SyscallUnavailable);
    }
    if !USER_TEST_MAPPED.load(Ordering::Acquire) {
        let code_frame = allocator
            .allocate()
            .ok_or(UserTestError::OutOfPhysicalMemory)?;
        let stack_frame = allocator
            .allocate()
            .ok_or(UserTestError::OutOfPhysicalMemory)?;
        paging
            .map_user_page(USER_CODE_ADDRESS, code_frame, false, true, allocator)
            .map_err(|_| UserTestError::MappingFailed)?;
        paging
            .map_user_page(USER_STACK_ADDRESS, stack_frame, true, false, allocator)
            .map_err(|_| UserTestError::MappingFailed)?;
        USER_CODE_PHYSICAL.store(code_frame, Ordering::Release);
        USER_TEST_MAPPED.store(true, Ordering::Release);
    }

    let code_frame = USER_CODE_PHYSICAL.load(Ordering::Acquire);
    let code_address = paging
        .hhdm_offset()
        .checked_add(code_frame)
        .ok_or(UserTestError::AddressOverflow)?;
    // mov eax, SystemInfo; xor edi,edi; syscall; mov rdi,rax;
    // mov eax, Exit; syscall; ud2
    const PROGRAM: [u8; 21] = [
        0xb8, 0x41, 0x00, 0x00, 0x00, 0x31, 0xff, 0x0f, 0x05, 0x48, 0x89, 0xc7, 0xb8, 0x00, 0x00,
        0x00, 0x00, 0x0f, 0x05, 0x0f, 0x0b,
    ];
    // SAFETY: The user-test code frame is exclusively owned by the kernel and
    // remains mapped through the HHDM. The program fits in one page.
    unsafe {
        core::ptr::write_bytes(code_address as *mut u8, 0, PAGE_SIZE);
        core::ptr::copy_nonoverlapping(PROGRAM.as_ptr(), code_address as *mut u8, PROGRAM.len());
    }

    let selectors = gdt::selectors();
    let user_stack = USER_STACK_ADDRESS + PAGE_SIZE as u64 - 16;
    // SAFETY: Code and stack pages are present with user permissions; the GDT
    // selectors have DPL3, and Exit returns through the saved kernel frame.
    let status = unsafe {
        nexos_enter_user(
            USER_CODE_ADDRESS,
            user_stack,
            selectors.user_code,
            selectors.user_data,
        )
    };
    Ok(status)
}

#[derive(Clone, Copy, Debug)]
pub enum UserTestError {
    SyscallUnavailable,
    OutOfPhysicalMemory,
    MappingFailed,
    AddressOverflow,
}

#[unsafe(no_mangle)]
extern "C" fn nexos_syscall_dispatch(frame: &SyscallFrame) -> i64 {
    SYSCALL_COUNT.fetch_add(1, Ordering::Relaxed);
    LAST_SYSCALL.store(frame.number, Ordering::Relaxed);

    let syscall = match Syscall::try_from(frame.number) {
        Ok(syscall) => syscall,
        Err(()) => return Error::NotSupported.as_syscall_result(),
    };
    match syscall {
        Syscall::Exit => {
            LAST_EXIT_STATUS.store(frame.argument_0, Ordering::Relaxed);
            // SAFETY: The syscall assembly checks and clears this single-CPU
            // handoff flag before restoring the saved kernel continuation.
            unsafe {
                core::ptr::write_volatile(&raw mut NEXOS_SYSCALL_RETURN_TO_KERNEL, 1);
            }
            frame.argument_0 as i64
        }
        Syscall::SystemInfo => match frame.argument_0 {
            value if value == SystemInfoSelector::AbiVersion as u64 => i64::from(ABI_VERSION),
            value if value == SystemInfoSelector::PageSize as u64 => PAGE_SIZE as i64,
            _ => Error::InvalidArgument.as_syscall_result(),
        },
        Syscall::ClockGet => crate::interrupts::uptime_milliseconds() as i64,
        _ => Error::NotSupported.as_syscall_result(),
    }
}

unsafe fn read_msr(msr: u32) -> u64 {
    let low: u32;
    let high: u32;
    // SAFETY: The caller selects an architectural MSR available in long mode.
    unsafe {
        asm!(
            "rdmsr",
            in("ecx") msr,
            out("eax") low,
            out("edx") high,
            options(nomem, nostack)
        );
    }
    (u64::from(high) << 32) | u64::from(low)
}

unsafe fn write_msr(msr: u32, value: u64) {
    let bytes = value.to_le_bytes();
    let low = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
    let high = u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]);
    // SAFETY: The caller selects a writable architectural MSR and provides the
    // intended 64-bit value split across EDX:EAX.
    unsafe {
        asm!(
            "wrmsr",
            in("ecx") msr,
            in("eax") low,
            in("edx") high,
            options(nomem, nostack)
        );
    }
}
