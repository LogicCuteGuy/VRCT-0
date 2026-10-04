//! MT19937 with Python integer seeding/getrandbits/shuffle compatibility.
//! This keeps existing --seed scene selections and pilot sampling reproducible.
pub(crate) struct Random {
    state: [u32; 624],
    index: usize,
}
impl Random {
    pub fn new(seed: i64) -> Self {
        let seed = seed.unsigned_abs();
        let mut key = vec![seed as u32];
        if seed >> 32 != 0 {
            key.push((seed >> 32) as u32);
        }
        let mut rng = Self {
            state: [0; 624],
            index: 624,
        };
        rng.state[0] = 19650218;
        for i in 1..624 {
            rng.state[i] = 1812433253u32
                .wrapping_mul(rng.state[i - 1] ^ (rng.state[i - 1] >> 30))
                .wrapping_add(i as u32);
        }
        let (mut i, mut j) = (1, 0);
        for _ in 0..624.max(key.len()) {
            rng.state[i] = (rng.state[i]
                ^ (rng.state[i - 1] ^ (rng.state[i - 1] >> 30)).wrapping_mul(1664525))
            .wrapping_add(key[j])
            .wrapping_add(j as u32);
            i += 1;
            j += 1;
            if i >= 624 {
                rng.state[0] = rng.state[623];
                i = 1;
            }
            if j >= key.len() {
                j = 0;
            }
        }
        for _ in 0..623 {
            rng.state[i] = (rng.state[i]
                ^ (rng.state[i - 1] ^ (rng.state[i - 1] >> 30)).wrapping_mul(1566083941))
            .wrapping_sub(i as u32);
            i += 1;
            if i >= 624 {
                rng.state[0] = rng.state[623];
                i = 1;
            }
        }
        rng.state[0] = 0x80000000;
        rng
    }
    fn next(&mut self) -> u32 {
        if self.index >= 624 {
            for i in 0..624 {
                let y = (self.state[i] & 0x80000000) | (self.state[(i + 1) % 624] & 0x7fffffff);
                self.state[i] = self.state[(i + 397) % 624]
                    ^ (y >> 1)
                    ^ if y & 1 != 0 { 0x9908b0df } else { 0 };
            }
            self.index = 0;
        }
        let mut y = self.state[self.index];
        self.index += 1;
        y ^= y >> 11;
        y ^= (y << 7) & 0x9d2c5680;
        y ^= (y << 15) & 0xefc60000;
        y ^= y >> 18;
        y
    }
    fn below(&mut self, n: usize) -> usize {
        let bits = usize::BITS - n.leading_zeros();
        loop {
            let v = if bits <= 32 {
                (self.next() >> (32 - bits)) as usize
            } else {
                self.next() as usize | (((self.next() >> (64 - bits)) as usize) << 32)
            };
            if v < n {
                return v;
            }
        }
    }
    pub fn shuffle<T>(&mut self, items: &mut [T]) {
        for i in (1..items.len()).rev() {
            let j = self.below(i + 1);
            items.swap(i, j);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn legacy_python_seed_zero_shuffle() {
        for (seed, expected) in [
            (0, [7, 8, 1, 5, 3, 4, 2, 0, 9, 6]),
            (42, [7, 3, 2, 8, 5, 6, 9, 4, 0, 1]),
        ] {
            let mut values: Vec<_> = (0..10).collect();
            Random::new(seed).shuffle(&mut values);
            assert_eq!(values, expected);
        }
    }
    #[test]
    fn negative_integer_seed_matches_its_absolute_value() {
        let mut positive: Vec<_> = (0..100).collect();
        let mut negative = positive.clone();
        Random::new(42).shuffle(&mut positive);
        Random::new(-42).shuffle(&mut negative);
        assert_eq!(positive, negative);
    }
}
