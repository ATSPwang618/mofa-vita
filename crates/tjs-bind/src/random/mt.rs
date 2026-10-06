//! MT19937 with the observable TJS state/left/next representation.
//! rand_mt does not expose this representation; it remains the independent
//! output oracle in tests. Serialization must not substitute future samples.
// Algorithm adapted from tjsMT19937ar-cok.cpp (MT19937, 2002/2/10).
// Copyright (C) 1997 - 2002, Makoto Matsumoto and Takuji Nishimura.
// All rights reserved.
// Redistribution and use in source and binary forms, with or without
// modification, are permitted provided that the following conditions are met:
// 1. Redistributions of source code must retain the above copyright notice,
//    this list of conditions and the following disclaimer.
// 2. Redistributions in binary form must reproduce the above copyright notice,
//    this list of conditions and the following disclaimer in the documentation
//    and/or other materials provided with the distribution.
// 3. The names of its contributors may not be used to endorse or promote
//    products derived from this software without specific prior written permission.
// THIS SOFTWARE IS PROVIDED BY THE COPYRIGHT HOLDERS AND CONTRIBUTORS "AS IS"
// AND ANY EXPRESS OR IMPLIED WARRANTIES, INCLUDING, BUT NOT LIMITED TO, THE
// IMPLIED WARRANTIES OF MERCHANTABILITY AND FITNESS FOR A PARTICULAR PURPOSE
// ARE DISCLAIMED. IN NO EVENT SHALL THE COPYRIGHT OWNER OR CONTRIBUTORS BE
// LIABLE FOR ANY DIRECT, INDIRECT, INCIDENTAL, SPECIAL, EXEMPLARY, OR CONSEQUENTIAL
// DAMAGES (INCLUDING, BUT NOT LIMITED TO, PROCUREMENT OF SUBSTITUTE GOODS OR
// SERVICES; LOSS OF USE, DATA, OR PROFITS; OR BUSINESS INTERRUPTION) HOWEVER
// CAUSED AND ON ANY THEORY OF LIABILITY, WHETHER IN CONTRACT, STRICT LIABILITY,
// OR TORT (INCLUDING NEGLIGENCE OR OTHERWISE) ARISING IN ANY WAY OUT OF THE USE
// OF THIS SOFTWARE, EVEN IF ADVISED OF THE POSSIBILITY OF SUCH DAMAGE.

pub(super) const N: usize = 624;

pub(super) struct Generator {
    pub words: [u32; N],
    pub left: usize,
    pub next: usize,
}
impl Generator {
    pub fn seeded(seed: u32) -> Self {
        let mut words = [0u32; N];
        words[0] = seed;
        for i in 1..N {
            words[i] = 1812433253u32
                .wrapping_mul(words[i - 1] ^ (words[i - 1] >> 30))
                .wrapping_add(i as u32);
        }
        Self {
            words,
            left: 1,
            next: 0,
        }
    }

    pub fn keyed(key: &[u32]) -> Self {
        assert!(!key.is_empty());
        let mut state = Self::seeded(19650218);
        let mut i = 1;
        for j in 0..N.max(key.len()) {
            let previous = state.words[i - 1];
            state.words[i] = (state.words[i] ^ (previous ^ (previous >> 30)).wrapping_mul(1664525))
                .wrapping_add(key[j % key.len()])
                .wrapping_add((j % key.len()) as u32);
            i += 1;
            if i == N {
                state.words[0] = state.words[N - 1];
                i = 1;
            }
        }
        for _ in 1..N {
            let previous = state.words[i - 1];
            state.words[i] = (state.words[i]
                ^ (previous ^ (previous >> 30)).wrapping_mul(1566083941))
            .wrapping_sub(i as u32);
            i += 1;
            if i == N {
                state.words[0] = state.words[N - 1];
                i = 1;
            }
        }
        state.words[0] = 0x80000000;
        state
    }

    pub fn restore(words: [u32; N], left: i32, next: i32) -> Option<Self> {
        // The C++ class accepts unchecked pointer/counter data. Keep every
        // combination whose entire future execution stays within the array,
        // including short blocks not produced by ordinary seeding.
        if !(1..=625).contains(&left)
            || !(0..=624).contains(&next)
            || (left != 1 && next + left - 1 > 624)
        {
            return None;
        }
        Some(Self {
            words,
            left: left as usize,
            next: next as usize,
        })
    }

    pub fn next(&mut self) -> u32 {
        self.left -= 1;
        if self.left == 0 {
            for i in 0..N {
                let mix = (self.words[i] & 0x80000000) | (self.words[(i + 1) % N] & 0x7fffffff);
                self.words[i] = self.words[(i + 397) % N]
                    ^ (mix >> 1)
                    ^ (0u32.wrapping_sub(mix & 1) & 0x9908b0df);
            }
            self.left = N;
            self.next = 0;
        }
        let mut word = self.words[self.next];
        self.next += 1;
        word ^= word >> 11;
        word ^= (word << 7) & 0x9d2c5680;
        word ^= (word << 15) & 0xefc60000;
        word ^ (word >> 18)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn state_and_stream_match_independent_mt_implementation() {
        for seed in [0, 1, 5489, 0x12345678, u32::MAX] {
            let mut state = Generator::seeded(seed);
            let mut oracle = rand_mt::Mt::new(seed);
            assert_eq!((state.left, state.next), (1, 0));
            for _ in 0..3000 {
                assert_eq!(state.next(), oracle.next_u32());
            }
        }
        for key in [
            &[0, 0][..],
            &[123, 0],
            &[u32::MAX, u32::MAX],
            &[0x123, 0x234, 0x345, 0x456],
        ] {
            let mut state = Generator::keyed(key);
            let mut oracle = rand_mt::Mt::new_with_key(key.iter().copied());
            assert_eq!(state.words[0], 0x80000000);
            for _ in 0..3000 {
                assert_eq!(state.next(), oracle.next_u32());
            }
        }
        // Published rand_mt seed-state vector for seed 0x12345678.
        assert_eq!(
            &Generator::seeded(0x12345678).words[..4],
            &[305419896, 775181657, 499207455, 1600259134]
        );
    }
}
