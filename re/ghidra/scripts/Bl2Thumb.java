// -process script: recover THUMB functions by explicitly setting the TMode context
// register = 1 at each Thumb prologue (PUSH {..,lr} = 0xB5xx), then disassembling.
import ghidra.app.cmd.function.CreateFunctionCmd;
import ghidra.app.script.GhidraScript;
import ghidra.program.model.address.*;
import ghidra.program.model.lang.Register;
import ghidra.program.model.listing.*;
import ghidra.program.model.listing.ProgramContext;
import ghidra.program.model.mem.*;

import java.math.BigInteger;

public class Bl2Thumb extends GhidraScript {
    Register tmode;

    boolean undef(Address ad) {
        if (getInstructionAt(ad) != null) return false;
        if (currentProgram.getListing().getDefinedDataAt(ad) != null) return false;
        if (getFunctionContaining(ad) != null) return false;
        return true;
    }

    long mkthumb(Address ad) {
        try {
            ProgramContext ctx = currentProgram.getProgramContext();
            // set TMode=1 over a window so the disassembler stays in Thumb
            Address end = ad.add(0x400);
            ctx.setValue(tmode, ad, end, BigInteger.ONE);
            disassemble(ad);
            if (getInstructionAt(ad) != null) {
                CreateFunctionCmd fc = new CreateFunctionCmd(ad);
                fc.applyTo(currentProgram, monitor);
                if (getFunctionAt(ad) != null) return 1;
            }
        } catch (Exception ex) {
        }
        return 0;
    }

    public void run() throws Exception {
        tmode = currentProgram.getRegister("TMode");
        if (tmode == null) {
            println("no TMode register!");
            return;
        }
        Memory mem = currentProgram.getMemory();
        long thumb = 0, cand = 0;
        // seed first
        Address cb = toAddr(0x60040de4L);
        Function f = getFunctionAt(cb);
        if (f != null && f.getBody().getNumAddresses() < 8) removeFunction(f);
        long seedres = mkthumb(cb);
        Function sf = getFunctionAt(cb);
        println(
                "seed 60040de4 thumb-create="
                        + seedres
                        + " size="
                        + (sf == null ? 0 : sf.getBody().getNumAddresses()));
        for (MemoryBlock mb : mem.getBlocks()) {
            if (!mb.isInitialized() || !mb.isExecute()) continue;
            long s = mb.getStart().getOffset(), e = mb.getEnd().getOffset();
            if (e > 0x60098000L) e = 0x60098000L;
            for (long a = s; a + 2 <= e; a += 2) {
                if ((mem.getByte(toAddr(a + 1)) & 0xff) != 0xB5) continue;
                cand++;
                Address ad = toAddr(a);
                if (undef(ad)) thumb += mkthumb(ad);
            }
        }
        println("Bl2Thumb: thumb candidates=" + cand + " created=" + thumb);
        try {
            ghidra.app.plugin.core.analysis.AutoAnalysisManager mgr =
                    ghidra.app.plugin.core.analysis.AutoAnalysisManager.getAnalysisManager(
                            currentProgram);
            mgr.reAnalyzeAll(null);
            mgr.startAnalysis(monitor);
        } catch (Exception ex) {
            println("reanalyze: " + ex);
        }
    }
}
