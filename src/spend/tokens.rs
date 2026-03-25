#![cfg_attr(not(any(feature = "sol", feature = "evm")), allow(dead_code))]

//! Shared token registry for well-known tokens across chains.
//!
//! Maps (chain, symbol) -> (contract/mint address, decimals).
//! Used by both EVM and SOL providers to resolve `--token usdc` style flags.

pub struct KnownToken {
    pub symbol: &'static str,
    pub address: &'static str,
    pub decimals: u8,
}

// ═══════════════════════════════════════════
// EVM tokens
// ═══════════════════════════════════════════

const EVM_TOKENS_BASE: &[KnownToken] = &[
    KnownToken {
        symbol: "usdc",
        address: "0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913",
        decimals: 6,
    },
    KnownToken {
        symbol: "usdt",
        address: "0xfde4C96c8593536E31F229EA8f37b2ADa2699bb2",
        decimals: 6,
    },
];

const EVM_TOKENS_BASE_SEPOLIA: &[KnownToken] = &[KnownToken {
    symbol: "usdc",
    address: "0x036CbD53842c5426634e7929541eC2318f3dCF7e",
    decimals: 6,
}];

const EVM_TOKENS_ARBITRUM: &[KnownToken] = &[
    KnownToken {
        symbol: "usdc",
        address: "0xaf88d065e77c8cC2239327C5EDb3A432268e5831",
        decimals: 6,
    },
    KnownToken {
        symbol: "usdt",
        address: "0xFd086bC7CD5C481DCC9C85ebE478A1C0b69FCbb9",
        decimals: 6,
    },
];

const EVM_TOKENS_ARBITRUM_SEPOLIA: &[KnownToken] = &[KnownToken {
    symbol: "usdc",
    address: "0x75faf114eafb1BDbe2F0316DF893fd58CE46AA4d",
    decimals: 6,
}];

const EVM_TOKENS_ETHEREUM: &[KnownToken] = &[
    KnownToken {
        symbol: "usdc",
        address: "0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48",
        decimals: 6,
    },
    KnownToken {
        symbol: "usdt",
        address: "0xdAC17F958D2ee523a2206206994597C13D831ec7",
        decimals: 6,
    },
];

pub fn evm_known_tokens(chain_id: u64) -> &'static [KnownToken] {
    match chain_id {
        8453 => EVM_TOKENS_BASE,
        84532 => EVM_TOKENS_BASE_SEPOLIA,
        42161 => EVM_TOKENS_ARBITRUM,
        421614 => EVM_TOKENS_ARBITRUM_SEPOLIA,
        1 => EVM_TOKENS_ETHEREUM,
        _ => &[],
    }
}

/// Resolve an EVM token symbol (case-insensitive) to its known token info.
pub fn resolve_evm_token(chain_id: u64, symbol: &str) -> Option<&'static KnownToken> {
    let lower = symbol.to_ascii_lowercase();
    evm_known_tokens(chain_id)
        .iter()
        .find(|t| t.symbol == lower)
}

// ═══════════════════════════════════════════
// SOL tokens
// ═══════════════════════════════════════════

const SOL_TOKENS_MAINNET: &[KnownToken] = &[
    KnownToken {
        symbol: "usdc",
        address: "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v",
        decimals: 6,
    },
    KnownToken {
        symbol: "usdt",
        address: "Es9vMFrzaCERmJfrF4H2FYD4KCoNkY11McCe8BenwNYB",
        decimals: 6,
    },
];

const SOL_TOKENS_DEVNET: &[KnownToken] = &[KnownToken {
    symbol: "usdc",
    address: "4zMMC9srt5Ri5X14GAgXhaHii3GnPAEERYPJgZJDncDU",
    decimals: 6,
}];

pub fn sol_known_tokens(cluster: &str) -> &'static [KnownToken] {
    match cluster {
        "mainnet-beta" | "mainnet" => SOL_TOKENS_MAINNET,
        "devnet" => SOL_TOKENS_DEVNET,
        _ => SOL_TOKENS_MAINNET, // default to mainnet
    }
}

/// Resolve a SOL token symbol (case-insensitive) to its known token info.
pub fn resolve_sol_token(cluster: &str, symbol: &str) -> Option<&'static KnownToken> {
    let lower = symbol.to_ascii_lowercase();
    sol_known_tokens(cluster).iter().find(|t| t.symbol == lower)
}

/// Detect cluster name from an RPC endpoint URL.
pub fn sol_cluster_from_endpoint(endpoint: &str) -> &'static str {
    if endpoint.contains("devnet") {
        "devnet"
    } else if endpoint.contains("testnet") {
        "testnet"
    } else {
        "mainnet-beta"
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn resolve_evm_usdc_base() {
        let token = resolve_evm_token(8453, "usdc");
        assert!(token.is_some());
        let t = token.unwrap();
        assert_eq!(t.decimals, 6);
        assert!(t.address.starts_with("0x"));
    }

    #[test]
    fn resolve_evm_usdc_case_insensitive() {
        assert!(resolve_evm_token(8453, "USDC").is_some());
        assert!(resolve_evm_token(8453, "Usdc").is_some());
    }

    #[test]
    fn resolve_evm_unknown_chain() {
        assert!(resolve_evm_token(999999, "usdc").is_none());
    }

    #[test]
    fn resolve_evm_unknown_token() {
        assert!(resolve_evm_token(8453, "doge").is_none());
    }

    #[test]
    fn resolve_sol_usdc_mainnet() {
        let token = resolve_sol_token("mainnet-beta", "usdc");
        assert!(token.is_some());
        assert_eq!(
            token.unwrap().address,
            "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v"
        );
    }

    #[test]
    fn resolve_sol_usdc_devnet() {
        let token = resolve_sol_token("devnet", "usdc");
        assert!(token.is_some());
    }

    #[test]
    fn sol_cluster_detection() {
        assert_eq!(
            sol_cluster_from_endpoint("https://api.devnet.solana.com"),
            "devnet"
        );
        assert_eq!(
            sol_cluster_from_endpoint("https://api.mainnet-beta.solana.com"),
            "mainnet-beta"
        );
        assert_eq!(
            sol_cluster_from_endpoint("https://rpc.helius.xyz"),
            "mainnet-beta"
        );
    }

    #[test]
    fn evm_usdt_addresses() {
        assert!(resolve_evm_token(1, "usdt").is_some());
        assert!(resolve_evm_token(8453, "usdt").is_some());
        assert!(resolve_evm_token(42161, "usdt").is_some());
    }
}
