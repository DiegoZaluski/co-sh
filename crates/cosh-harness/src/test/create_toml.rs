use crate::namespace_cache::NamespaceCache;

#[test]
fn create_toml() {
    let path = "/tmp/cosh/cosh-cache-test.toml";

    let mut cache = NamespaceCache::new("my-server", "filesystem", "test", "File system tools")
        .with_cache_file(path);

    let toml = cache.build().unwrap();
    cache.set_cache(&toml).unwrap();

    let content = std::fs::read_to_string(path).unwrap();
    println!("{content}")
}
