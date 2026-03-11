# Testing

## Unit Tests

```bash
cargo test
```

## Integration Tests

### Cashu (testnut FakeWallet mint, auto-pays invoices)

```bash
cargo test --features cashu -- --ignored cashu_live
```

### SOL devnet (no funds needed)

```bash
cargo test --features sol -- --ignored sol_live
```

### EVM testnet (no funds needed)

```bash
EVM_TEST_RPC="https://sepolia.base.org" cargo test --features evm -- --ignored evm_live
```

### BTC signet (no funds needed)

```bash
cargo test --features btc-esplora -- --ignored btc_live
```

### BTC regression tests (offline, no live chain dependency)

```bash
cargo test --no-default-features --features btc-esplora,redb provider::btc::tests::
cargo test --no-default-features --features btc-esplora,redb store::transaction::tests::update_tx_status
cargo test --no-default-features --features btc-esplora,redb --test btc_receive_wait
```

### REST API (no external dependencies)

```bash
cargo test --no-default-features --features redb,rest,exchange-rate --test rest_test
```

### Send tests (require funded wallet)

```bash
SOL_TEST_MNEMONIC="word1 ... word12" cargo test --features sol -- --ignored sol_live_send
EVM_TEST_MNEMONIC="word1 ... word12" cargo test --features evm -- --ignored evm_live_send
BTC_TEST_MNEMONIC="word1 ... word12" cargo test --features btc-esplora -- --ignored btc_live_send
```
