# ngrok-rs &emsp; ![Build] ![Crate]

[build]: https://github.com/nkconnor/ngrok/workflows/Rust/badge.svg
[crate]: https://img.shields.io/crates/v/ngrok

A minimal [ngrok](https://ngrok.com/) wrapper. Only tested on Linux,
assuming it does not work with Windows (contributions welcome!).

## Getting Started

```toml
[dependencies]
ngrok = "0.1.1"
```

## Usage

```rust
use ngrok;

fn main() {
    let ngrok = ngrok::builder()
        .http()
        .port(3030)
        .run()
        .unwrap();


    let callback = ngrok.tunnel().http().unwrap();

    println!("Tunnel is open at {:?}", callback);
}
```

## License

Licensed under either of <a href="LICENSE-APACHE">Apache License, Version
2.0</a> or <a href="LICENSE-MIT">MIT license</a> at your option.

Unless you explicitly state otherwise, any contribution intentionally submitted
for inclusion in Ent by you, as defined in the Apache-2.0 license, shall be
dual licensed as above, without any additional terms or conditions.
