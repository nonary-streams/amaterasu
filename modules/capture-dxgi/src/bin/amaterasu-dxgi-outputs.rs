fn main() {
    match amaterasu_capture_dxgi::enumerate_local_outputs() {
        Ok(outputs) => println!("{outputs}"),
        Err(error) => {
            eprintln!("DXGI enumeration failed: {error}");
            std::process::exit(1);
        }
    }
}
