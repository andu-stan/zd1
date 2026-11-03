// Post-script (run -noanalysis on a COPY of zd1): create functions at code addresses referenced by
// literal-pool
// words / call refs / orphan prologues that are not covered by any function, then decompile those
// new functions.
// args: outfile lo hi   (only dump new functions whose entry is in [lo,hi))
import ghidra.app.cmd.disassemble.DisassembleCommand;
import ghidra.app.cmd.function.CreateFunctionCmd;
import ghidra.app.decompiler.*;
import ghidra.app.script.GhidraScript;
import ghidra.program.model.address.*;
import ghidra.program.model.listing.*;
import ghidra.program.model.mem.*;
import ghidra.program.model.symbol.*;

import java.io.*;
import java.util.*;

public class OrphanDec extends GhidraScript {
    public void run() throws Exception {
        String[] a = getScriptArgs();
        long lo = Long.parseLong(a[1], 16), hi = Long.parseLong(a[2], 16);
        Memory mem = currentProgram.getMemory();
        Listing lst = currentProgram.getListing();
        TreeSet<Long> cand = new TreeSet<>();
        for (long p = 0x80008000L; p < 0x80034000L; p += 2) {
            long v;
            try {
                v = mem.getInt(toAddr(p)) & 0xffffffffL;
            } catch (Exception e) {
                continue;
            }
            if (v >= 0x80008000L && v < 0x80031884L && (v & 1) == 0) cand.add(v);
            Address ad = toAddr(p);
            if (lst.getInstructionAt(ad) != null && getFunctionContaining(ad) == null) {
                int h = mem.getShort(ad) & 0xffff;
                if (h == 0xEBCD || ((h & 0xF00F) == 0xD001 && (h & 0x0FF0) != 0)) cand.add(p);
            }
        }
        for (Reference r :
                currentProgram.getReferenceManager().getReferenceIterator(toAddr(0x80008000L))) {
            if (r.getReferenceType().isCall()) cand.add(r.getToAddress().getOffset());
        }
        List<Function> made = new ArrayList<>();
        for (int pass = 0; pass < 3; pass++) {
            int n = 0;
            for (long v : cand) {
                Address ad = toAddr(v);
                if (getFunctionContaining(ad) != null) continue;
                if (lst.getDefinedDataContaining(ad) != null) continue;
                if (lst.getInstructionAt(ad) == null) {
                    if (pass == 0) continue; // only previously-disassembled code on first pass
                    // only disassemble if it looks like a prologue
                    int h = mem.getShort(ad) & 0xffff;
                    if (!(h == 0xEBCD || ((h & 0xF00F) == 0xD001 && (h & 0x0FF0) != 0))) continue;
                    new DisassembleCommand(ad, null, true).applyTo(currentProgram, monitor);
                }
                if (lst.getInstructionAt(ad) == null) continue;
                new CreateFunctionCmd(ad).applyTo(currentProgram, monitor);
                Function f = getFunctionAt(ad);
                if (f != null) {
                    made.add(f);
                    n++;
                }
            }
            println("pass " + pass + " created " + n);
        }
        DecompInterface di = new DecompInterface();
        di.openProgram(currentProgram);
        PrintWriter out = new PrintWriter(new FileWriter(a[0]));
        made.sort((x, y) -> x.getEntryPoint().compareTo(y.getEntryPoint()));
        for (Function f : made) {
            long e = f.getEntryPoint().getOffset();
            if (e < lo || e >= hi) continue;
            out.println(
                    "// ===== FUN_"
                            + Long.toHexString(e)
                            + " @ "
                            + Long.toHexString(e)
                            + " size "
                            + f.getBody().getNumAddresses());
            DecompileResults r = di.decompileFunction(f, 90, monitor);
            if (r != null && r.decompileCompleted()) out.println(r.getDecompiledFunction().getC());
            else out.println("// decompile failed");
        }
        out.close();
        PrintWriter fl = new PrintWriter(new FileWriter(a[0] + ".funcs"));
        for (Function f : currentProgram.getFunctionManager().getFunctions(true))
            fl.println(f.getEntryPoint() + " " + f.getName() + " " + f.getBody().getNumAddresses());
        fl.close();
    }
}
