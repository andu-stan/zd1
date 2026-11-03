//
// Sol: the DM870's audio-DSP datapath, driven by Warp1. Modelled from docs/module/warp.md (hardware,
// probed 2026-10-21) so that our OWN Sol programs produce hardware-faithful results in the emulator --
// a target to develop and test DSP/compute kernels against. This is a model of the chip, not of KnOS's
// ARM reference model (`sys sol testInstr`); it is not written to that self test (although, since the
// hardware matches that reference for the ops it checks, the self test's ALU failures do fall as a
// side effect -- a convenient sanity signal, not the goal).
//
// Sol is a dataflow VLIW (US 8,458,443 B2): load / exec / store units joined by data queues, driven by
// a sequencer. Two 64-bit-wide program memories:
//   mem1 (ARM 0xD0760000): [63:48] ALU slot, [47:32] MAC slot, [31:16] per-tap coeff/reg select,
//                          [15:0] sequencer.
//   mem0 (ARM 0xD0720000): three 16-bit address-unit slots ([15:0] unit0, [31:16] unit1, [47:32] unit2),
//                          [63:48] always 0.
// ALU/MAC slot operand field (10 bits): src1[2:0] / src2[5:3] / dst[9:6]; source 4 = unit0's loaded
// value, 5 = unit1's, 3 = the ALU's own last result, the rest other units / stale (0 here) except the
// MAC-accumulator move ops. Data memory = 10 banks x 1 KB at ARM 0xA8000000 (Warp1 local); a 64-bit Sol
// value is two 32-bit lanes in a bank pair -- lane 0 (low) in the even bank, lane 1 (high) in the odd.
// unit0 works on banks 6/7 (0xA8001800/0x1C00), unit1 on banks 8/9 (0xA8002000/0x2400).
//
// Run handshake (KnOS and our runner both do this): load both program memories, write operands into the
// data banks, write 0xD0740020 = 4 then 0xD0740018 = 0, poll 0xD0740018 bit 0 for done. On the start
// write we execute the whole program at once and set the done bit (Sol is modelled as instantaneous).
//
// Verified against KnOS's `sys sol testInstr` (its ARM reference model == hardware): the ALU + SIMD +
// logic + shift/rotate ops, the operand field, Modeset/Moderead, the address-unit load/store, and the
// MAC (field selection, dual accumulators, sign-extended operands) all match -- the self test reports
// 0 failures. The one remaining first-cut piece is the counted-loop sequencer: the 0xF3xx back-jump
// target is the low 6 bits; the loop counter (loaded by a 0x88NN commit, docs/module/warp.md) is not
// modelled yet, so loops are bounded by an iteration guard. Straight-line programs (next/end) run exactly.
//
namespace Antmicro.Renode.Peripherals.CR870
{
    internal class CR870_Sol
    {
        public CR870_Sol(CR870_Core core)
        {
            this.core = core;
            Reset();
        }

        public void Reset()
        {
            mode = 2; // Modeset mode register; power-on default 0x2 (docs/module/warp.md)
            macState = 0x400; // MAC mode state register (ctx+0x1c); 0x400 as the unit runs it
            macA = macB = 0; // the two MAC accumulators: persist across runs, cleared by InitMac
            macResA = macResB = 0;
            for (var u = 0; u < Units; u++)
            {
                loadPtr[u] = storePtr[u] = 0;
            }
            for (var q = 0; q < Latches; q++)
            {
                latch[q] = 0;
            }
            aluOut = 0;
        }

        /// <summary>An ARM/Warp write to the Sol control block (0xD0740000). Returns true once it has
        /// triggered and completed a run (the caller leaves the done bit as we set it).</summary>
        public bool ControlWrite(ulong a, uint v)
        {
            if (a == 0xD0740018 && v == 0 && core.Get(0xD0740020) == 4)
            {
                Run();
                return true;
            }
            return false;
        }

        /// <summary>A write to a Sol unit window (0xD0C00000 + unit * 0x40000). The register and its value
        /// are encoded in the address (window + (reg << 2), data ignored); unit 3's `0x8C0v` group sets
        /// the ALU/SIMD mode register (how KnOS's Modeset works -- a register write, not an ALU op).</summary>
        public void UnitWrite(ulong a)
        {
            var reg = (uint)((a - (0xD0C00000 + 3 * 0x40000)) >> 2); // unit 3 register index
            if ((reg & 0xFF00) == 0x8C00)
            {
                mode = reg & 0xF;
            }
        }

        // -------------------------------------------------------------------------------------------------

        private void Run()
        {
            int pc = 0,
                guard = 0;
            while (guard++ < 4096) // bound: the loop-count param is not decoded (see header)
            {
                ulong m1 = Instr(Mem1, pc);
                ulong m0 = Instr(Mem0, pc);

                // address units (mem0): process loads/stores so operands are in their latches for the op
                Unit(0, (ushort)(m0 & 0xFFFF), (ushort)((m1 >> 16) & 0xFFFF));
                Unit(1, (ushort)((m0 >> 16) & 0xFFFF), (ushort)((m1 >> 16) & 0xFFFF));
                Unit(2, (ushort)((m0 >> 32) & 0xFFFF), (ushort)((m1 >> 16) & 0xFFFF));

                ushort macSlot = (ushort)((m1 >> 32) & 0xFFFF);
                if (((macSlot >> 14) & 3) != 0) // a MAC op is present (op in [15:14]; see header caveat)
                {
                    Mac(macSlot, (ushort)((m1 >> 16) & 0xFFFF));
                }

                ushort aluSlot = (ushort)(m1 >> 48);
                if ((aluSlot >> 10) != 0)
                {
                    Alu(aluSlot, (ushort)((m1 >> 16) & 0xFFFF));
                }

                ushort seq = (ushort)(m1 & 0xFFFF);
                if (seq == 0x0038) // end
                {
                    break;
                }
                if ((seq & 0xF000) == 0xF000 && (seq & 0x0F00) != 0) // 0xF3xx test + backward jump
                {
                    if (loop > 0)
                    {
                        loop--;
                        pc = seq & 0x3F; // target = low 6 bits (docs/module/warp.md, measured)
                        continue;
                    }
                }
                pc++;
            }
            core.Set(0xD0740018, core.Get(0xD0740018) | 1u); // done
        }

        // ---- address units -----------------------------------------------------------------------------

        // A 16-bit address-unit word, dispatched on the top nibble (bits 15:12): 0xF reset (0xF00F),
        // 0x7 load (bit 11 lane 0, bit 12 lane 1, bit 9 bit-reversed index), 0x6/0x5 store (low byte =
        // lane/source select: low nibble bits 1:0 = 11 ALU, 10 MAC). Other nibbles (0xC/0xD) are the
        // address-unit register setup words -- not modelled in this first cut (bank pairs are fixed:
        // unit0 -> banks 6/7, unit1 -> banks 8/9; load index stays at the base, store index steps by 1).
        private void Unit(int n, ushort w, ushort coeff)
        {
            var top = w >> 12;
            if (top == 0xF) // 0xF00F: reset this unit's pointers
            {
                loadPtr[n] = storePtr[n] = 0;
                return;
            }
            var evenBank = BankBase + (ulong)(6 + 2 * n) * BankStride; // lane 0 (unit0 -> 6, unit1 -> 8)
            var oddBank = evenBank + BankStride; // lane 1 (unit0 -> 7, unit1 -> 9)
            if (top == 0x7) // load into this unit's operand latch
            {
                var idx = (w & 0x0200) != 0 ? BitReverse8(loadPtr[n]) : loadPtr[n];
                uint lo = (w & 0x0800) != 0 ? core.Get(evenBank + (ulong)idx * 4) : 0;
                uint hi = (w & 0x1000) != 0 ? core.Get(oddBank + (ulong)idx * 4) : 0;
                latch[4 + n] = ((ulong)hi << 32) | lo;
                // load index is held at its base here (the single-op programs load word 0 twice); the
                // loop-stepping address generator is a documented omission.
            }
            else if (top == 0x6 || top == 0x5) // store a result to the bank pair
            {
                var src = w & 0xFF;
                var val = (src & 3) == 2 ? MacStoreValue() : aluOut; // low nibble 10 = MAC, else the ALU
                var idx = storePtr[n];
                core.Set(evenBank + (ulong)idx * 4, (uint)val); // lane 0
                core.Set(oddBank + (ulong)idx * 4, (uint)(val >> 32)); // lane 1
                storePtr[n] = (byte)(storePtr[n] + 1); // consecutive outputs (biquad loop)
            }
            // else (0x0 nop, 0xC/0xD setup): nothing in this first cut
        }

        // ---- ALU / SIMD --------------------------------------------------------------------------------

        private void Alu(ushort slot, ushort coeff)
        {
            int op = slot >> 10;
            ulong a = Src(slot & 7);
            ulong b = Src((slot >> 3) & 7);
            bool signed = (mode & 1) != 0;
            ulong r = aluOut;
            switch (op)
            {
                case 0x01:
                    r = ~a;
                    break; // Not
                case 0x02:
                    r = a | b;
                    break; // Or
                case 0x03:
                    r = ~(a | b);
                    break; // Nor
                case 0x04:
                    r = a & b;
                    break; // And
                case 0x05:
                    r = ~(a & b);
                    break; // Nand
                case 0x06:
                    r = a ^ b;
                    break; // Xor
                case 0x0B:
                    r = unchecked(a + b);
                    break; // Add (full 64-bit)
                case 0x0C:
                case 0x0E:
                case 0x10:
                case 0x12:
                case 0x13:
                    r = unchecked(a - b);
                    break; // Sub / SubA / SubS / SubAS / Comp = A-B
                case 0x0D:
                    r = Avg64(a, b);
                    break; // AddA = (A+B)>>1, floor (no overflow)
                case 0x0F:
                    r = SatAdd64(a, b, signed);
                    break; // AddS = saturating add
                case 0x11:
                    r = SatAdd64(a, b, signed) >> 1;
                    break; // AddAS = AddS then >>1
                case 0x14:
                case 0x16: // Shift1 / Rot1 (per-lane)
                    r = SimdLanes(a, (int)ShiftCount(b), op == 0x16, false);
                    break;
                case 0x18:
                    r = SimdAddSub(a, ModeShift2(b), false, signed);
                    break; // SimdAdd
                case 0x19:
                case 0x20:
                    r = SimdAddSub(a, ModeShift2(b), true, signed);
                    break; // SimdSub / SimdComp
                case 0x1A:
                    r = SimdAvg(a, b, false);
                    break; // SimdAddA
                case 0x1B:
                    r = SimdAvg(a, b, true);
                    break; // SimdSubA
                case 0x1C:
                case 0x1E:
                    r = SimdSat(a, b, false, signed);
                    break; // SimdAddS / SimdAddAS
                case 0x1D:
                case 0x1F:
                    r = SimdSat(a, b, true, signed);
                    break; // SimdSubS / SimdSubAS
                case 0x21:
                    r = SimdLanes(a, (int)ShiftCount(b), false, false);
                    break; // SimdShift
                case 0x22:
                    r = SimdLanes(a, (int)(b & 0x1F), true, false);
                    break; // SimdRot (rot-left by B&31)
                case 0x23: // Modeset (ALU-op form): mode = imm & 0xF
                    mode = (uint)(slot & 0xF);
                    return; // KnOS sets mode via UnitWrite instead
                case 0x24:
                    r = (ulong)(long)(int)mode;
                    break; // Moderead (sign-extended)
                case 0x25:
                case 0x26:
                case 0x27:
                case 0x28:
                case 0x29:
                case 0x2A:
                case 0x2B:
                case 0x2C:
                    r = (ulong)macResB;
                    break; // undocumented 0x25-0x2C: MAC-acc moves
                default:
                    r = 0;
                    break; // 0x00, 0x07-0x0A, 0x2D-0x3F: no-ops
            }
            aluOut = r;
            latch[3] = r; // source 3 = the ALU feedback
        }

        // SIMD helpers operate on two independent 32-bit lanes; the scalar ops above treat the 64-bit word
        // as one value (Add carries across both lanes, verified on hardware).

        private ulong SimdAddSub(ulong a, ulong b, bool sub, bool signed)
        {
            uint a0 = (uint)a,
                a1 = (uint)(a >> 32),
                b0 = (uint)b,
                b1 = (uint)(b >> 32);
            uint r0 = sub ? unchecked(a0 - b0) : unchecked(a0 + b0);
            uint r1 = sub ? unchecked(a1 - b1) : unchecked(a1 + b1);
            return ((ulong)r1 << 32) | r0;
        }

        private ulong SimdAvg(ulong a, ulong b, bool sub)
        {
            uint a0 = (uint)a,
                a1 = (uint)(a >> 32),
                b0 = (uint)b,
                b1 = (uint)(b >> 32);
            uint r0 = (uint)(((ulong)a0 + (sub ? (ulong)(uint)-(int)b0 : b0)) >> 1);
            uint r1 = (uint)(((ulong)a1 + (sub ? (ulong)(uint)-(int)b1 : b1)) >> 1);
            return ((ulong)r1 << 32) | r0;
        }

        private ulong SimdSat(ulong a, ulong b, bool sub, bool signed)
        {
            uint a0 = (uint)a,
                a1 = (uint)(a >> 32),
                b0 = (uint)b,
                b1 = (uint)(b >> 32);
            return ((ulong)SatAdd32(a1, b1, sub, signed) << 32) | SatAdd32(a0, b0, sub, signed);
        }

        // Shift (logical, signed count: positive left, negative right) or rotate-left, applied per lane.
        private ulong SimdLanes(ulong a, int amt, bool rotate, bool unused)
        {
            uint a0 = (uint)a,
                a1 = (uint)(a >> 32);
            return ((ulong)LaneOp(a1, amt, rotate) << 32) | LaneOp(a0, amt, rotate);
        }

        private static uint LaneOp(uint v, int amt, bool rotate)
        {
            if (rotate)
            {
                amt &= 31;
                return amt == 0 ? v : (v << amt) | (v >> (32 - amt));
            }
            if (amt == 0)
            {
                return v;
            }
            if (amt > 0)
            {
                return amt >= 32 ? 0 : v << amt;
            }
            amt = -amt;
            return amt >= 32 ? 0 : v >> amt;
        }

        // 6-bit signed shift count from src2's low lane (docs/module/warp.md: 45 -> -19 => right shift by 19).
        private static int ShiftCount(ulong b)
        {
            int c = (int)(b & 0x3F);
            return c >= 32 ? c - 64 : c;
        }

        // Modeset bits 3:2 select a right-shift on the SIMD second operand ({0,2,3,4}); 0 for modes 0-3.
        private ulong ModeShift2(ulong b)
        {
            var sel = (mode >> 2) & 3;
            int sh =
                sel == 1 ? 2
                : sel == 2 ? 3
                : sel == 3 ? 4
                : 0;
            if (sh == 0)
            {
                return b;
            }
            uint b0 = (uint)b,
                b1 = (uint)(b >> 32);
            return ((ulong)(b1 >> sh) << 32) | (b0 >> sh);
        }

        private static ulong Avg64(ulong a, ulong b)
        {
            return (a >> 1) + (b >> 1) + (a & b & 1); // floor((a+b)/2) without overflow
        }

        private static ulong SatAdd64(ulong a, ulong b, bool signed)
        {
            if (!signed)
            {
                ulong u = unchecked(a + b);
                return u < a ? ulong.MaxValue : u; // unsigned carry -> clamp high
            }
            long x = (long)a,
                y = (long)b,
                s = unchecked(x + y);
            if (x > 0 && y > 0 && s < 0)
            {
                return 0x7FFFFFFFFFFFFFFFUL;
            }
            if (x < 0 && y < 0 && s >= 0)
            {
                return 0x8000000000000000UL;
            }
            return (ulong)s;
        }

        private static uint SatAdd32(uint a, uint b, bool sub, bool signed)
        {
            if (sub)
            {
                b = unchecked((uint)-(int)b);
            }
            if (!signed)
            {
                ulong u = (ulong)a + b;
                return u > uint.MaxValue ? uint.MaxValue : (uint)u;
            }
            long s = (long)(int)a + (int)b;
            if (s > int.MaxValue)
            {
                return 0x7FFFFFFF;
            }
            if (s < int.MinValue)
            {
                return 0x80000000;
            }
            return (uint)(int)s;
        }

        // ---- MAC (provisional: 16x16 multiply + accumulate; mode Q-format/signed encoding not pinned) ----

        // MAC slot (bits 47:32): [15:14] op (1 Mac, 2 Mult, 3 InitMac), [13:6] macmode, [5:0] operand
        // selects (src1 = the value split into four 16-bit shorts, src2 = the value as two 32-bit lanes).
        // Transcribed from KnOS's MAC reference (== hardware): macmode bits 5:4 and 3:2 pick two of src1's
        // four 16-bit shorts, bits 7 and 6 pick src2's lane for each; it forms two products P1/P2 and keeps
        // two 64-bit accumulators (InitMac sets them to the products, Mac adds, Mult leaves them). The store
        // reads each accumulator arithmetic-shifted right by MacShift (the Q-format). When the mode-state
        // register and macmode bits 7/6 agree it is a signed 32x32 variant into a single accumulator.
        // Accumulators persist across runs (cleared only by InitMac).
        private void Mac(ushort slot, ushort coeff)
        {
            int op = (slot >> 14) & 3; // 1 Mac, 2 Mult, 3 InitMac
            uint macmode = (uint)((slot >> 6) & 0xFF);
            ulong av = Src(slot & 7); // src1: four 16-bit shorts
            ulong bv = Src((slot >> 3) & 7); // src2: two 32-bit lanes
            uint[] ash = { (ushort)av, (ushort)(av >> 16), (ushort)(av >> 32), (ushort)(av >> 48) };
            uint[] blane = { (uint)bv, (uint)(bv >> 32) };
            int bit7 = (int)((macmode >> 7) & 1),
                bit6 = (int)((macmode >> 6) & 1);
            int idx1 = (int)((macmode & 0x30) >> 4); // src1 short for P1 (macmode bits 5:4)
            int idx2 = (int)((macmode & 0x0F) >> 2); // src1 short for P2 (macmode bits 3:2)
            bool unsignedPath = !(
                (macState & 0x800) == 0 && ((macState & 0x1000) != 0 || bit7 == bit6)
            );
            if (unsignedPath)
            {
                long p1 = (long)(int)blane[bit7] * (short)ash[idx1]; // signed 32-bit lane x signed 16-bit short
                long p2 = (long)(int)blane[bit6] * (short)ash[idx2];
                switch (op)
                {
                    case 3:
                        macA = p1;
                        macB = p2;
                        macResA = macA;
                        macResB = macB;
                        break; // InitMac
                    case 1:
                        macA += p1;
                        macB += p2;
                        macResA = macA;
                        macResB = macB;
                        break; // Mac
                    default:
                        macResA = p1;
                        macResB = p2;
                        break; // Mult
                }
            }
            else // signed 32x32 into one accumulator (macB)
            {
                int aval = ((int)(short)(ushort)ash[idx1] << 16) | (int)ash[idx2];
                long p = (long)(int)blane[bit6] * (long)aval;
                switch (op)
                {
                    case 3:
                        macA = 0;
                        macB = p;
                        macResA = macA;
                        macResB = macB;
                        break; // InitMac
                    case 1:
                        macB += p;
                        macResA = macA;
                        macResB = macB;
                        break; // Mac
                    default:
                        macResA = macA;
                        macResB = p;
                        break; // Mult
                }
            }
        }

        // The 0x60F2 store reads the two MAC accumulators, each arithmetic-shifted right by MacShift (the
        // Q-format): lane 0 (even bank) = accumulator B >> MacShift, lane 1 (odd bank) = accumulator A.
        private ulong MacStoreValue()
        {
            uint lo = (uint)(macResB >> MacShift);
            uint hi = (uint)(macResA >> MacShift);
            return ((ulong)hi << 32) | lo;
        }

        // ---- operand sources ---------------------------------------------------------------------------

        private ulong Src(int sel) => sel < Latches ? latch[sel] : 0;

        private ulong Instr(ulong bank, int i)
        {
            var baseAddr = bank + (ulong)i * 8;
            return ((ulong)core.Get(baseAddr + 4) << 32) | core.Get(baseAddr);
        }

        private static byte BitReverse8(byte v)
        {
            int r = 0;
            for (var i = 0; i < 8; i++)
            {
                r = (r << 1) | ((v >> i) & 1);
            }
            return (byte)r;
        }

        private const int Units = 3;
        private const int Latches = 8;
        private const ulong BankBase = 0xA8000000;
        private const ulong BankStride = 0x400;
        private const ulong Mem1 = 0xD0760000;
        private const ulong Mem0 = 0xD0720000;
        private const int MacShift = 0; // the MAC output Q-format shift (*DAT_603b544c); 0 as the unit runs it

        private readonly CR870_Core core;
        private uint mode;
        private uint macState;
        private long macA,
            macB,
            macResA,
            macResB;
        private ulong aluOut;
        private int loop;
        private readonly byte[] loadPtr = new byte[Units];
        private readonly byte[] storePtr = new byte[Units];
        private readonly ulong[] latch = new ulong[Latches];
    }
}
