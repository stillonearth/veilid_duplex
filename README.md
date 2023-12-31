# veilid_duplex 

Full-duplex asyncronous communication between two peers with [Veilid](https://gitlab.com/veilid/veilid).

Alice publishes her route to DHT and sends DHT key to Bob. Alice will update here route on DHT when connection breaks;
Bob will do the same, and will send his DHT key to alice over Veilid channel.
When Alice or Bob fail to send a message they will try getting a new route from DHT. They will also update their DHT records when routes break.
Sometimes a message will be delivered twice, so Alice and Bob keep a record of all hashed of messages they got.

## Current Issues

Veilid crushing

```bash
thread 'tokio-runtime-worker' panicked at /home/cwiz/.cargo/git/checkouts/veilid-88b7e7557f46c329/cc5cb8a/veilid-tools/src/must_join_handle.rs:67:13:
MustJoinHandle was not completed upon drop. Add cooperative cancellation where appropriate to ensure this is completed before drop.
note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace
Error: new_custom_private_route
```

## Examples

### Pingpong

2 peers increment the counter and pass it to each other.

Host: 
```bash
cargo run --example pingpong -- --server
```

This will print host's DHT key

Client: 
```bash
cargo run --example pingpong -- --client "VLD0:MDoZwLsoQgM6-XKE3giy-8r53e4yCod5Y546laT0El0"
```