use ethers::types::U256;

#[derive(Debug, Clone, Copy)]
pub struct ReplacementDecision {
    pub replace: bool,
    pub new_gas_price_wei: U256,
    pub reason: &'static str,
}

#[derive(Debug, Clone, Copy)]
pub struct ReplacementPolicy {
    pub bump_bps: u64,
    pub max_attempts: u8,
    pub max_gas_price_wei: U256,
}

impl ReplacementPolicy {
    pub fn conservative(max_gas_price_wei: U256) -> Self {
        Self {
            bump_bps: 1_250,
            max_attempts: 2,
            max_gas_price_wei,
        }
    }

    #[inline(always)]
    pub fn decide(
        self,
        current_gas_price_wei: U256,
        attempts: u8,
        still_profitable: bool,
    ) -> ReplacementDecision {
        if !still_profitable {
            return ReplacementDecision {
                replace: false,
                new_gas_price_wei: current_gas_price_wei,
                reason: "profit invalidated",
            };
        }
        if attempts >= self.max_attempts {
            return ReplacementDecision {
                replace: false,
                new_gas_price_wei: current_gas_price_wei,
                reason: "replacement attempts exhausted",
            };
        }
        let bumped = current_gas_price_wei.saturating_mul(U256::from(10_000u64 + self.bump_bps))
            / U256::from(10_000u64);
        if bumped > self.max_gas_price_wei {
            return ReplacementDecision {
                replace: false,
                new_gas_price_wei: current_gas_price_wei,
                reason: "replacement gas cap exceeded",
            };
        }
        ReplacementDecision {
            replace: true,
            new_gas_price_wei: bumped,
            reason: "fee bump allowed",
        }
    }
}
