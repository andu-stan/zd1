// Post-script: decompile all functions in parallel, dump C + function list + string xref index
import ghidra.app.decompiler.*;
import ghidra.app.decompiler.parallel.*;
import ghidra.app.script.GhidraScript;
import ghidra.program.model.address.*;
import ghidra.program.model.data.*;
import ghidra.program.model.listing.*;
import ghidra.program.model.symbol.*;
import ghidra.util.task.TaskMonitor;

import java.io.*;
import java.util.*;

public class NetDump extends GhidraScript {
    public void run() throws Exception {
        String outdir = getScriptArgs()[0];
        new File(outdir).mkdirs();
        Listing lst = currentProgram.getListing();
        PrintWriter fl = new PrintWriter(new FileWriter(outdir + "/functions.txt"));
        List<Function> fns = new ArrayList<>();
        for (Function f : currentProgram.getFunctionManager().getFunctions(true)) {
            fns.add(f);
            fl.println(f.getEntryPoint() + " " + f.getName() + " " + f.getBody().getNumAddresses());
        }
        fl.close();
        // string xrefs
        PrintWriter sx = new PrintWriter(new FileWriter(outdir + "/string_xrefs.txt"));
        for (Data d : lst.getDefinedData(true)) {
            if (!(d.getDataType() instanceof AbstractStringDataType)) continue;
            Object v = d.getValue();
            if (v == null) continue;
            String s = v.toString().replace("\n", "\\n").replace("\r", "\\r");
            StringBuilder sb = new StringBuilder();
            for (Reference r : getReferencesTo(d.getAddress())) {
                Function f = getFunctionContaining(r.getFromAddress());
                sb.append(" ")
                        .append(r.getFromAddress())
                        .append("(")
                        .append(f == null ? "?" : f.getName())
                        .append(")");
            }
            sx.println(d.getAddress() + "\t" + s + "\t|" + sb);
        }
        sx.close();
        final PrintWriter all = new PrintWriter(new FileWriter(outdir + "/decomp_all.c"));
        DecompilerCallback<String> cb =
                new DecompilerCallback<String>(
                        currentProgram,
                        new DecompileConfigurer() {
                            public void configure(DecompInterface d) {
                                d.toggleCCode(true);
                                d.toggleSyntaxTree(false);
                                d.setSimplificationStyle("decompile");
                            }
                        }) {
                    public String process(DecompileResults r, TaskMonitor m) throws Exception {
                        Function f = r.getFunction();
                        String c =
                                r.getDecompiledFunction() == null
                                        ? "/* decompile failed: " + r.getErrorMessage() + " */"
                                        : r.getDecompiledFunction().getC();
                        return "\n// ==== FUNCTION "
                                + f.getEntryPoint()
                                + " "
                                + f.getName()
                                + "\n"
                                + c;
                    }
                };
        cb.setTimeout(60);
        List<String> res = ParallelDecompiler.decompileFunctions(cb, fns, monitor);
        TreeMap<String, String> sorted = new TreeMap<>();
        for (String s : res)
            if (s != null) {
                int i = s.indexOf("FUNCTION ") + 9;
                sorted.put(s.substring(i, s.indexOf(' ', i)), s);
            }
        for (String s : sorted.values()) all.println(s);
        all.close();
        cb.dispose();
        println("dumped " + fns.size() + " functions");
    }
}
