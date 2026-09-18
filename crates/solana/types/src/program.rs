use std::fmt;

use serde::{Deserialize, Serialize};

/// Known Solana DEX program IDs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum SolanaProgram {
    /// Raydium AMM V4
    RaydiumAmmV4,
    /// Raydium CLMM (Concentrated Liquidity)
    RaydiumClmm,
    /// Raydium CPMM (Constant Product)
    RaydiumCpmm,
    /// OpenBook V2
    OpenBookV2,
    /// Jupiter Aggregator V6
    JupiterV6,
    /// Pump.fun
    PumpFun,
    /// Custom/unknown program
    Other([u8; 32]),
}

impl SolanaProgram {
    pub fn from_program_id(id: &[u8; 32]) -> Self {
        match *id {
            RAYDIUM_AMM_V4 => Self::RaydiumAmmV4,
            RAYDIUM_CLMM => Self::RaydiumClmm,
            RAYDIUM_CPMM => Self::RaydiumCpmm,
            OPENBOOK_V2 => Self::OpenBookV2,
            JUPITER_V6 => Self::JupiterV6,
            PUMP_FUN => Self::PumpFun,
            other => Self::Other(other),
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::RaydiumAmmV4 => "raydium_amm_v4",
            Self::RaydiumClmm => "raydium_clmm",
            Self::RaydiumCpmm => "raydium_cpmm",
            Self::OpenBookV2 => "openbook_v2",
            Self::JupiterV6 => "jupiter_v6",
            Self::PumpFun => "pump_fun",
            Self::Other(_) => "unknown",
        }
    }
}

impl fmt::Display for SolanaProgram {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Other(bytes) => {
                write!(f, "0x")?;
                for b in &bytes[..8] {
                    write!(f, "{:02x}", b)?;
                }
                write!(f, "...")
            }
            _ => write!(f, "{}", self.as_str()),
        }
    }
}

// Known program ID constants (base58-decoded).
const RAYDIUM_AMM_V4: [u8; 32] = [
    6, 228, 177, 183, 246, 198, 99, 161, 179, 223, 145, 47, 103, 111, 4, 79, 102, 33, 201,
    122, 173, 43, 85, 145, 98, 181, 91, 246, 57, 144, 34, 156,
];
const RAYDIUM_CLMM: [u8; 32] = [
    228, 149, 148, 34, 191, 116, 84, 153, 197, 116, 38, 97, 184, 112, 196, 143, 127, 3, 247,
    45, 10, 171, 64, 82, 181, 81, 39, 241, 180, 63, 193, 79,
];
const RAYDIUM_CPMM: [u8; 32] = [
    172, 117, 206, 149, 216, 183, 88, 153, 122, 157, 113, 53, 109, 176, 165, 22, 148, 166,
    114, 15, 22, 193, 81, 122, 146, 250, 34, 228, 225, 61, 228, 219,
];
const OPENBOOK_V2: [u8; 32] = [
    11, 43, 186, 106, 193, 166, 74, 219, 150, 185, 148, 71, 19, 226, 182, 220, 84, 135, 197,
    198, 89, 179, 164, 79, 54, 43, 15, 185, 89, 90, 235, 167,
];
const JUPITER_V6: [u8; 32] = [
    136, 233, 11, 173, 88, 157, 163, 24, 157, 39, 238, 221, 226, 180, 13, 120, 219, 225, 184,
    109, 243, 112, 196, 139, 89, 156, 18, 170, 206, 15, 72, 17,
];
const PUMP_FUN: [u8; 32] = [
    6, 87, 22, 155, 84, 225, 208, 172, 47, 121, 113, 20, 139, 161, 118, 173, 190, 153, 154,
    237, 94, 136, 174, 113, 231, 180, 154, 244, 210, 120, 164, 162,
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_programs_parse() {
        let p = SolanaProgram::from_program_id(&RAYDIUM_AMM_V4);
        assert_eq!(p, SolanaProgram::RaydiumAmmV4);
        assert_eq!(p.as_str(), "raydium_amm_v4");
    }

    #[test]
    fn unknown_program_gets_other() {
        let id = [0xAB; 32];
        let p = SolanaProgram::from_program_id(&id);
        assert!(matches!(p, SolanaProgram::Other(_)));
    }

    #[test]
    fn display_known() {
        assert_eq!(SolanaProgram::RaydiumAmmV4.to_string(), "raydium_amm_v4");
    }
}
