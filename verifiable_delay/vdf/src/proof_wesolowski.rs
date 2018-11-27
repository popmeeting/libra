// Copyright 2018 POA Networks Ltd.
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//   http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

use super::classgroup::ClassGroup;
use super::gmp_classgroup::ffi::mpz_powm;
use super::gmp_classgroup::GmpClassGroup;
use super::proof_of_time::{iterate_squarings, serialize};
use gmp::mpz::Mpz;
use sha2::{digest::FixedOutput, Digest, Sha256};
use std::{cmp::Eq, collections::HashMap, hash::Hash, mem, u64, usize};

#[derive(Debug, Clone)]
pub struct WesolowskiVDF {
    int_size_bits: u16,
}
use super::InvalidIterations as Bad;

#[derive(Clone, Copy, Eq, PartialEq, PartialOrd, Ord, Hash, Debug)]
pub struct WesolowskiVDFParams(pub u16);
impl super::VDFParams for WesolowskiVDFParams {
    type VDF = WesolowskiVDF;
    fn new(self) -> Self::VDF {
        WesolowskiVDF {
            int_size_bits: self.0,
        }
    }
}

impl super::VDF for WesolowskiVDF {
    fn check_difficulty(&self, _difficulty: u64) -> Result<(), Bad> {
        Ok(())
    }
    fn solve(&self, challenge: &[u8], difficulty: u64) -> Result<Vec<u8>, Bad> {
        if difficulty > usize::MAX as u64 {
            Err(Bad("Cannot have more that usize::MAX iterations".to_owned()))
        } else {
            Ok(create_proof_of_time_wesolowski::<GmpClassGroup>(
                challenge,
                difficulty as usize,
                self.int_size_bits,
            ))
        }
    }

    fn verify(
        &self,
        challenge: &[u8],
        difficulty: u64,
        alleged_solution: &[u8],
    ) -> Result<(), super::InvalidProof> {
        check_proof_of_time_wesolowski::<GmpClassGroup>(
            challenge,
            alleged_solution,
            difficulty,
            self.int_size_bits,
        )
        .map_err(|()| super::InvalidProof)
    }
}
/// To quote the original Python code:
///
/// > Create `L` and `k` parameters from papers, based on how many iterations
/// > need to be performed, and how much memory should be used.
pub fn approximate_parameters(t: f64) -> (usize, u8, u64) {
    let log_memory = (10_000_000.0f64).log2();
    let log_t = (t as f64).log2();
    let l = if log_t - log_memory > 0. {
        2.0f64.powf(log_memory - 20.).ceil()
    } else {
        1.
    };

    let intermediate = t * (2.0f64).ln() / (2.0 * l);
    let k = (intermediate.ln() - intermediate.ln().ln() + 0.25)
        .round()
        .max(1.);

    let w = (t / (t / k + l * (2.0f64).powf(k + 1.0)) - 2.0).floor();
    (l as _, k as _, w as _)
}

fn u64_to_bytes(q: u64) -> [u8; 8] {
    if false {
        // This use of `std::mem::transumte` is correct, but still not justified.
        unsafe { std::mem::transmute(q.to_be()) }
    } else {
        [
            (q >> 56) as u8,
            (q >> 48) as u8,
            (q >> 40) as u8,
            (q >> 32) as u8,
            (q >> 24) as u8,
            (q >> 16) as u8,
            (q >> 8) as u8,
            q as u8,
        ]
    }
}

/// Quote:
///
/// > Creates a random prime based on input s.
fn hash_prime(seed: &[&[u8]]) -> Mpz {
    let mut j = 0u64;
    loop {
        let mut hasher = Sha256::new();
        hasher.input(b"prime");
        hasher.input(u64_to_bytes(j));
        for i in seed {
            hasher.input(i);
        }
        let n = Mpz::from(&hasher.fixed_result()[..16]);
        if n.probab_prime(25) != gmp::mpz::ProbabPrimeResult::NotPrime {
            break n;
        }
        j += 1;
    }
}

/// Quote:
///
/// > Get“s the ith block of `2^T // B`, such that `sum(get_block(i) * 2^(k*i))
/// > = t^T // B`
fn get_block(i: u64, k: u8, t: u64, b: &Mpz) -> Mpz {
    let mut res = Mpz::new();
    super::gmp_classgroup::ffi::mpz_powm(
        &mut res,
        &Mpz::from(2),
        &Mpz::from(t - u64::from(k) * (i + 1)),
        b,
    );
    res *= Mpz::one() << k as usize;
    res / b
}

fn eval_optimized<T, L: ClassGroup<BigNum = Mpz> + Eq + Hash>(
    h: &L,
    b: &Mpz,
    t: usize,
    k: u8,
    l: usize,
    powers: &T,
) -> L
where
    T: for<'a> std::ops::Index<&'a u64, Output = L>,
{
    assert!(k > 0, "k cannot be zero");
    assert!(l > 0, "l cannot be zero");
    let kl = (k as usize)
        .checked_mul(l)
        .expect("computing k*l overflowed a u64");
    assert!(kl <= u64::MAX as _);
    assert!((kl as u64) < (1u64 << 53), "k*l overflowed an f64");
    assert!((t as u64) < (1u64 << 53), "t overflows an f64");
    assert!(
        k < (mem::size_of::<usize>() << 3) as u8,
        "k must be less than the number of bits in a usize"
    );
    let k1 = k >> 1;
    let k0 = k - k1;
    let mut x = h.identity();
    let identity = h.identity();
    let k_exp = 1usize << k;
    let k0_exp = 1usize << k0;
    let k1_exp = 1usize << k1;
    for j in (0..l).rev() {
        x.pow(Mpz::from(k_exp as u64));
        let mut ys: HashMap<Mpz, L> = HashMap::new();
        for b in 0..1usize << k {
            ys.entry(Mpz::from(b as u64))
                .or_insert_with(|| identity.clone());
        }
        let end_of_loop = ((t as f64) / kl as f64).ceil() as usize;
        assert!(end_of_loop == 0 || (end_of_loop as u64 - 1).checked_mul(l as u64).is_some());
        for i in 0..end_of_loop {
            if t < k as usize * (i * l + j + 1) {
                continue;
            }
            let b = get_block((i as u64) * (l as u64), k, t as _, b);
            *ys.get_mut(&b).unwrap() *= &powers[&((i * kl) as _)];
        }

        for b1 in 0..k1_exp {
            let mut z = identity.clone();
            for b0 in 0..k0_exp {
                z *= &ys[&Mpz::from((b1 * k0_exp + b0) as u64)]
            }
            z.pow(Mpz::from((b1 as u64) * (k0_exp as u64)));
            x *= &z;
        }

        for b0 in 0..k0_exp {
            let mut z = identity.clone();
            for b1 in 0..k1_exp {
                z *= &ys[&Mpz::from((b1 * k0_exp + b0) as u64)];
            }
            z.pow(Mpz::from(b0 as u64));
            x *= &z;
        }
    }
    x
}

pub fn generate_proof<T, V: ClassGroup<BigNum = Mpz> + Eq + Hash>(
    x: &V,
    iterations: u64,
    k: u8,
    l: usize,
    powers: &T,
    int_size_bits: usize,
) -> V
where
    T: for<'a> std::ops::Index<&'a u64, Output = V>,
{
    let element_len = 2 * ((int_size_bits + 16) >> 4);
    let mut x_buf = vec![0; element_len];
    x.serialize(&mut x_buf[..])
        .expect(super::INCORRECT_BUFFER_SIZE);
    let mut y_buf = vec![0; element_len];
    powers[&iterations]
        .serialize(&mut y_buf[..])
        .expect(super::INCORRECT_BUFFER_SIZE);
    let b = hash_prime(&[&x_buf[..], &y_buf[..]]);
    eval_optimized(&x, &b, iterations as _, k, l, powers)
}

/// Verify a proof, according to the Wesolowski paper.
pub fn verify_proof<V: ClassGroup<BigNum = Mpz>>(
    mut x: V,
    y: &V,
    mut proof: V,
    t: u64,
    int_size_bits: usize,
) -> Result<(), ()> {
    let element_len = 2 * ((int_size_bits + 16) >> 4);
    let mut x_buf = vec![0; element_len];
    x.serialize(&mut x_buf[..])
        .expect(super::INCORRECT_BUFFER_SIZE);
    let mut y_buf = vec![0; element_len];
    y.serialize(&mut y_buf[..])
        .expect(super::INCORRECT_BUFFER_SIZE);
    let b = hash_prime(&[&x_buf[..], &y_buf[..]]);
    let mut r = Mpz::new();
    mpz_powm(&mut r, &Mpz::from(2u64), &Mpz::from(t), &b);
    proof.pow(b);
    x.pow(r);
    proof *= &x;
    if &proof == y {
        Ok(())
    } else {
        Err(())
    }
}

pub fn create_proof_of_time_wesolowski<V: ClassGroup<BigNum = gmp::mpz::Mpz> + Eq + Hash>(
    challenge: &[u8],
    iterations: usize,
    int_size_bits: u16,
) -> Vec<u8>
where
    for<'a, 'b> &'a V: std::ops::Mul<&'b V, Output = V>,
    for<'a, 'b> &'a V::BigNum: std::ops::Mul<&'b V::BigNum, Output = V::BigNum>,
{
    let discriminant = super::create_discriminant::create_discriminant(&challenge, int_size_bits);
    let x = V::from_ab_discriminant(2.into(), 1.into(), discriminant);
    assert!((iterations as u128) < (1u128 << 53));
    let (l, k, _) = approximate_parameters(iterations as f64);
    let q = l.checked_mul(k as _).expect("bug");
    let powers = iterate_squarings(
        x.clone(),
        (0..=iterations / q + 1)
            .map(|i| i * q)
            .chain(Some(iterations))
            .map(|x| x as _),
    );
    let proof = generate_proof(&x, iterations as _, k, l, &powers, int_size_bits.into());
    serialize(&[proof], &powers[&(iterations as _)], int_size_bits.into())
}

pub fn check_proof_of_time_wesolowski<V: ClassGroup<BigNum = Mpz>>(
    challenge: &[u8],
    proof_blob: &[u8],
    iterations: u64,
    int_size_bits: u16,
) -> Result<(), ()> {
    let discriminant = super::create_discriminant::create_discriminant(challenge, int_size_bits);
    let x = V::from_ab_discriminant(2.into(), 1.into(), discriminant.clone());
    if (usize::MAX - 16) < int_size_bits.into() {
        return Err(());
    }
    let int_size = (usize::from(int_size_bits) + 16) >> 4;
    if int_size * 4 != proof_blob.len() {
        return Err(());
    }
    let (result_bytes, proof_bytes) = proof_blob.split_at(2 * int_size);
    let proof = ClassGroup::from_bytes(proof_bytes, discriminant.clone());
    let y = ClassGroup::from_bytes(result_bytes, discriminant);

    verify_proof(x, &y, proof, iterations, int_size_bits.into())
}
