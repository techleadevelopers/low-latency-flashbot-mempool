use ethers::types::{Address, I256, U256};

pub const Q96_F64: f64 = 79_228_162_514_264_337_593_543_950_336.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct V3Tick {
    pub index: i32,
    pub liquidity_net: I256,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct V3PoolState {
    pub pool: Address,
    pub token0: Address,
    pub token1: Address,
    pub sqrt_price_x96: U256,
    pub liquidity: U256,
    pub current_tick: i32,
    pub fee_bps: u64,
    pub initialized_ticks: Vec<V3Tick>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct V3SwapResult {
    pub amount_in: U256,
    pub amount_out: U256,
    pub sqrt_price_x96_after: U256,
    pub tick_after: i32,
    pub price_impact_bps: u64,
}

impl V3PoolState {
    pub fn simulate_exact_in(
        &self,
        token_in: Address,
        token_out: Address,
        amount_in: U256,
    ) -> Option<(Self, V3SwapResult)> {
        if amount_in.is_zero() || self.liquidity.is_zero() {
            return None;
        }
        let zero_for_one = token_in == self.token0 && token_out == self.token1;
        let one_for_zero = token_in == self.token1 && token_out == self.token0;
        if !zero_for_one && !one_for_zero {
            return None;
        }

        let amount_less_fee = amount_in
            .saturating_mul(U256::from(10_000u64.saturating_sub(self.fee_bps)))
            / U256::from(10_000u64);
        let sqrt_before = u256_to_f64(self.sqrt_price_x96)? / Q96_F64;
        let liquidity = u256_to_f64(self.liquidity)?;
        let amount = u256_to_f64(amount_less_fee)?;
        if sqrt_before <= 0.0 || liquidity <= 0.0 {
            return None;
        }

        let (sqrt_after, amount_out_f64) = if zero_for_one {
            let sqrt_after = liquidity * sqrt_before / (liquidity + amount * sqrt_before);
            let out = liquidity * (sqrt_before - sqrt_after);
            (sqrt_after, out)
        } else {
            let sqrt_after = sqrt_before + amount / liquidity;
            let out = liquidity * (1.0 / sqrt_before - 1.0 / sqrt_after);
            (sqrt_after, out)
        };

        if !sqrt_after.is_finite() || !amount_out_f64.is_finite() || amount_out_f64 <= 0.0 {
            return None;
        }

        let sqrt_price_x96_after = f64_to_u256(sqrt_after * Q96_F64)?;
        let tick_after = sqrt_price_to_tick(sqrt_after);
        let price_impact_bps = price_impact_bps(sqrt_before, sqrt_after);
        let amount_out = f64_to_u256(amount_out_f64)?;
        let mut next = self.clone();
        next.sqrt_price_x96 = sqrt_price_x96_after;
        next.current_tick = tick_after;
        next.liquidity = apply_crossed_tick_liquidity(
            self.liquidity,
            self.current_tick,
            tick_after,
            &self.initialized_ticks,
        );

        Some((
            next,
            V3SwapResult {
                amount_in,
                amount_out,
                sqrt_price_x96_after,
                tick_after,
                price_impact_bps,
            },
        ))
    }
}

fn apply_crossed_tick_liquidity(
    mut liquidity: U256,
    before: i32,
    after: i32,
    ticks: &[V3Tick],
) -> U256 {
    if before == after {
        return liquidity;
    }
    let ascending = after > before;
    for tick in ticks {
        let crossed = if ascending {
            tick.index > before && tick.index <= after
        } else {
            tick.index <= before && tick.index > after
        };
        if !crossed {
            continue;
        }
        if tick.liquidity_net >= I256::zero() {
            liquidity = liquidity.saturating_add(tick.liquidity_net.into_raw());
        } else {
            liquidity = liquidity.saturating_sub((-tick.liquidity_net).into_raw());
        }
    }
    liquidity
}

fn sqrt_price_to_tick(sqrt_price: f64) -> i32 {
    let price = sqrt_price * sqrt_price;
    (price.ln() / 1.0001_f64.ln()).floor() as i32
}

fn price_impact_bps(sqrt_before: f64, sqrt_after: f64) -> u64 {
    let price_before = sqrt_before * sqrt_before;
    let price_after = sqrt_after * sqrt_after;
    if price_before <= 0.0 {
        return 10_000;
    }
    (((price_after - price_before).abs() / price_before) * 10_000.0)
        .min(10_000.0)
        .max(0.0) as u64
}

fn u256_to_f64(value: U256) -> Option<f64> {
    value.to_string().parse::<f64>().ok()
}

fn f64_to_u256(value: f64) -> Option<U256> {
    if !value.is_finite() || value < 0.0 {
        return None;
    }
    U256::from_dec_str(&format!("{value:.0}")).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn v3_exact_in_moves_sqrt_price() {
        let pool = V3PoolState {
            pool: Address::zero(),
            token0: Address::from_low_u64_be(1),
            token1: Address::from_low_u64_be(2),
            sqrt_price_x96: U256::from_dec_str("79228162514264337593543950336").unwrap(),
            liquidity: U256::from(1_000_000u64),
            current_tick: 0,
            fee_bps: 30,
            initialized_ticks: Vec::new(),
        };
        let (next, result) = pool
            .simulate_exact_in(pool.token0, pool.token1, U256::from(1_000u64))
            .unwrap();
        assert!(result.amount_out > U256::zero());
        assert!(next.sqrt_price_x96 < pool.sqrt_price_x96);
    }
}
