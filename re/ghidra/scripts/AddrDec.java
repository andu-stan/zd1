// Post-script (run with -noanalysis on a COPY of the zd1 project):
// args: outfile addr1 addr2 ...  -> disassemble+create function at each addr (if missing),
// decompile, dump C + disasm
import ghidra.app.cmd.disassemble.DisassembleCommand;
import ghidra.app.cmd.function.CreateFunctionCmd;
import ghidra.app.decompiler.*;
import ghidra.app.script.GhidraScript;
import ghidra.program.model.address.*;
import ghidra.program.model.listing.*;

import java.io.*;

public class AddrDec extends GhidraScript {
    public void run() throws Exception {
        String[] a = getScriptArgs();
        PrintWriter out = new PrintWriter(new FileWriter(a[0]));
        Listing lst = currentProgram.getListing();
        DecompInterface di = new DecompInterface();
        di.openProgram(currentProgram);
        for (int i = 1; i < a.length; i++) {
            Address ad = toAddr(Long.parseLong(a[i].replace("0x", ""), 16));
            if (lst.getInstructionAt(ad) == null) {
                DisassembleCommand dc = new DisassembleCommand(ad, null, true);
                dc.applyTo(currentProgram, monitor);
            }
            Function f = getFunctionAt(ad);
            if (f == null) {
                Function g = getFunctionContaining(ad);
                if (g != null && !g.getEntryPoint().equals(ad)) {
                    removeFunction(g);
                }
                new CreateFunctionCmd(ad).applyTo(currentProgram, monitor);
                f = getFunctionAt(ad);
            }
            out.println("// ===== FUN_" + a[i].replace("0x", "") + " @ " + a[i].replace("0x", ""));
            if (f == null) {
                out.println("// could not create function");
                continue;
            }
            DecompileResults r = di.decompileFunction(f, 90, monitor);
            if (r != null && r.decompileCompleted()) out.println(r.getDecompiledFunction().getC());
            else out.println("// decompile failed");
            out.println("// ---- disasm");
            InstructionIterator it = lst.getInstructions(f.getBody(), true);
            while (it.hasNext()) {
                Instruction ins = it.next();
                out.println("// " + ins.getAddress() + "  " + ins.toString());
            }
        }
        out.close();
    }
}
