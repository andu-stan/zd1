# The MCU firmware (AT32UC3A1256; composite USB: UAC1 audio + HID + the host link). App at 0x80004000 (B&W DFU layout).
# Microchip's AVR32 GNU toolchain on PATH (an x86_64 Linux binary): `nix build .#mcu` runs this (nix/mcu/firmware.nix).
#   make [OUT=zd1] [EXTRA_CFLAGS=...]    -> build/OUT.{elf,hex,bin,map}, objects in build/obj/OUT/
OUT ?= zd1
CROSS ?= avr32-
CC := $(CROSS)gcc
OBJCOPY := $(CROSS)objcopy
SIZE := $(CROSS)size

ASF := asf
B := build
OBJ := $(B)/obj/$(OUT)

# the vendored ASF subset; the link order is the order here (the image depends on it)
ASSRCS := \
  avr32/drivers/intc/exception.S \
  avr32/utils/startup/startup_uc3.S
CSRCS := \
  avr32/drivers/flashc/flashc.c \
  avr32/drivers/gpio/gpio.c \
  avr32/drivers/intc/intc.c \
  avr32/drivers/pm/pm.c \
  avr32/drivers/pm/pm_conf_clocks.c \
  avr32/drivers/pm/power_clocks_lib.c \
  avr32/drivers/spi/spi.c \
  avr32/drivers/twi/twi.c \
  avr32/drivers/pwm/pwm.c \
  avr32/drivers/adc/adc.c \
  avr32/drivers/wdt/wdt.c \
  avr32/drivers/usbb/usbb_device.c \
  avr32/drivers/pdca/pdca.c \
  avr32/drivers/ssc/i2s/ssc_i2s.c \
  common/services/clock/uc3a0_a1/sysclk.c \
  common/services/sleepmgr/uc3/sleepmgr.c \
  common/services/usb/class/hid/device/udi_hid.c \
  common/services/usb/class/composite/device/udi_composite_desc.c \
  common/services/usb/udc/udc.c
# the application: every src/*.c, in byte order ($(sort))
APPSRCS := $(sort $(wildcard src/*.c))

INCS := \
  avr32/drivers/flashc avr32/drivers/gpio avr32/drivers/intc avr32/drivers/pm avr32/drivers/usbb \
  avr32/drivers/spi avr32/drivers/twi avr32/drivers/pwm avr32/drivers/adc avr32/drivers/wdt avr32/drivers/pdca \
  avr32/drivers/ssc/i2s avr32/drivers/cpu/cycle_counter avr32/utils avr32/utils/preprocessor common/services/delay \
  common/boards common/services/clock common/services/sleepmgr common/services/sleepmgr/uc3 common/services/usb \
  common/services/usb/class/hid \
  common/services/usb/class/hid/device common/services/usb/class/composite/device common/services/usb/udc common/utils

CFLAGS := -mpart=uc3a1256 -Os -fdata-sections -ffunction-sections -muse-rodata-section -masm-addr-pseudos \
  -Wall -std=gnu99 -D BOARD=USER_BOARD -D UDD_ENABLE $(EXTRA_CFLAGS) \
  -Isrc -Iconfig -Idfp-include/AT32UC3A1256 $(addprefix -I$(ASF)/,$(INCS))

ASFOBJS := $(addprefix $(OBJ)/,$(notdir $(ASSRCS:.S=.o) $(CSRCS:.c=.o)))
APPOBJS := $(patsubst src/%.c,$(OBJ)/app_%.o,$(APPSRCS))

vpath %.S $(sort $(dir $(addprefix $(ASF)/,$(ASSRCS))))
vpath %.c $(sort $(dir $(addprefix $(ASF)/,$(CSRCS))))

all: $(B)/$(OUT).hex $(B)/$(OUT).bin
	$(SIZE) $(B)/$(OUT).elf

$(B)/$(OUT).elf: $(ASFOBJS) $(APPOBJS) link_zd1.lds
	$(CC) $(CFLAGS) -nostartfiles -T link_zd1.lds -Wl,-e,_start -Wl,--gc-sections -Wl,-Map=$(B)/$(OUT).map \
	  $(ASFOBJS) $(APPOBJS) -o $@
$(B)/$(OUT).hex: $(B)/$(OUT).elf
	$(OBJCOPY) -O ihex $< $@
$(B)/$(OUT).bin: $(B)/$(OUT).elf
	$(OBJCOPY) -O binary $< $@

$(OBJ)/app_%.o: src/%.c | $(OBJ)
	$(CC) $(CFLAGS) -c $< -o $@
$(OBJ)/%.o: %.c | $(OBJ)
	$(CC) $(CFLAGS) -c $< -o $@
$(OBJ)/%.o: %.S | $(OBJ)
	$(CC) $(CFLAGS) -c $< -o $@
$(OBJ):
	mkdir -p $@

.PHONY: all
