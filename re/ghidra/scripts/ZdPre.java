// Pre-script: create UC3A1 memory map blocks and peripheral labels
import ghidra.app.script.GhidraScript;
import ghidra.program.model.address.*;
import ghidra.program.model.mem.*;
import ghidra.program.model.symbol.*;

public class ZdPre extends GhidraScript {
    void blk(String name, long start, long len, boolean vol) throws Exception {
        Memory m = currentProgram.getMemory();
        Address a = toAddr(start);
        MemoryBlock b = m.createUninitializedBlock(name, a, len, false);
        b.setRead(true);
        b.setWrite(true);
        b.setExecute(false);
        if (vol) b.setVolatile(true);
    }

    void lab(String n, long a) throws Exception {
        currentProgram.getSymbolTable().createLabel(toAddr(a), n, SourceType.USER_DEFINED);
    }

    public void run() throws Exception {
        for (MemoryBlock mb : currentProgram.getMemory().getBlocks()) {
            if (mb.contains(toAddr(0x80000000L))) {
                mb.setWrite(false);
                mb.setExecute(true);
                mb.setName("FLASH");
            }
        }
        blk("SRAM_lo", 0x0L, 0x8L, false);
        byte[] dat = new byte[0x3628];
        currentProgram.getMemory().getBytes(toAddr(0x80034ad0L), dat);
        MemoryBlock d =
                currentProgram
                        .getMemory()
                        .createInitializedBlock(
                                ".data",
                                toAddr(0x8L),
                                new java.io.ByteArrayInputStream(dat),
                                0x3628L,
                                monitor,
                                false);
        d.setRead(true);
        d.setWrite(true);
        blk(".bss_heap_stack", 0x3630L, 0x10000L - 0x3630L, false);
        blk("USERPAGE", 0x80800000L, 0x200L, false);
        blk("USBFIFO", 0xE0000000L, 0x10000L, true);
        blk("HSBPERIPH", 0xFFFE0000L, 0x3000L, true);
        blk("PBPERIPH", 0xFFFF0000L, 0x4000L, true);
        String[][] p = {
            {"USBB", "FFFE0000"},
            {"HMATRIX", "FFFE1000"},
            {"FLASHC", "FFFE1400"},
            {"MACB", "FFFE1800"},
            {"SMC", "FFFE1C00"},
            {"SDRAMC", "FFFE2000"},
            {"PDCA", "FFFF0000"},
            {"INTC", "FFFF0800"},
            {"PM", "FFFF0C00"},
            {"RTC", "FFFF0D00"},
            {"WDT", "FFFF0D30"},
            {"EIC", "FFFF0D80"},
            {"GPIO", "FFFF1000"},
            {"USART0", "FFFF1400"},
            {"USART1", "FFFF1800"},
            {"USART2", "FFFF1C00"},
            {"USART3", "FFFF2000"},
            {"SPI0", "FFFF2400"},
            {"SPI1", "FFFF2800"},
            {"TWI", "FFFF2C00"},
            {"PWM", "FFFF3000"},
            {"SSC", "FFFF3400"},
            {"TC", "FFFF3800"},
            {"ADC", "FFFF3C00"}
        };
        for (String[] e : p) lab(e[0], Long.parseLong(e[1], 16));
        for (int port = 0; port < 4; port++) lab("GPIO_PORT" + port, 0xFFFF1000L + port * 0x100);
    }
}
