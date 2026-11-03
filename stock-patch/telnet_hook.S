/*
 * Telnet hook for the stock B&W Zeppelin Air MCU firmware (ZD1_v2-04-05, AT32UC3A1256, AVR32 UC3A).
 *
 * Puts the network module's shell on telnet and re-asserts it PERIODICALLY (~30 s): if the module reboots on its
 * own (its watchdog, after a crash) while the MCU keeps running, it reverts its shell to UART1, and a one-shot
 * enable would lose telnet until a full power cycle; this restores it within ~30 s, with no MCU reboot needed.
 * The module's comm-settings commit is change-detecting, so re-writing reg 0x76=5 while it is already telnet is a
 * no-op (it does NOT restart the telnet server / drop an active session). After a module reboot the stored port
 * reverted too, so we re-write BOTH 0x75 and 0x76.
 *
 * Timing uses the CPU COUNT cycle register (mfsr rX,0x108; 66 MHz in RUN mode) so the
 * period is independent of the main-loop tick rate and survives a module reboot.
 *
 * The hook: the voTask call pointer (flash 0x8001e870) is repointed to this trampoline (patch.py); it runs our
 * logic, then tail-calls the real voTask.
 *
 * Persistent RAM (carved, zeroed by crt0 to 0x8258 by patch.py):
 *   STATE byte  @0x8250  : 0..3 = write sequence, 4 = enabled/counting
 *   LASTCOUNT   @0x8254  : COUNT snapshot taken when the sequence last completed
 */

	.text
	.align	2
	.global	telnet_hook
	.type	telnet_hook,@function

	.set	STATUS,    0x00003ee0	/* DM1000Status word, == 0x2100 when ready */
	.set	ATTACH,    0x00004df4	/* attach-state byte, == 1 in RUN          */
	.set	STATE,     0x00008250	/* our state byte (carved, zeroed)         */
	.set	LASTCOUNT, 0x00008254	/* COUNT snapshot word (carved, zeroed)    */
	.set	WRITEREG,  0x8000b910	/* dmhBcoMgr_ercWriteRegister              */
	.set	VOTASK,    0x8000e19c	/* dmhBcoMgr_voTask (original call target) */
	.set	READY,     0x00002100
	.set	QUEUED,    0x00420020	/* ercWriteRegister: newly queued          */
	.set	PERIOD,    0x76000000	/* ~1.98e9 cycles ~= 30 s at 66 MHz        */

telnet_hook:
	/* --- gate: only act when the module is ready AND attached --- */
	lddpc	r8, .Lstatus
	ld.w	r9, r8[0]
	lddpc	r10, .Lready
	cp.w	r9, r10
	brne	.Ltail
	lddpc	r8, .Lattach
	ld.ub	r9, r8[0]
	cp.w	r9, 1
	brne	.Ltail

	lddpc	r8, .Lstate
	ld.ub	r9, r8[0]
	cp.w	r9, 4
	brne	.Lactive		/* states 0-3 drive the write sequence (need a helper call) */

	/* --- state 4: enabled; re-arm every PERIOD cycles (no helper, no LR save) --- */
	mfsr	r9, 0x108		/* r9 = COUNT */
	lddpc	r8, .Llastcount
	ld.w	r10, r8[0]
	sub	r9, r10			/* r9 = COUNT - LASTCOUNT (unsigned delta) */
	lddpc	r10, .Lperiod
	cp.w	r9, r10
	brlo	.Ltail			/* not enough time elapsed yet */
	lddpc	r8, .Lstate		/* re-arm: STATE = 0 -> sequence re-runs next ticks */
	mov	r9, 0
	st.b	r8[0], r9
	rjmp	.Ltail

.Lactive:
	/* --- states 0-3 call ercWriteRegister: save LR, keep SP 8-byte aligned --- */
	pushm	lr
	sub	sp, 4
	lddpc	r8, .Lstate
	ld.ub	r9, r8[0]

	cp.w	r9, 0
	brne	.Lc1
	/* state 0: queue write reg 0x75 = "23" */
	lddpc	r8, .Lwritereg
	mov	r12, 0x75
	lddpc	r11, .Lport
	mov	r10, 2
	icall	r8
	lddpc	r9, .Lqueued
	cp.w	r12, r9
	brne	.Lrestore
	mov	r9, 1
	rjmp	.Lsave

.Lc1:
	cp.w	r9, 1
	brne	.Lc2
	/* state 1: confirm 0x75 (re-issue identical; ret 0 => done) */
	lddpc	r8, .Lwritereg
	mov	r12, 0x75
	lddpc	r11, .Lport
	mov	r10, 2
	icall	r8
	cp.w	r12, 0
	brne	.Lrestore
	mov	r9, 2
	rjmp	.Lsave

.Lc2:
	cp.w	r9, 2
	brne	.Lc3
	/* state 2: queue write reg 0x76 = mode 5 */
	lddpc	r8, .Lwritereg
	mov	r12, 0x76
	lddpc	r11, .Lmode
	mov	r10, 2
	icall	r8
	lddpc	r9, .Lqueued
	cp.w	r12, r9
	brne	.Lrestore
	mov	r9, 3
	rjmp	.Lsave

.Lc3:
	/* state 3: confirm 0x76; on completion snapshot COUNT and go to state 4 */
	lddpc	r8, .Lwritereg
	mov	r12, 0x76
	lddpc	r11, .Lmode
	mov	r10, 2
	icall	r8
	cp.w	r12, 0
	brne	.Lrestore
	mfsr	r9, 0x108		/* snapshot COUNT */
	lddpc	r8, .Llastcount
	st.w	r8[0], r9
	mov	r9, 4			/* STATE = 4 (enabled/counting) */

.Lsave:
	lddpc	r8, .Lstate
	st.b	r8[0], r9

.Lrestore:
	sub	sp, -4
	ldm	sp++, lr

.Ltail:
	lddpc	pc, .Lvotask

	.align	2
.Lstatus:	.long	STATUS
.Lattach:	.long	ATTACH
.Lstate:	.long	STATE
.Llastcount:	.long	LASTCOUNT
.Lwritereg:	.long	WRITEREG
.Lvotask:	.long	VOTASK
.Lready:	.long	READY
.Lqueued:	.long	QUEUED
.Lperiod:	.long	PERIOD
.Lport:		.long	.Lportdata
.Lmode:		.long	.Lmodedata

	.align	2
.Lportdata:	.byte	0x32, 0x33	/* "23" */
	.align	2
.Lmodedata:	.byte	0x05, 0x00	/* UARTPortsSelect mode 5 */

	.size	telnet_hook, .-telnet_hook
