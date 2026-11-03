// Linux 2.6.24 compatibility: the calls zd1-compat replaces go through its shims. Here, on the one binary, rather than
// as the target's rustflags: those reach every link, crane's dependency build too, which has no zd1-compat to link.
fn main() {
    for f in ["syscall", "epoll_create1", "eventfd"] {
        println!("cargo:rustc-link-arg-bins=-Wl,--wrap={f}");
    }
}
