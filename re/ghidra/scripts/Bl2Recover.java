// Post-script (run on a FRESH import): recover functions in the mixed ARM/Thumb binary.
//  - ARM prologue  STMFD sp!,{...,lr}  (word 0xE92D_4xxx) -> ArmDisassembleCommand(false)
//  - Thumb prologue PUSH {..,lr} (halfword 0xB5xx)        -> set TB=1 context, disassemble()
import ghidra.app.cmd.disassemble.ArmDisassembleCommand;
import ghidra.app.cmd.function.CreateFunctionCmd;
import ghidra.app.script.GhidraScript;
import ghidra.program.model.address.*;
import ghidra.program.model.lang.Register;
import ghidra.program.model.listing.*;
import ghidra.program.model.mem.*;

import java.math.BigInteger;

public class Bl2Recover extends GhidraScript {
    Register TB;

    boolean undef(Address ad) {
        if (getInstructionAt(ad) != null) return false;
        if (currentProgram.getListing().getDefinedDataAt(ad) != null) return false;
        if (getFunctionContaining(ad) != null) return false;
        return true;
    }

    void clearStub(Address ad) {
        try {
            Function f = getFunctionAt(ad);
            if (f != null && f.getBody().getNumAddresses() < 8) removeFunction(f);
            currentProgram.getListing().clearCodeUnits(ad, ad.add(2), false, monitor);
        } catch (Exception e) {
        }
    }

    long mkArm(Address ad) {
        try {
            clearStub(ad);
            ArmDisassembleCommand dc = new ArmDisassembleCommand(ad, null, false);
            dc.applyTo(currentProgram, monitor);
            if (getInstructionAt(ad) != null) {
                new CreateFunctionCmd(ad).applyTo(currentProgram, monitor);
                if (getFunctionAt(ad) != null) return 1;
            }
        } catch (Exception e) {
        }
        return 0;
    }

    long mkThumb(Address ad) {
        try {
            clearStub(ad);
            currentProgram.getProgramContext().setValue(TB, ad, ad.add(0x40), BigInteger.ONE);
            disassemble(ad);
            if (getInstructionAt(ad) != null) {
                new CreateFunctionCmd(ad).applyTo(currentProgram, monitor);
                if (getFunctionAt(ad) != null) return 1;
            }
        } catch (Exception e) {
        }
        return 0;
    }

    public void run() throws Exception {
        TB = currentProgram.getRegister("TB");
        Memory mem = currentProgram.getMemory();
        long arm = 0, th = 0;
        // explicit region thumb disassembly for flow-isolated validators.
        // checkBCDAndFlashStatus ~0x60040de4; generic BCD validator region ~0x60033400.
        long[][] regions = {{0x60040c00L, 0x60041260L}, {0x60033380L, 0x60034000L}};
        for (long[] rg : regions) {
            Address rs = toAddr(rg[0]), re = toAddr(rg[1]);
            try {
                for (Function fn : currentProgram.getFunctionManager().getFunctions(rs, true)) {
                    if (fn.getEntryPoint().compareTo(re) > 0) break;
                    if (fn.getBody().getNumAddresses() < 8) removeFunction(fn);
                }
                currentProgram.getListing().clearCodeUnits(rs, re, false, monitor);
                currentProgram.getProgramContext().setValue(TB, rs, re, BigInteger.ONE);
            } catch (Exception e) {
                println("region clear " + e);
            }
            // disassemble from every even offset that is a thumb push prologue
            for (long x = rg[0]; x < rg[1]; x += 2) {
                int b;
                try {
                    b = mem.getByte(toAddr(x + 1)) & 0xff;
                } catch (Exception e) {
                    continue;
                }
                if (b != 0xB5) continue;
                Address ad = toAddr(x);
                try {
                    disassemble(ad);
                    if (getInstructionAt(ad) != null)
                        new CreateFunctionCmd(ad).applyTo(currentProgram, monitor);
                } catch (Exception e) {
                }
            }
        }
        Address cb = toAddr(0x60040de4L);
        Function sf = getFunctionAt(cb);
        println("checkBCD 60040de4 size=" + (sf == null ? 0 : sf.getBody().getNumAddresses()));
        for (MemoryBlock mb : mem.getBlocks()) {
            if (!mb.isInitialized() || !mb.isExecute()) continue;
            long s = mb.getStart().getOffset(), e = mb.getEnd().getOffset();
            if (e > 0x60098000L) e = 0x60098000L;
            for (long a = s; a + 4 <= e; a += 4) {
                if ((mem.getByte(toAddr(a + 3)) & 0xff) != 0xE9) continue;
                if ((mem.getByte(toAddr(a + 2)) & 0xff) != 0x2D) continue;
                if ((mem.getByte(toAddr(a + 1)) & 0x40) == 0) continue;
                Address ad = toAddr(a);
                if (undef(ad)) arm += mkArm(ad);
            }
            for (long a = s; a + 2 <= e; a += 2) {
                if ((mem.getByte(toAddr(a + 1)) & 0xff) != 0xB5) continue;
                Address ad = toAddr(a);
                if (undef(ad)) th += mkThumb(ad);
            }
        }
        // stub cleanup pass: re-force any function with body<8
        java.util.List<Address> stubs = new java.util.ArrayList<>();
        for (Function fn : currentProgram.getFunctionManager().getFunctions(true))
            if (fn.getBody().getNumAddresses() < 8) stubs.add(fn.getEntryPoint());
        long sf2 = 0;
        for (Address ad : stubs) {
            int b = -1;
            try {
                b = mem.getByte(ad.add(1)) & 0xff;
            } catch (Exception e) {
            }
            sf2 += (b == 0xB5) ? mkThumb(ad) : mkArm(ad);
        }
        println(
                "Bl2Recover: ARM="
                        + arm
                        + " THUMB="
                        + th
                        + " stubs="
                        + stubs.size()
                        + " stubFixed="
                        + sf2);
        try {
            ghidra.app.plugin.core.analysis.AutoAnalysisManager mgr =
                    ghidra.app.plugin.core.analysis.AutoAnalysisManager.getAnalysisManager(
                            currentProgram);
            mgr.reAnalyzeAll(null);
            mgr.startAnalysis(monitor);
        } catch (Exception e) {
            println("reanalyze " + e);
        }
    }
}
