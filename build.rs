fn main() {
    pkg_config::probe_library("fontconfig").unwrap();
}
