fn main() {
    #[cfg(feature = "embedded-observatory")]
    {
        let dist = std::path::Path::new("../../observatory/dist/index.html");
        if !dist.exists() {
            println!(
                "cargo:warning=Observatory dist/ not found. \
                 Build with --no-default-features to skip embedding, \
                 or run 'npm run build' in observatory/ first."
            );
        }
    }
}
