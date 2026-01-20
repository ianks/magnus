fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Declare Ruby 4.1 cfg flags
    println!("cargo::rustc-check-cfg=cfg(ruby_lt_4_1)");
    println!("cargo::rustc-check-cfg=cfg(ruby_gte_4_1)");

    let _ = rb_sys_env::activate()?;

    Ok(())
}
