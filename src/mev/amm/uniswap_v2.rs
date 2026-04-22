use ethers::types::{Address, U256};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct V2PoolState {
    pub pair: Address,
    pub token0: Address,
    pub token1: Address,
    pub reserve0: U256,
    pub reserve1: U256,
    pub fee_bps: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct V2SwapResult {
    pub amount_in: U256,
    pub amount_out: U256,
    pub new_reserve_in: U256,
    pub new_reserve_out: U256,
    pub price_impact_bps: u64,
}

impl V2PoolState {
    pub fn reserves_for(&self, token_in: Address, token_out: Address) -> Option<(U256, U256)> {
        if token_in == self.token0 && token_out == self.token1 {
            Some((self.reserve0, self.reserve1))
        } else if token_in == self.token1 && token_out == self.token0 {
            Some((self.reserve1, self.reserve0))
        } else {
            None
        }
    }

    pub fn apply_swap_exact_in(
        &self,
        token_in: Address,
        token_out: Address,
        amount_in: U256,
    ) -> Option<(Self, V2SwapResult)> {
        let (reserve_in, reserve_out) = self.reserves_for(token_in, token_out)?;
        if amount_in.is_zero() || reserve_in.is_zero() || reserve_out.is_zero() {
            return None;
        }

        let amount_out = amount_out_exact_in(amount_in, reserve_in, reserve_out, self.fee_bps)?;
        if amount_out.is_zero() || amount_out >= reserve_out {
            return None;
        }

        let new_reserve_in = reserve_in.saturating_add(amount_in);
        let new_reserve_out = reserve_out.saturating_sub(amount_out);
        let price_impact_bps = price_impact_bps(amount_in, amount_out, reserve_in, reserve_out);

        let mut next = *self;
        if token_in == self.token0 {
            next.reserve0 = new_reserve_in;
            next.reserve1 = new_reserve_out;
        } else {
            next.reserve1 = new_reserve_in;
            next.reserve0 = new_reserve_out;
        }

        Some((
            next,
            V2SwapResult {
                amount_in,
                amount_out,
                new_reserve_in,
                new_reserve_out,
                price_impact_bps,
            },
        ))
    }
}

pub fn amount_out_exact_in(
    amount_in: U256,
    reserve_in: U256,
    reserve_out: U256,
    fee_bps: u64,
) -> Option<U256> {
    if amount_in.is_zero() || reserve_in.is_zero() || reserve_out.is_zero() || fee_bps >= 10_000 {
        return None;
    }

    let fee_denominator = U256::from(10_000u64);
    let amount_in_with_fee = amount_in.saturating_mul(U256::from(10_000u64 - fee_bps));
    let numerator = amount_in_with_fee.saturating_mul(reserve_out);
    let denominator = reserve_in
        .saturating_mul(fee_denominator)
        .saturating_add(amount_in_with_fee);
    if denominator.is_zero() {
        return None;
    }
    Some(numerator / denominator)
}

pub fn amount_in_for_exact_out(
    amount_out: U256,
    reserve_in: U256,
    reserve_out: U256,
    fee_bps: u64,
) -> Option<U256> {
    if amount_out.is_zero()
        || reserve_in.is_zero()
        || reserve_out.is_zero()
        || amount_out >= reserve_out
        || fee_bps >= 10_000
    {
        return None;
    }

    let numerator = reserve_in
        .saturating_mul(amount_out)
        .saturating_mul(U256::from(10_000u64));
    let denominator = reserve_out
        .saturating_sub(amount_out)
        .saturating_mul(U256::from(10_000u64 - fee_bps));
    if denominator.is_zero() {
        return None;
    }
    Some((numerator / denominator).saturating_add(U256::one()))
}

pub fn price_impact_bps(
    amount_in: U256,
    amount_out: U256,
    reserve_in: U256,
    reserve_out: U256,
) -> u64 {
    if amount_in.is_zero() || amount_out.is_zero() || reserve_in.is_zero() || reserve_out.is_zero()
    {
        return 10_000;
    }

    let ideal_out = amount_in.saturating_mul(reserve_out) / reserve_in;
    if ideal_out.is_zero() || ideal_out <= amount_out {
        return 0;
    }

    let impact = ideal_out.saturating_sub(amount_out);
    (impact.saturating_mul(U256::from(10_000u64)) / ideal_out)
        .min(U256::from(10_000u64))
        .as_u64()
}

pub fn find_roi_optimal_input(
    reserve_in: U256,
    reserve_out: U256,
    capital_cap: U256,
    gas_cost_wei: U256,
    fee_bps: u64,
) -> Option<(U256, U256)> {
    if capital_cap.is_zero() || reserve_in.is_zero() || reserve_out.is_zero() {
        return None;
    }

    let candidates = [
        50u64, 100, 200, 350, 500, 750, 1_000, 1_500, 2_000, 2_500, 3_000,
    ];
    let mut best: Option<(U256, U256, U256)> = None;

    for bps in candidates {
        let amount_in = capital_cap.saturating_mul(U256::from(bps)) / U256::from(10_000u64);
        if amount_in.is_zero() {
            continue;
        }
        let amount_out = amount_out_exact_in(amount_in, reserve_in, reserve_out, fee_bps)?;
        let gross = amount_out.saturating_sub(amount_in);
        let net = gross.saturating_sub(gas_cost_wei);
        if net.is_zero() {
            continue;
        }
        let roi_key = net.saturating_mul(U256::from(1_000_000u64)) / amount_in;
        if best
            .as_ref()
            .map(|(_, _, best_roi)| roi_key > *best_roi)
            .unwrap_or(true)
        {
            best = Some((amount_in, net, roi_key));
        }
    }

    best.map(|(amount_in, net, _)| (amount_in, net))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn v2_exact_in_respects_constant_product_fee() {
        let out = amount_out_exact_in(
            U256::from(1_000u64),
            U256::from(10_000u64),
            U256::from(10_000u64),
            30,
        )
        .unwrap();
        assert_eq!(out, U256::from(906u64));
    }

    #[test]
    fn v2_state_moves_reserves_after_swap() {
        let pool = V2PoolState {
            pair: Address::zero(),
            token0: Address::from_low_u64_be(1),
            token1: Address::from_low_u64_be(2),
            reserve0: U256::from(10_000u64),
            reserve1: U256::from(10_000u64),
            fee_bps: 30,
        };
        let (next, result) = pool
            .apply_swap_exact_in(pool.token0, pool.token1, U256::from(1_000u64))
            .unwrap();
        assert_eq!(result.amount_out, U256::from(906u64));
        assert_eq!(next.reserve0, U256::from(11_000u64));
        assert_eq!(next.reserve1, U256::from(9_094u64));
    }
}
