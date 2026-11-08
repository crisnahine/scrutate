# asm sample
.text
.globl _start
_start:
    mov $1, %eax
    call helper
    ret
helper:
    ret
