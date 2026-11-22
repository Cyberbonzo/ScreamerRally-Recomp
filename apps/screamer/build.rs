fn main() {
    // A large stack reserve keeps Windows from placing the main thread's stack in the bottom
    // megabyte, so the launcher can reserve real conventional memory for the game there.
    println!("cargo:rustc-link-arg-bins=/STACK:33554432");
}
