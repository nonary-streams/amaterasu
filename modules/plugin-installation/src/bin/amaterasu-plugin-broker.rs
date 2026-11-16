fn main() {
    let result = (|| {
        let policy = std::env::args()
            .nth(1)
            .ok_or("usage: amaterasu-plugin-broker POLICY")?;
        let policy =
            amaterasu_plugin_installation::broker::read_policy(std::path::Path::new(&policy))?;
        amaterasu_plugin_installation::broker::serve(
            policy,
            std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
        )
    })();
    if let Err(error) = result {
        eprintln!("{error}");
        std::process::exit(1);
    }
}
