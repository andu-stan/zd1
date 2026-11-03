// Pre-script for DM870 BL2 stage2 flat binary (base 0x60008000).
// Adds SRAM, high-SDRAM bss, and peripheral MMIO blocks. No brute sweep (that ate
// the interleaved C++ string literals); function recovery is done in Bl2Recover.
import ghidra.app.script.GhidraScript;
import ghidra.program.model.address.*;
import ghidra.program.model.listing.*;
import ghidra.program.model.mem.*;

public class Bl2Pre extends GhidraScript {
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
        long istart = 0, iend = 0;
        for (MemoryBlock mb : currentProgram.getMemory().getBlocks()) {
            mb.setExecute(true);
            mb.setWrite(true);
            if (mb.isInitialized()) {
                istart = mb.getStart().getOffset();
                iend = mb.getEnd().getOffset() + 1;
            }
        }
        currentProgram.getSymbolTable().addExternalEntryPoint(toAddr(istart));
        disassemble(toAddr(istart));
        blk("SRAM", 0x00400000L, 0x20000L, false);
        if (iend > 0x60000000L && iend < 0x62000000L)
            blk(
                    "SDRAM_HI",
                    (iend + 0xfffL) & ~0xfffL,
                    0x62000000L - ((iend + 0xfffL) & ~0xfffL),
                    false);
        blk("SDRAM_NC", 0x71000000L, 0x01000000L, false);
        blk("P_E0", 0xE0000000L, 0x00040000L, true);
        blk("P_B1", 0xB1000000L, 0x00010000L, true);
        blk("P_B5", 0xB5000000L, 0x00010000L, true);
        blk("P_IRQ", 0xFFFF0000L, 0x00010000L, true);
    }
}
