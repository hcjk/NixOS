OUTPUT_FORMAT(elf64-x86-64)
OUTPUT_ARCH(i386:x86-64)
ENTRY(_start)

PHDRS
{
    requests PT_LOAD FLAGS(4);
    text     PT_LOAD FLAGS(5);
    rodata   PT_LOAD FLAGS(4);
    data     PT_LOAD FLAGS(6);
}

SECTIONS
{
    . = 0xffffffff80000000;

    .requests : ALIGN(CONSTANT(MAXPAGESIZE))
    {
        KEEP(*(.requests_start_marker))
        KEEP(*(.requests))
        KEEP(*(.requests_end_marker))
    } :requests

    .text : ALIGN(CONSTANT(MAXPAGESIZE))
    {
        *(.text .text.*)
    } :text

    .rodata : ALIGN(CONSTANT(MAXPAGESIZE))
    {
        *(.rodata .rodata.*)
    } :rodata

    .data : ALIGN(CONSTANT(MAXPAGESIZE))
    {
        *(.data .data.*)
    } :data

    .bss : ALIGN(CONSTANT(MAXPAGESIZE))
    {
        *(COMMON)
        *(.bss .bss.*)
    } :data

    /DISCARD/ :
    {
        *(.eh_frame*)
        *(.note .note.*)
    }
}

