; hello.asm - prints "Hello, World!" to stdout
; Target: Windows x64 (Win32 console API), assembler: NASM
;
; Build:  nasm -f win64 hello.asm -o hello.obj
;         link hello.obj /subsystem:console /entry:main /out:hello.exe
;         (or with lld: lld-link /subsystem:console /entry:main hello.obj)

        bits 64
        default rel

        extern GetStdHandle
        extern WriteFile
        extern ExitProcess

        section .data
msg:    db "Hello, World!", 13, 10
msg_len equ $ - msg

        section .bss
written: resq 1

        section .text
        global main

main:
        sub     rsp, 40                 ; 32 bytes shadow space + alignment

        ; hStdout = GetStdHandle(STD_OUTPUT_HANDLE)   ; -11
        mov     ecx, -11
        call    GetStdHandle
        mov     rbx, rax                ; save handle

        ; WriteFile(hStdout, msg, msg_len, &written, NULL)
        mov     rcx, rbx
        lea     rdx, [msg]
        mov     r8d, msg_len
        lea     r9, [written]
        mov     qword [rsp + 32], 0     ; 5th argument on the stack
        call    WriteFile

        ; ExitProcess(0)
        xor     ecx, ecx
        call    ExitProcess

        add     rsp, 40                 ; not reached
        ret
