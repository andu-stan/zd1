//! A minimal module image for trying install paths: console + MCU link + HCR 0x21 into BL2, and a build tag
//! (ZD1_TAG) so two flashed builds can be told apart. Entered at 0x60008000 by the preloader (mini.ld).
#![no_std]
#![no_main]

zd1_boot::entry!(main);

fn main() -> ! {
    let mut link = zd1_boot::link_up();
    zd1_boot::say(&mut link, "\n[zd1-os] minimal module image up on ARM926EJ-S, board = dm870, image ");
    zd1_boot::say(&mut link, option_env!("ZD1_TAG").unwrap_or("dev"));
    zd1_boot::say(&mut link, "\n[zd1-os] link up\n");
    zd1_boot::serve_until_bl2(&mut link, "[zd1-os]")
}
