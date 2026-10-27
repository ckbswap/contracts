# CKB Swap Contracts


[**Explore the interactive Cell transition →**](https://ckbswap.github.io/contracts/)

See how a CKB → SWAP exchange consumes and creates Cells. Adjust the amount to explore changes to pool reserves and Cell capacity.
A constant-product automated market maker (AMM) implemented in Rust for CKB. The protocol supports CKB/xUDT and xUDT/xUDT pools, liquidity provision, liquidity withdrawal, and swaps with either a fixed input or a fixed output.

The repository separates pool calculations and binary encoding from on-chain transaction validation. Both crates use `no_std`.

## Repository Structure

```text
contracts/
  swap/
    Cargo.toml
    src/main.rs       # CKB script entry point and transaction validation
crates/
  protocol/
    Cargo.toml
    src/lib.rs        # Pool state, actions, arithmetic, encoding, and unit tests
```

- **`ckb-swap-protocol`** defines the state machine, swap formulas, liquidity accounting, and serialization. It uses 256-bit intermediate arithmetic.
- **`ckb-swap-script`** validates script roles, pool initialization, reserve cells, actor authorization, asset movements, and LP minting or burning.

## Assets and Pool Accounting

Assets are identified by 32-byte values:

- CKB is represented by the all-zero hash. Amounts are measured in shannons, with `1 CKB = 100,000,000 shannons`.
- An xUDT asset is represented by the CKB hash of its serialized type script. Token amounts use the raw integer units stored in cell data.

Asset identifiers must satisfy `asset0 < asset1` in lexicographic byte order. A pool containing CKB therefore uses CKB as `asset0`.

Reserves and total LP accounting are bounded by `2^112 - 1`. CKB reserves must also fit within the state cell's `u64` capacity after adding its fixed capacity allocation.

### Liquidity

The first liquidity deposit establishes the reserve ratio. Its total LP accounting is:

```text
initial_total_lp = floor(sqrt(amount0 * amount1))
provider_lp = initial_total_lp - 1000
```

The minimum liquidity of `1000` is included in `total_lp` but is never minted to the provider, keeping that portion permanently unavailable for withdrawal. The initial square root must exceed `1000`.

Subsequent deposits use the existing reserve ratio and mint LP proportional to the actual amounts accepted. Withdrawals burn LP and return proportional amounts of both assets, rounded down.

### Swaps

Swaps charge a **0.3% fee**, retained in the pool. For input reserve `rin` and output reserve `rout`, the calculations are:

```text
output = floor(input * 997 * rout / (rin * 1000 + input * 997))
input = floor(rin * output * 1000 / ((rout - output) * 997)) + 1
```

The script checks the resulting state against the protocol transition. Swap calculations also enforce a fee-adjusted constant-product invariant. Each successful liquidity or swap action increments the state nonce by one.

## Script Roles and Cell Layout

The same script binary serves three roles, selected by the first byte of its arguments. All roles share the same script code hash and hash type.

| Role | Argument layout | Purpose |
| --- | --- | --- |
| `0` | `role: u8 \| pool_id: [u8; 32]` | State cell type script; validates initialization and transitions |
| `1` | `role: u8 \| pool_id: [u8; 32]` | State cell lock; requires one group input carrying the corresponding state type script |
| `2` | `role: u8 \| pool_id: [u8; 32] \| asset: [u8; 32]` | xUDT reserve lock; requires the corresponding state input |

### State Cell

The state cell must be transaction output `0`. It uses role `0` as its type script and role `1` as its lock script. Its capacity is:

- **CKB/xUDT:** `500 CKB + reserve0`, with `reserve0` expressed in shannons.
- **xUDT/xUDT:** exactly `500 CKB`.

CKB liquidity is held directly in the state cell's capacity. State validation permits exactly one state output across pools using the same script code hash and hash type, so those pools cannot be updated together in one transaction.

### xUDT Reserve Cells

Each non-CKB asset has exactly one reserve cell, locked by role `2` for that pool and asset. Reserve outputs follow the state cell, starting at output `1`, in asset order while skipping CKB.

Each reserve cell must:

- Have exactly `200 CKB` capacity.
- Use the configured xUDT code hash, hash type `2`, and 36-byte arguments ending in four zero bytes.
- Store exactly 16 bytes of data containing the reserve amount as a little-endian `u128`.
- Match the asset identifier and reserve amount recorded in the state.

When spending an existing pool, its state and reserve inputs must reference outputs from the same previous transaction. Reserve output indices in that transaction must match the required layout.

### LP Token

LP tokens use an xUDT type script with the configured xUDT code hash and hash type `2`. Its arguments are the hash of the pool's role `1` lock script followed by four zero bytes.

The script validates LP balance changes both across the entire transaction and for the actor. A pool's LP token cannot also be one of its reserve assets.

### Pool Initialization

A pool ID is derived as:

```text
pool_id = ckb_hash(serialized_first_transaction_input || little_endian_u64(0))
```

Here, `ckb_hash` is a 32-byte Blake2b hash personalized with `ckb-default-hash`.

Initialization creates a state with nonce, reserves, and total LP all set to zero. It must contain no pool LP tokens in either inputs or outputs. The initial liquidity deposit is a subsequent state transition.

## State Encoding

State cell data is exactly **125 bytes**. All integer fields use little-endian encoding; offsets are zero-based.

| Offset | Size | Field |
| --- | --- | --- |
| `0` | 5 bytes | Magic and version: `CSWP` followed by `0x01` |
| `5` | 8 bytes | `nonce: u64` |
| `13` | 32 bytes | `asset0` |
| `45` | 32 bytes | `asset1` |
| `77` | 16 bytes | `reserve0: u128` |
| `93` | 16 bytes | `reserve1: u128` |
| `109` | 16 bytes | `total_lp: u128` |

## Actions and Witness Encoding

For an existing pool, the action is stored in `WitnessArgs.input_type` at the state input's transaction index. The actor is a lock script hash that must appear among transaction inputs.

Action data is exactly **114 bytes**, with little-endian integer fields:

| Offset | Size | Field |
| --- | --- | --- |
| `0` | 1 byte | `op` |
| `1` | 1 byte | `side` |
| `2` | 32 bytes | `actor` |
| `34` | 16 bytes | `amount0: u128` |
| `50` | 16 bytes | `amount1: u128` |
| `66` | 16 bytes | `min0: u128` |
| `82` | 16 bytes | `min1: u128` |
| `98` | 16 bytes | `min_lp: u128` |

### Action Parameters

| Operation | Value | Amount parameters | Slippage limits |
| --- | --- | --- | --- |
| `ADD` | `0` | `amount0` and `amount1` are the available deposit amounts | `min0` and `min1` are minimum accepted deposits; `min_lp` is minimum LP minted |
| `REMOVE` | `1` | `amount0` is LP to burn; `amount1` must be zero | `min0` and `min1` are minimum asset returns; `min_lp` must be zero |
| `SWAP_IN` | `2` | `amount0` is exact input; `amount1` must be zero | `min0` is minimum output; `min1` and `min_lp` must be zero |
| `SWAP_OUT` | `3` | `amount0` is exact output; `amount1` is maximum input | `min0`, `min1`, and `min_lp` must be zero |

For swaps, `side = 0` exchanges asset0 for asset1, and `side = 1` exchanges asset1 for asset0. Liquidity actions require `side = 0`.

The script checks the actor's asset balance changes against the transition. For CKB, it accounts for the transaction fee as the difference between total input and output capacities.

## Error Codes

The script returns `0` on success and the following codes on validation failure:

| Code | Error | Meaning |
| --- | --- | --- |
| `5` | `Encoding` | Invalid binary format, role, operation, or action fields |
| `6` | `Arithmetic` | Overflow or invalid arithmetic |
| `7` | `Amount` | Invalid liquidity or swap amount |
| `8` | `Slippage` | An action exceeds its input limit or misses a minimum |
| `9` | `Invariant` | Invalid pool state or state transition |
| `10` | `Authorization` | Missing actor input or corresponding pool state input |
| `11` | `Structure` | Invalid transaction layout or a mapped syscall failure |
| `12` | `Capacity` | Invalid cell capacity or CKB accounting |
| `13` | `Token` | Invalid token script, data, or balance accounting |
| `14` | `Genesis` | Invalid pool initialization |

## Tests and Repository Scope

The protocol crate includes unit tests for initial LP accounting, minimum liquidity retention, proportional deposits, swap fees, exact-output input limits, large integer arithmetic, state encoding, asset ordering, slippage, capacity limits, and repeated round-trip swaps.

The repository currently contains the protocol library and on-chain validation script. It does not include a transaction builder, deployment configuration, or CKB VM integration tests. The xUDT code hash is fixed by `XUDT_CODE_HASH` in the protocol crate.
