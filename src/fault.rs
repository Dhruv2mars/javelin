pub fn hit(point: &str) {
    let enabled = std::env::var("JAVELIN_FAULT_POINT").unwrap_or_default();
    if enabled.split(',').any(|configured| configured == point) {
        eprintln!("injected fault at {point}");
        std::process::exit(86);
    }
}
