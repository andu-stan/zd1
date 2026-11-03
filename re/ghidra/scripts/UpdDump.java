// Post-script: decompile all functions of the updater dylib/exe
import ghidra.app.decompiler.*;
import ghidra.app.script.GhidraScript;
import ghidra.program.model.listing.*;

import java.io.*;

public class UpdDump extends GhidraScript {
    public void run() throws Exception {
        String out = getScriptArgs()[0];
        DecompInterface di = new DecompInterface();
        di.openProgram(currentProgram);
        PrintWriter all = new PrintWriter(new FileWriter(out));
        for (Function f : currentProgram.getFunctionManager().getFunctions(true)) {
            if (f.isExternal() || f.isThunk()) continue;
            DecompileResults r = di.decompileFunction(f, 120, monitor);
            all.println("// ===== " + f.getName() + " @ " + f.getEntryPoint());
            if (r != null && r.decompileCompleted()) all.println(r.getDecompiledFunction().getC());
            else all.println("// decompile failed");
        }
        all.close();
    }
}
