#![no_std]
#![no_main]
use alloc::{vec, vec::Vec};
use blake2b_ref::Blake2bBuilder;
use ckb_std::{
    ckb_constants::Source,
    ckb_types::{packed::Script, prelude::*},
    error::SysError,
    high_level::*,
};
use ckb_swap_protocol::*;
ckb_std::entry!(entry);
ckb_std::default_alloc!();

fn entry() -> i8 {
    match verify() {
        Ok(()) => 0,
        Err(e) => e as i8,
    }
}

fn sys<T>(r: core::result::Result<T, SysError>) -> Result<T> {
    r.map_err(|_| Error::Structure)
}

fn hash(b: &[u8]) -> Hash {
    let mut out = [0; 32];
    let mut h = Blake2bBuilder::new(32)
        .personal(b"ckb-default-hash")
        .build();
    h.update(b);
    h.finalize(&mut out);
    out
}

fn args(s: &Script) -> Vec<u8> {
    s.args().raw_data().to_vec()
}

fn protocol(base: &Script, role: u8, id: &Hash, asset: Option<&Hash>) -> Script {
    let mut a = vec![role];
    a.extend_from_slice(id);
    if let Some(v) = asset {
        a.extend_from_slice(v)
    }
    Script::new_builder()
        .code_hash(base.code_hash())
        .hash_type(base.hash_type())
        .args(a.pack())
        .build()
}

fn lp(base: &Script, id: &Hash) -> Script {
    let mut a = hash(protocol(base, 1, id, None).as_slice()).to_vec();
    a.extend_from_slice(&[0; 4]);
    Script::new_builder()
        .code_hash(ckb_std::ckb_types::packed::Byte32::from_slice(&XUDT_CODE_HASH).unwrap())
        .hash_type(2u8)
        .args(a.pack())
        .build()
}

fn count(source: Source) -> usize {
    QueryIter::new(load_cell_capacity, source).count()
}

fn indices(s: &Script, source: Source, lock: bool) -> Vec<usize> {
    (0..count(source))
        .filter(|i| {
            if lock {
                load_cell_lock(*i, source).ok().as_ref() == Some(s)
            } else {
                load_cell_type(*i, source).ok().flatten().as_ref() == Some(s)
            }
        })
        .collect()
}

fn state_count(base: &Script, source: Source) -> usize {
    (0..count(source))
        .filter(|i| {
            load_cell_type(*i, source).ok().flatten().is_some_and(|s| {
                s.code_hash() == base.code_hash()
                    && s.hash_type() == base.hash_type()
                    && args(&s).first() == Some(&0)
            })
        })
        .count()
}

fn amount(i: usize, source: Source) -> Result<u128> {
    let b = sys(load_cell_data(i, source))?;
    Ok(u128::from_le_bytes(
        b.get(..16)
            .ok_or(Error::Token)?
            .try_into()
            .map_err(|_| Error::Token)?,
    ))
}

fn canonical_xudt(s: &Script) -> bool {
    let a = args(s);
    s.code_hash().as_slice() == XUDT_CODE_HASH
        && s.hash_type() == 2u8.into()
        && a.len() == 36
        && a[32..] == [0; 4]
}

fn total(asset: &Hash, source: Source, owner: Option<&Hash>) -> Result<u128> {
    let mut sum = 0u128;
    for i in 0..count(source) {
        if owner.is_some_and(|v| load_cell_lock_hash(i, source).ok().as_ref() != Some(v)) {
            continue;
        }
        let n = if *asset == ZERO {
            u128::from(sys(load_cell_capacity(i, source))?)
        } else {
            let Some(s) = sys(load_cell_type(i, source))? else {
                continue;
            };
            if hash(s.as_slice()) != *asset {
                continue;
            }
            amount(i, source)?
        };
        sum = sum.checked_add(n).ok_or(Error::Arithmetic)?
    }
    Ok(sum)
}

fn reserve_cells(
    base: &Script,
    id: &Hash,
    s: &State,
    source: Source,
    state_input: Option<usize>,
) -> Result<()> {
    let mut output_index = 1u32;
    let mut expected = 0;
    for (asset, reserve) in [(s.asset0, s.reserve0), (s.asset1, s.reserve1)] {
        if asset == ZERO {
            continue;
        }
        expected += 1;
        let lock = protocol(base, 2, id, Some(&asset));
        let found = indices(&lock, source, true);
        if found.len() != 1 {
            return Err(Error::Structure);
        }
        let i = found[0];
        if matches!(source, Source::Output) && i != output_index as usize {
            return Err(Error::Structure);
        }
        if let Some(si) = state_input {
            let state_out = sys(load_input(si, Source::Input))?.previous_output();
            let reserve_out = sys(load_input(i, Source::Input))?.previous_output();
            if reserve_out.tx_hash() != state_out.tx_hash()
                || Unpack::<u32>::unpack(&reserve_out.index()) != output_index
            {
                return Err(Error::Structure);
            }
        }
        let token = sys(load_cell_type(i, source))?.ok_or(Error::Token)?;
        if !canonical_xudt(&token)
            || hash(token.as_slice()) != asset
            || token == lp(base, id)
            || sys(load_cell_data(i, source))?.len() != 16
            || amount(i, source)? != reserve
            || sys(load_cell_capacity(i, source))? != VAULT_CAPACITY
        {
            return Err(Error::Token);
        }
        output_index += 1;
    }
    let actual = (0..count(source))
        .filter(|i| {
            load_cell_lock(*i, source).ok().is_some_and(|v| {
                let a = args(&v);
                v.code_hash() == base.code_hash()
                    && v.hash_type() == base.hash_type()
                    && a.len() >= 33
                    && a[0] == 2
                    && a[1..33] == id[..]
            })
        })
        .count();
    if actual != expected {
        return Err(Error::Structure);
    }
    Ok(())
}

fn verify() -> Result<()> {
    let script = sys(load_script())?;
    let a = args(&script);
    if !matches!(a.len(), 33 | 65) {
        return Err(Error::Encoding);
    }
    let id: Hash = a[1..33].try_into().map_err(|_| Error::Encoding)?;
    match a[0] {
        0 if a.len() == 33 => verify_state(&script, &id),
        1 if a.len() == 33 => {
            if count(Source::GroupInput) != 1
                || sys(load_cell_type(0, Source::GroupInput))?
                    != Some(protocol(&script, 0, &id, None))
            {
                return Err(Error::Structure);
            }
            Ok(())
        }
        2 if a.len() == 65 => {
            if indices(&protocol(&script, 0, &id, None), Source::Input, false).len() != 1 {
                return Err(Error::Authorization);
            }
            Ok(())
        }
        _ => Err(Error::Encoding),
    }
}

fn verify_state(script: &Script, id: &Hash) -> Result<()> {
    let inputs = indices(script, Source::Input, false);
    let outputs = indices(script, Source::Output, false);
    if outputs != [0]
        || inputs.len() > 1
        || state_count(script, Source::Output) != 1
        || state_count(script, Source::Input) != inputs.len()
    {
        return Err(Error::Structure);
    }
    let after = State::decode(&sys(load_cell_data(0, Source::Output))?)?;
    if sys(load_cell_lock(0, Source::Output))? != protocol(script, 1, id, None)
        || sys(load_cell_capacity(0, Source::Output))? != after.capacity()?
    {
        return Err(Error::Capacity);
    }
    reserve_cells(script, id, &after, Source::Output, None)?;
    let lp_hash = hash(lp(script, id).as_slice());
    if after.asset0 == lp_hash || after.asset1 == lp_hash {
        return Err(Error::Token);
    }
    if inputs.is_empty() {
        let mut seed = sys(load_input(0, Source::Input))?.as_slice().to_vec();
        seed.extend_from_slice(&0u64.to_le_bytes());
        if hash(&seed) != *id
            || after != State::new(after.asset0, after.asset1)
            || total(&lp_hash, Source::Input, None)? != 0
            || total(&lp_hash, Source::Output, None)? != 0
        {
            return Err(Error::Genesis);
        }
        return Ok(());
    }
    let si = inputs[0];
    let before = State::decode(&sys(load_cell_data(si, Source::Input))?)?;
    if sys(load_cell_lock(si, Source::Input))? != protocol(script, 1, id, None)
        || sys(load_cell_capacity(si, Source::Input))? != before.capacity()?
    {
        return Err(Error::Capacity);
    }
    reserve_cells(script, id, &before, Source::Input, Some(si))?;
    let witness = sys(load_witness_args(si, Source::Input))?;
    let action = Action::decode(
        &witness
            .input_type()
            .to_opt()
            .ok_or(Error::Encoding)?
            .raw_data(),
    )?;
    if !QueryIter::new(load_cell_lock_hash, Source::Input).any(|h| h == action.actor) {
        return Err(Error::Authorization);
    }
    let t = before.apply(&action)?;
    if after != t.next {
        return Err(Error::Invariant);
    }
    let fee = total(&ZERO, Source::Input, None)?
        .checked_sub(total(&ZERO, Source::Output, None)?)
        .ok_or(Error::Capacity)?;
    for (asset, input, output) in [
        (before.asset0, t.in0, t.out0),
        (before.asset1, t.in1, t.out1),
    ] {
        let old = total(&asset, Source::Input, Some(&action.actor))?;
        let new = total(&asset, Source::Output, Some(&action.actor))?;
        if asset == ZERO {
            if new
                .checked_add(input)
                .and_then(|v| v.checked_add(fee))
                .ok_or(Error::Arithmetic)?
                < old.checked_add(output).ok_or(Error::Arithmetic)?
            {
                return Err(Error::Capacity);
            }
        } else if new.checked_add(input) != old.checked_add(output) {
            return Err(Error::Token);
        }
    }
    for owner in [None, Some(&action.actor)] {
        let old = total(&lp_hash, Source::Input, owner)?;
        let new = total(&lp_hash, Source::Output, owner)?;
        if new.checked_add(t.lp_burn) != old.checked_add(t.lp_mint) {
            return Err(Error::Token);
        }
    }
    Ok(())
}
