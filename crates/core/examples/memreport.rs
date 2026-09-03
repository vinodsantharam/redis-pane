fn main() {
    use redis_pane_core::state::LoadedSet;
    for n in [100_000usize, 1_000_000] {
        let mut s = LoadedSet::default();
        for i in 0..n {
            s.push(format!("user:{i:08}:session").as_bytes());
        }
        println!(
            "{:>9} keys  {:>6.1} MB  {:>3} bytes/key",
            n,
            s.heap_bytes() as f64 / 1024.0 / 1024.0,
            s.heap_bytes() / s.len()
        );
    }
}
