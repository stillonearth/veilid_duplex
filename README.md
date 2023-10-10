# veilid_duplex 

Full-duplex asyncronous communication between two peers with [Veilid](https://gitlab.com/veilid/veilid).

How this works: a peer publishes his route to dht and updates the record when route changes. Another peer sends a app_message with his own DHT record. Then both peers hold a route to each other.

## Examples

### Pingpong

2 peers increment the counter and pass it to each other.

Host: `cargo run --example pingpong -- --server`
Client: `cargo run --example pingpong -- --client "VLD0:MDoZwLsoQgM6-XKE3giy-8r53e4yCod5Y546laT0El0"`