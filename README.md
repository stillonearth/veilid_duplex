# veilid_duplex 

Full-duplex asyncronous communication between two peers with [Veilid](https://gitlab.com/veilid/veilid).

- Alice publishes her route to DHT and sends DHT key to Bob. Alice will update here route on DHT when connection breaks;
- Bob does the same, and sends his DHT key to Alice over Veilid channel.
- When Alice or Bob fail to send a message they try getting a new route from DHT. They also update their DHT records when their routes die.
- Sometimes a message will be delivered twice, so Alice and Bob keep a record of all hashed messages they got.

Veilid duplex manages veilid internals for you, such as allocating routes and recovering from route shutdowns.

## Current Issues

Veilid crushing

```bash
thread 'tokio-runtime-worker' panicked at /home/cwiz/.cargo/git/checkouts/veilid-88b7e7557f46c329/cc5cb8a/veilid-tools/src/must_join_handle.rs:67:13:
MustJoinHandle was not completed upon drop. Add cooperative cancellation where appropriate to ensure this is completed before drop.
note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace
Error: new_custom_private_route
```

## Usage

See [pingpong](examples/pingpong.rs) example.

## Examples

### Pingpong

2 peers increment the counter and pass it to each other.

Host: 
```bash
cargo run --example pingpong -- --server --verbose
```

This will print host's DHT key

Client: 
```bash
cargo run --example pingpong --  --verbose --client "VLD0:MDoZwLsoQgM6-XKE3giy-8r53e4yCod5Y546laT0El0"
```