use core::arch::x86_64::__cpuid;

pub struct CpuInfo {
    pub has_apic: bool,
    pub has_nx: bool,
    pub has_sse2: bool,
}

impl CpuInfo {
    #[must_use]
    pub fn detect() -> Self {
        let basic = __cpuid(1);
        let maximum_extended = __cpuid(0x8000_0000).eax;
        let extended = if maximum_extended >= 0x8000_0001 {
            __cpuid(0x8000_0001)
        } else {
            __cpuid(0)
        };
        Self {
            has_apic: basic.edx & (1 << 9) != 0,
            has_sse2: basic.edx & (1 << 26) != 0,
            has_nx: maximum_extended >= 0x8000_0001 && extended.edx & (1 << 20) != 0,
        }
    }
}
