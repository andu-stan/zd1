// Post-script: find more functions, decompile everything, dump to files
import ghidra.app.cmd.disassemble.DisassembleCommand;
import ghidra.app.cmd.function.CreateFunctionCmd;
import ghidra.app.decompiler.*;
import ghidra.app.script.GhidraScript;
import ghidra.program.model.address.*;
import ghidra.program.model.listing.*;
import ghidra.program.model.mem.*;

import java.io.*;
import java.util.*;

public class ZdDump extends GhidraScript {
    public void run() throws Exception {
        String outdir = getScriptArgs().length > 0 ? getScriptArgs()[0] : "/tmp/zd";
        new File(outdir).mkdirs();
        Memory mem = currentProgram.getMemory();
        Listing lst = currentProgram.getListing();
        // seed: disassemble at app entry
        long[] seeds = {0x80000000L, 0x80008000L};
        for (long s : seeds) {
            disassemble(toAddr(s));
        }
        analyzeAll(currentProgram);
        // scan for function prologues: stm --sp,... (0xEBCD) and pushm (0xD4x1 with low nibble 1 =>
        // pushm)
        int pass = 0;
        while (pass < 3) {
            int added = 0;
            for (long a = 0x80008000L; a < 0x80031884L; a += 2) {
                Address ad = toAddr(a);
                if (lst.getInstructionContaining(ad) != null) continue;
                if (lst.getDefinedDataContaining(ad) != null) continue;
                int h = mem.getShort(ad) & 0xffff;
                boolean cand = (h == 0xEBCD) || ((h & 0xF00F) == 0xD001 && (h & 0x0FF0) != 0);
                if (!cand) continue;
                DisassembleCommand dc = new DisassembleCommand(ad, null, true);
                dc.applyTo(currentProgram, monitor);
                if (lst.getInstructionAt(ad) != null && getFunctionAt(ad) == null) {
                    new CreateFunctionCmd(ad).applyTo(currentProgram, monitor);
                    added++;
                }
            }
            analyzeAll(currentProgram);
            println("pass " + pass + " added " + added);
            if (added == 0) break;
            pass++;
        }
        DecompInterface di = new DecompInterface();
        di.openProgram(currentProgram);
        PrintWriter all = new PrintWriter(new FileWriter(outdir + "/decomp_all.c"));
        PrintWriter fl = new PrintWriter(new FileWriter(outdir + "/functions.txt"));
        int n = 0;
        for (Function f : currentProgram.getFunctionManager().getFunctions(true)) {
            fl.println(f.getEntryPoint() + " " + f.getName() + " " + f.getBody().getNumAddresses());
            DecompileResults r = di.decompileFunction(f, 60, monitor);
            all.println("// ===== " + f.getName() + " @ " + f.getEntryPoint());
            if (r != null && r.decompileCompleted()) all.println(r.getDecompiledFunction().getC());
            else all.println("// decompile failed");
            n++;
        }
        all.close();
        fl.close();
        // disassembly listing
        PrintWriter dis = new PrintWriter(new FileWriter(outdir + "/disasm.txt"));
        InstructionIterator it = lst.getInstructions(true);
        Function cur = null;
        while (it.hasNext()) {
            Instruction ins = it.next();
            Function f = getFunctionContaining(ins.getAddress());
            if (f != null && f != cur && f.getEntryPoint().equals(ins.getAddress()))
                dis.println("\n; ---- " + f.getName());
            cur = f;
            dis.println(ins.getAddress() + "  " + ins.toString());
        }
        dis.close();
        println("functions: " + n);
    }
}
