use super::super::namespace_cache::NamespaceCache;

#[test]
fn create_toml() {
    let path = "/tmp/cosh/cosh-cache-test.toml";
    let _ = std::fs::create_dir_all("/tmp/cosh");

    let mut cache = NamespaceCache::new().with_cache_file(path);
    cache.set_context("my-server", "filesystem", "test");
    cache.mark_modified("File system tools".into());
    cache.flush().unwrap();

    let content = std::fs::read_to_string(path).unwrap();
    println!("{content}")
}
