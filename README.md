# veilid_duplex 

Full-duplex asyncronous communication between two peers with [Veilid](https://gitlab.com/veilid/veilid).

How this works: a peer publishes his route to dht and updates the record when route changes. Another peer sends a app_message with his own DHT record. Then both peers hold a route to each other.

## Current Issues

```
thread 'tokio-runtime-worker' panicked at 'join error was not a panic, should not poll after abort', /home/cwiz/.cargo/git/checkouts/veilid-88b7e7557f46c329/ff56634/veilid-tools/src/must_join_handle.rs:93:37
```

```
2023-10-10T12:12:26.887523Z ERROR rtab: Test route failed: can't make private routes until our node info is valid: PeerInfo { node_ids: CryptoTypedGroup { items: [CryptoTyped { kind: VLD0, value: CryptoKey(OSt9_wT8p0PHmCmiOzlnKjUeOTSEajhb5aTUDrR3aao) }] }, signed_node_info: Direct(SignedDirectNodeInfo { node_info: NodeInfo { network_class: Invalid, outbound_protocols: EnumSet(UDP | TCP), address_types: EnumSet(IPV4), envelope_support: [0], crypto_support: [VLD0], capabilities: [ROUT, SGNL, RLAY, DIAL, DHTV, APPM], dial_info_detail_list: [] }, timestamp: 1696939946880721, signatures: [CryptoTyped { kind: VLD0, value: Signature(i0EEcWmRK5flAaFapDaU9xd12QA964Vcn8IbjDcWYI8u7oyGXhLuP3reHP3IpjZZ2ia9EKSpHx6Cev3wWdQoCg) }] }) }
```

## Examples

### Pingpong

2 peers increment the counter and pass it to each other.

Host: 
```bash
cargo run --example pingpong -- --server
```

Client: 
```bash
cargo run --example pingpong -- --client "VLD0:MDoZwLsoQgM6-XKE3giy-8r53e4yCod5Y546laT0El0"
```