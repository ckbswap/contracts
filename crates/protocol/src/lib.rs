#![no_std]
extern crate alloc;
use alloc::{vec, vec::Vec};
mod wide {
    uint::construct_uint! { pub struct U256(4); }
}
pub use wide::U256;
pub type Hash = [u8; 32];
pub const ZERO: Hash = [0; 32];
pub const CKB: u64 = 100_000_000;
pub const STATE_CAPACITY: u64 = 500 * CKB;
pub const VAULT_CAPACITY: u64 = 200 * CKB;
pub const MINIMUM_LIQUIDITY: u128 = 1000;
pub const MAX_RESERVE: u128 = (1u128 << 112) - 1;
pub const ADD: u8 = 0;
pub const REMOVE: u8 = 1;
pub const SWAP_IN: u8 = 2;
pub const SWAP_OUT: u8 = 3;
pub const XUDT_CODE_HASH: Hash = [
    0x50, 0xbd, 0x8d, 0x66, 0x80, 0xb8, 0xb9, 0xcf, 0x98, 0xb7, 0x3f, 0x3c, 0x08, 0xfa, 0xf8, 0xb2,
    0xa2, 0x19, 0x14, 0x31, 0x19, 0x54, 0x11, 0x8a, 0xd6, 0x60, 0x9b, 0xe6, 0xe7, 0x8a, 0x1b, 0x95,
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(i8)]
pub enum Error {
    Encoding = 5,
    Arithmetic = 6,
    Amount = 7,
    Slippage = 8,
    Invariant = 9,
    Authorization = 10,
    Structure = 11,
    Capacity = 12,
    Token = 13,
    Genesis = 14,
}

pub type Result<T> = core::result::Result<T, Error>;

fn checked(n: U256) -> Result<u128> {
    if n > U256::from(u128::MAX) {
        Err(Error::Arithmetic)
    } else {
        Ok(n.low_u128())
    }
}

pub fn mul_div(a: u128, b: u128, c: u128) -> Result<u128> {
    if c == 0 {
        return Err(Error::Arithmetic);
    }
    checked(U256::from(a) * U256::from(b) / U256::from(c))
}

pub fn sqrt_product(a: u128, b: u128) -> u128 {
    let n = U256::from(a) * U256::from(b);
    if n.is_zero() {
        return 0;
    }
    let mut x = U256::one() << ((n.bits() + 1) / 2);
    loop {
        let y = (x + n / x) >> 1;
        if y >= x {
            return x.low_u128();
        }
        x = y;
    }
}

pub fn amount_out(input: u128, rin: u128, rout: u128) -> Result<u128> {
    if input == 0 || input > MAX_RESERVE || rin == 0 || rout == 0 {
        return Err(Error::Amount);
    }
    let fee = U256::from(input) * U256::from(997u64);
    let n = checked(fee * U256::from(rout) / (U256::from(rin) * U256::from(1000u64) + fee))?;
    if n == 0 || n >= rout {
        return Err(Error::Amount);
    }
    Ok(n)
}

pub fn amount_in(output: u128, rin: u128, rout: u128) -> Result<u128> {
    if output == 0 || output >= rout || rin == 0 {
        return Err(Error::Amount);
    }
    let n = checked(
        U256::from(rin) * U256::from(output) * U256::from(1000u64)
            / (U256::from(rout - output) * U256::from(997u64)),
    )?
    .checked_add(1)
    .ok_or(Error::Arithmetic)?;
    if n > MAX_RESERVE {
        return Err(Error::Amount);
    }
    Ok(n)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct State {
    pub nonce: u64,
    pub asset0: Hash,
    pub asset1: Hash,
    pub reserve0: u128,
    pub reserve1: u128,
    pub total_lp: u128,
}

impl State {
    pub fn new(asset0: Hash, asset1: Hash) -> Self {
        Self {
            nonce: 0,
            asset0,
            asset1,
            reserve0: 0,
            reserve1: 0,
            total_lp: 0,
        }
    }

    pub fn validate(&self) -> Result<()> {
        if self.asset0 >= self.asset1
            || self.reserve0 > MAX_RESERVE
            || self.reserve1 > MAX_RESERVE
            || self.total_lp > MAX_RESERVE
        {
            return Err(Error::Invariant);
        }
        if self.total_lp == 0 {
            if self.reserve0 != 0 || self.reserve1 != 0 {
                return Err(Error::Invariant);
            }
        } else if self.total_lp < MINIMUM_LIQUIDITY || self.reserve0 == 0 || self.reserve1 == 0 {
            return Err(Error::Invariant);
        }
        self.capacity()?;
        Ok(())
    }

    pub fn capacity(&self) -> Result<u64> {
        STATE_CAPACITY
            .checked_add(if self.asset0 == ZERO {
                u64::try_from(self.reserve0).map_err(|_| Error::Capacity)?
            } else {
                0
            })
            .ok_or(Error::Capacity)
    }

    pub fn apply(&self, a: &Action) -> Result<Transition> {
        self.validate()?;
        if a.side > 1 {
            return Err(Error::Encoding);
        }
        let mut t = Transition {
            next: self.clone(),
            in0: 0,
            in1: 0,
            out0: 0,
            out1: 0,
            lp_mint: 0,
            lp_burn: 0,
        };
        t.next.nonce = self.nonce.checked_add(1).ok_or(Error::Arithmetic)?;
        match a.op {
            ADD => {
                if a.side != 0
                    || a.amount0 == 0
                    || a.amount1 == 0
                    || a.amount0 > MAX_RESERVE
                    || a.amount1 > MAX_RESERVE
                {
                    return Err(Error::Amount);
                }
                if self.total_lp == 0 {
                    t.in0 = a.amount0;
                    t.in1 = a.amount1;
                    let root = sqrt_product(t.in0, t.in1);
                    if root <= MINIMUM_LIQUIDITY {
                        return Err(Error::Amount);
                    }
                    t.lp_mint = root - MINIMUM_LIQUIDITY;
                    t.next.total_lp = root;
                } else {
                    let optimal1 = mul_div(a.amount0, self.reserve1, self.reserve0)?;
                    if optimal1 <= a.amount1 {
                        t.in0 = a.amount0;
                        t.in1 = optimal1
                    } else {
                        t.in0 = mul_div(a.amount1, self.reserve0, self.reserve1)?;
                        t.in1 = a.amount1
                    }
                    if t.in0 == 0 || t.in1 == 0 {
                        return Err(Error::Amount);
                    }
                    t.lp_mint = mul_div(t.in0, self.total_lp, self.reserve0)?.min(mul_div(
                        t.in1,
                        self.total_lp,
                        self.reserve1,
                    )?);
                    t.next.total_lp = self
                        .total_lp
                        .checked_add(t.lp_mint)
                        .ok_or(Error::Arithmetic)?;
                }
                if t.lp_mint == 0 {
                    return Err(Error::Amount);
                }
                if t.in0 < a.min0 || t.in1 < a.min1 || t.lp_mint < a.min_lp {
                    return Err(Error::Slippage);
                }
                t.next.reserve0 = self.reserve0.checked_add(t.in0).ok_or(Error::Arithmetic)?;
                t.next.reserve1 = self.reserve1.checked_add(t.in1).ok_or(Error::Arithmetic)?;
            }
            REMOVE => {
                if a.side != 0
                    || a.amount1 != 0
                    || a.min_lp != 0
                    || a.amount0 == 0
                    || self.total_lp == 0
                    || a.amount0 > self.total_lp - MINIMUM_LIQUIDITY
                {
                    return Err(Error::Amount);
                }
                t.lp_burn = a.amount0;
                t.out0 = mul_div(a.amount0, self.reserve0, self.total_lp)?;
                t.out1 = mul_div(a.amount0, self.reserve1, self.total_lp)?;
                if t.out0 == 0 || t.out1 == 0 {
                    return Err(Error::Amount);
                }
                if t.out0 < a.min0 || t.out1 < a.min1 {
                    return Err(Error::Slippage);
                }
                t.next.reserve0 -= t.out0;
                t.next.reserve1 -= t.out1;
                t.next.total_lp -= a.amount0;
            }
            SWAP_IN | SWAP_OUT => {
                if a.min_lp != 0 || a.min1 != 0 {
                    return Err(Error::Encoding);
                }
                let (rin, rout) = if a.side == 0 {
                    (self.reserve0, self.reserve1)
                } else {
                    (self.reserve1, self.reserve0)
                };
                let (input, output) = if a.op == SWAP_IN {
                    if a.amount1 != 0 {
                        return Err(Error::Encoding);
                    }
                    let out = amount_out(a.amount0, rin, rout)?;
                    if out < a.min0 {
                        return Err(Error::Slippage);
                    }
                    (a.amount0, out)
                } else {
                    if a.min0 != 0 {
                        return Err(Error::Encoding);
                    }
                    let input = amount_in(a.amount0, rin, rout)?;
                    if input > a.amount1 {
                        return Err(Error::Slippage);
                    }
                    (input, a.amount0)
                };
                if a.side == 0 {
                    t.in0 = input;
                    t.out1 = output;
                    t.next.reserve0 = self.reserve0.checked_add(input).ok_or(Error::Arithmetic)?;
                    t.next.reserve1 -= output
                } else {
                    t.in1 = input;
                    t.out0 = output;
                    t.next.reserve1 = self.reserve1.checked_add(input).ok_or(Error::Arithmetic)?;
                    t.next.reserve0 -= output
                }
                let b0 = U256::from(t.next.reserve0) * U256::from(1000u64)
                    - U256::from(t.in0) * U256::from(3u64);
                let b1 = U256::from(t.next.reserve1) * U256::from(1000u64)
                    - U256::from(t.in1) * U256::from(3u64);
                // Reserves and inputs are uint112, so these products fit uint256.
                if b0 * b1
                    < U256::from(self.reserve0)
                        * U256::from(self.reserve1)
                        * U256::from(1_000_000u64)
                {
                    return Err(Error::Invariant);
                }
            }
            _ => return Err(Error::Encoding),
        }
        t.next.validate()?;
        Ok(t)
    }

    pub fn encode(&self) -> Vec<u8> {
        let mut b = b"CSWP\x01".to_vec();
        b.extend_from_slice(&self.nonce.to_le_bytes());
        b.extend_from_slice(&self.asset0);
        b.extend_from_slice(&self.asset1);
        for n in [self.reserve0, self.reserve1, self.total_lp] {
            b.extend_from_slice(&n.to_le_bytes())
        }
        b
    }

    pub fn decode(b: &[u8]) -> Result<Self> {
        if b.len() != 125 || &b[..5] != b"CSWP\x01" {
            return Err(Error::Encoding);
        }
        let s = Self {
            nonce: u64::from_le_bytes(b[5..13].try_into().unwrap()),
            asset0: b[13..45].try_into().unwrap(),
            asset1: b[45..77].try_into().unwrap(),
            reserve0: u128::from_le_bytes(b[77..93].try_into().unwrap()),
            reserve1: u128::from_le_bytes(b[93..109].try_into().unwrap()),
            total_lp: u128::from_le_bytes(b[109..125].try_into().unwrap()),
        };
        s.validate()?;
        Ok(s)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Action {
    pub op: u8,
    pub side: u8,
    pub actor: Hash,
    pub amount0: u128,
    pub amount1: u128,
    pub min0: u128,
    pub min1: u128,
    pub min_lp: u128,
}

impl Action {
    pub fn encode(&self) -> Vec<u8> {
        let mut b = vec![self.op, self.side];
        b.extend_from_slice(&self.actor);
        for n in [
            self.amount0,
            self.amount1,
            self.min0,
            self.min1,
            self.min_lp,
        ] {
            b.extend_from_slice(&n.to_le_bytes())
        }
        b
    }

    pub fn decode(b: &[u8]) -> Result<Self> {
        if b.len() != 114 {
            return Err(Error::Encoding);
        }
        let n = |at| u128::from_le_bytes(b[at..at + 16].try_into().unwrap());
        Ok(Self {
            op: b[0],
            side: b[1],
            actor: b[2..34].try_into().unwrap(),
            amount0: n(34),
            amount1: n(50),
            min0: n(66),
            min1: n(82),
            min_lp: n(98),
        })
    }
}

#[derive(Debug, Clone)]
pub struct Transition {
    pub next: State,
    pub in0: u128,
    pub in1: u128,
    pub out0: u128,
    pub out1: u128,
    pub lp_mint: u128,
    pub lp_burn: u128,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn action(op: u8, a: u128, b: u128) -> Action {
        Action {
            op,
            side: 0,
            actor: [3; 32],
            amount0: a,
            amount1: b,
            min0: 0,
            min1: 0,
            min_lp: 0,
        }
    }

    fn pool() -> State {
        State::new(ZERO, [1; 32])
            .apply(&action(ADD, 1_000_000, 2_000_000))
            .unwrap()
            .next
    }

    #[test]
    fn mint_minimum_and_withdraw() {
        let s = pool();
        assert_eq!(s.total_lp, 1_414_213);
        let t = s.apply(&action(REMOVE, s.total_lp - 1001, 0)).unwrap();
        assert_eq!(t.next.total_lp, 1001);
        assert!(t.next.reserve0 > 0);
    }

    #[test]
    fn permanently_locked_lp_cannot_be_removed() {
        let s = pool();
        assert!(s.apply(&action(REMOVE, s.total_lp, 0)).is_err());
    }

    #[test]
    fn add_uses_optimal_ratio() {
        let s = pool();
        let t = s.apply(&action(ADD, 100_000, 500_000)).unwrap();
        assert_eq!((t.in0, t.in1), (100_000, 200_000));
    }

    #[test]
    fn fee_stays_in_pool() {
        let s = pool();
        let t = s.apply(&action(SWAP_IN, 10000, 0)).unwrap();
        assert_eq!(t.out1, 19743);
        assert!(t.next.reserve0 * t.next.reserve1 > s.reserve0 * s.reserve1);
        assert_eq!(t.next.total_lp, s.total_lp);
    }

    #[test]
    fn exact_output_ceil_and_limit() {
        let s = pool();
        let t = s.apply(&action(SWAP_OUT, 19000, 10000)).unwrap();
        assert_eq!(t.out1, 19000);
        assert!(t.in0 < 10000);
        assert!(s.apply(&action(SWAP_OUT, 19000, t.in0 - 1)).is_err());
    }

    #[test]
    fn sqrt_and_large_products() {
        let n = MAX_RESERVE;
        assert_eq!(sqrt_product(n, n), n);
        assert_eq!(mul_div(n, n, n).unwrap(), n);
        let s = State::new([1; 32], [2; 32])
            .apply(&action(ADD, n - 1_000_000, n - 1_000_000))
            .unwrap()
            .next;
        let t = s.apply(&action(SWAP_IN, 1000, 0)).unwrap();
        assert!(t.out1 > 0);
        assert!(
            U256::from(t.next.reserve0) * U256::from(t.next.reserve1)
                >= U256::from(s.reserve0) * U256::from(s.reserve1)
        );
    }

    #[test]
    fn state_codec_and_order() {
        let s = pool();
        assert_eq!(State::decode(&s.encode()).unwrap(), s);
        let mut b = s.encode();
        b.push(0);
        assert!(State::decode(&b).is_err());
        assert!(State::new([1; 32], ZERO).validate().is_err());
    }

    #[test]
    fn slippage_and_capacity() {
        let s = pool();
        let mut a = action(SWAP_IN, 10000, 0);
        a.min0 = 20000;
        assert_eq!(s.apply(&a).unwrap_err(), Error::Slippage);
        let mut x = s;
        x.reserve0 = u64::MAX.into();
        assert!(x.validate().is_err());
    }

    #[test]
    fn round_trips_never_profit() {
        let mut s = pool();
        for _ in 0..1000 {
            let a = s.apply(&action(SWAP_IN, 1000, 0)).unwrap();
            let mut b = action(SWAP_IN, a.out1, 0);
            b.side = 1;
            let t = a.next.apply(&b).unwrap();
            assert!(t.out0 < 1000);
            s = t.next;
        }
        s.validate().unwrap();
    }
}
