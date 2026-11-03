// Memories the AVR32 core reads and writes directly (big-endian byte arrays), bypassing the system bus.
//
// The core finds every IAvr32DirectMemory registered on the system bus and serves CPU accesses inside it from Data;
// the same object stays a normal bus peripheral for everyone else (DMA, the monitor, other models). Instruction
// decoding is cached per halfword for these regions; writes through Write*() invalidate the cache.
using System;
using Antmicro.Renode.Core;
using Antmicro.Renode.Peripherals.Bus;

namespace Antmicro.Renode.Peripherals.Memory
{
    public interface IAvr32DirectMemory : IBusPeripheral
    {
        /// <summary>Backing store, big-endian. Its length is the size of the CPU-visible region.</summary>
        byte[] Data { get; }

        /// <summary>CPU stores go straight to Data (SRAM). False: they go over the bus (flash page buffer).</summary>
        bool CpuWritable { get; }

        /// <summary>Raised when Data changes behind the CPU's back (flash programming, DMA, loading an image).</summary>
        event Action<int, int> Changed;
    }

    /// <summary>Plain RAM: SRAM and other zero-wait-state memories the CPU uses directly.</summary>
    public class Avr32Ram
        : IAvr32DirectMemory,
            IBytePeripheral,
            IWordPeripheral,
            IDoubleWordPeripheral,
            IKnownSize
    {
        public Avr32Ram(long size)
        {
            data = new byte[size];
        }

        public byte[] Data => data;
        public bool CpuWritable => true;
        public long Size => data.Length;
        public event Action<int, int> Changed;

        public void Reset()
        {
            // RAM keeps its contents across a chip reset; a power cycle is a new machine (see emu/renode/README.md)
        }

        /// <summary>Fill with a byte (power-on contents).</summary>
        public void Fill(byte value)
        {
            for (var i = 0; i < data.Length; i++)
            {
                data[i] = value;
            }
            Changed?.Invoke(0, data.Length);
        }

        public byte ReadByte(long offset) => data[offset];

        public ushort ReadWord(long offset) => (ushort)(data[offset] << 8 | data[offset + 1]);

        public uint ReadDoubleWord(long offset) =>
            (uint)(
                data[offset] << 24
                | data[offset + 1] << 16
                | data[offset + 2] << 8
                | data[offset + 3]
            );

        public void WriteByte(long offset, byte value)
        {
            data[offset] = value;
            Changed?.Invoke((int)offset, 1);
        }

        public void WriteWord(long offset, ushort value)
        {
            data[offset] = (byte)(value >> 8);
            data[offset + 1] = (byte)value;
            Changed?.Invoke((int)offset, 2);
        }

        public void WriteDoubleWord(long offset, uint value)
        {
            data[offset] = (byte)(value >> 24);
            data[offset + 1] = (byte)(value >> 16);
            data[offset + 2] = (byte)(value >> 8);
            data[offset + 3] = (byte)value;
            Changed?.Invoke((int)offset, 4);
        }

        /// <summary>Copy bytes in (image loading from scripts).</summary>
        public void Load(byte[] bytes, long offset)
        {
            Array.Copy(bytes, 0, data, offset, Math.Min(bytes.Length, data.Length - offset));
            Changed?.Invoke((int)offset, bytes.Length);
        }

        private readonly byte[] data;
    }
}
