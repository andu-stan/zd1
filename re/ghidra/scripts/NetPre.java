// Pre-script for DM870 images: add SRAM / SDRAM-bss blocks
import ghidra.app.script.GhidraScript;
import ghidra.program.model.address.*;
import ghidra.program.model.mem.*;

public class NetPre extends GhidraScript {
    void blk(String name, long start, long len, boolean vol) throws Exception {
        Memory m = currentProgram.getMemory();
        try {
            MemoryBlock b = m.createUninitializedBlock(name, toAddr(start), len, false);
            b.setRead(true);
            b.setWrite(true);
            b.setExecute(false);
            if (vol) b.setVolatile(true);
        } catch (Exception e) {
            println("blk " + name + ": " + e);
        }
    }

    public void run() throws Exception {
        long end = 0;
        for (MemoryBlock mb : currentProgram.getMemory().getBlocks()) {
            mb.setExecute(true);
            mb.setWrite(true);
            long e = mb.getEnd().getOffset() + 1;
            if (e > end) end = e;
        }
        long ep = 0;
        for (MemoryBlock mb : currentProgram.getMemory().getBlocks()) {
            if (mb.isInitialized()) {
                ep = mb.getStart().getOffset();
                break;
            }
        }
        currentProgram.getSymbolTable().addExternalEntryPoint(toAddr(ep));
        disassemble(toAddr(ep));
        blk("SRAM", 0x00400000L, 0x10000L, false);
        if (end > 0x60000000L && end < 0x64000000L)
            blk(
                    "SDRAM_HI",
                    (end + 0xfff) & ~0xfffL,
                    0x64000000L - ((end + 0xfff) & ~0xfffL),
                    false);
    }
}
