# veilid_duplex 

Full-duplex asyncronous communication between two peers with [Veilid](https://gitlab.com/veilid/veilid).

- Alice publishes her route to DHT and sends DHT key to Bob. Alice will update here route on DHT when connection breaks;
- Bob does the same, and sends his DHT key to Alice over Veilid channel.
- When Alice or Bob fail to send a message they try getting a new route from DHT. They also update their DHT records when their routes die.
- Sometimes a message will be delivered twice, so Alice and Bob keep a record of all hashed messages they got.

Veilid duplex manages veilid internals for you, such as allocating routes and recovering from route shutdowns.

## Changlelog

- 0.1.5 WASM support

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