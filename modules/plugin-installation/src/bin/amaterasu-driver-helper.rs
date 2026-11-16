fn main() {
    if let Err(error) = amaterasu_plugin_installation::helper::main() {
        eprintln!("{error}");
        std::process::exit(1);
    }
}
