use std::sync::LazyLock;

const ORDER: usize = 65_535;
const POLYNOMIAL: u32 = 0x1_100b;
const TABLED: usize = 2048;

pub static FIELD: LazyLock<Field> = LazyLock::new(Field::new);

pub struct Field {
    log: Vec<u16>,
    exp: Vec<u16>,
    constants: Vec<u16>,
}

impl Field {
    fn new() -> Self {
        let mut log = vec![0; ORDER + 1];
        let mut exp = vec![0; 2 * ORDER];
        let mut value = 1u32;
        for (index, entry) in exp[..ORDER].iter_mut().enumerate() {
            let word = u16::try_from(value).expect("reduction keeps values below 65536");
            *entry = word;
            log[usize::from(word)] = u16::try_from(index).expect("field order is 65535");
            value <<= 1;
            if value & 0x1_0000 != 0 {
                value ^= POLYNOMIAL;
            }
        }
        exp.copy_within(..ORDER, ORDER);
        let constants = (1..ORDER)
            .filter(|n| n % 3 != 0 && n % 5 != 0 && n % 17 != 0 && n % 257 != 0)
            .map(|n| exp[n])
            .collect();
        Self {
            log,
            exp,
            constants,
        }
    }

    pub fn mul(&self, a: u16, b: u16) -> u16 {
        if a == 0 || b == 0 {
            0
        } else {
            self.exp[usize::from(self.log[usize::from(a)]) + usize::from(self.log[usize::from(b)])]
        }
    }

    pub fn inverse(&self, value: u16) -> u16 {
        self.exp[ORDER - usize::from(self.log[usize::from(value)])]
    }

    pub fn coefficient(&self, input: usize, exponent: u32) -> u16 {
        let base = self.constants[input];
        let power = u64::from(self.log[usize::from(base)]) * u64::from(exponent);
        self.exp[usize::try_from(power % ORDER as u64).expect("below the field order")]
    }

    pub fn add_scaled(&self, destination: &mut [u8], source: &[u8], coefficient: u16) {
        match coefficient {
            0 => {}
            1 => {
                for (out, byte) in destination.iter_mut().zip(source) {
                    *out ^= byte;
                }
            }
            _ if source.len() >= TABLED => {
                let mut low = [0u16; 256];
                let mut high = [0u16; 256];
                for byte in 0..=255u8 {
                    low[usize::from(byte)] = self.mul(u16::from(byte), coefficient);
                    high[usize::from(byte)] = self.mul(u16::from(byte) << 8, coefficient);
                }
                let (words, tail) = source.as_chunks::<2>();
                let (outs, _) = destination.as_chunks_mut::<2>();
                for (out, &[bottom, top]) in outs.iter_mut().zip(words) {
                    let product = low[usize::from(bottom)] ^ high[usize::from(top)];
                    let product = product.to_le_bytes();
                    out[0] ^= product[0];
                    out[1] ^= product[1];
                }
                if let (Some(out), Some(&bottom)) = (outs.get_mut(words.len()), tail.first()) {
                    let product = low[usize::from(bottom)].to_le_bytes();
                    out[0] ^= product[0];
                    out[1] ^= product[1];
                }
            }
            _ => {
                for (out, input) in destination
                    .as_chunks_mut::<2>()
                    .0
                    .iter_mut()
                    .zip(source.chunks(2))
                {
                    let word = u16::from_le_bytes([input[0], input.get(1).copied().unwrap_or(0)]);
                    let product = self.mul(word, coefficient).to_le_bytes();
                    out[0] ^= product[0];
                    out[1] ^= product[1];
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::FIELD;

    #[test]
    fn the_table_path_multiplies_like_the_log_path() {
        let source: Vec<u8> = (0..8191u32).map(|i| (i * 31 % 251) as u8).collect();
        for coefficient in [2u16, 3, 255, 256, 1025, 32_768, u16::MAX] {
            let even = source.len().next_multiple_of(2);
            let mut tabled = vec![0xa5; even];
            FIELD.add_scaled(&mut tabled, &source, coefficient);
            let mut expected = vec![0xa5; even];
            for (at, input) in source.chunks(2).enumerate() {
                let word = u16::from_le_bytes([input[0], input.get(1).copied().unwrap_or(0)]);
                let product = FIELD.mul(word, coefficient).to_le_bytes();
                expected[2 * at] ^= product[0];
                expected[2 * at + 1] ^= product[1];
            }
            assert!(tabled == expected, "{coefficient}");
        }
    }

    #[test]
    fn matches_polynomial_arithmetic_and_the_par2_input_constants() {
        for a in [0u16, 1, 2, 255, 256, 1025, 32_768, u16::MAX] {
            for b in 0..=u16::MAX {
                let (mut left, mut right, mut expected) = (u32::from(a), b, 0u32);
                for _ in 0..16 {
                    if right & 1 != 0 {
                        expected ^= left;
                    }
                    right >>= 1;
                    left <<= 1;
                    if left & 0x1_0000 != 0 {
                        left ^= 0x1_100b;
                    }
                }
                assert_eq!(u32::from(FIELD.mul(a, b)), expected);
            }
        }
        assert_eq!(FIELD.constants.len(), 32_768);
        assert_eq!(
            &FIELD.constants[..8],
            &[2, 4, 16, 128, 256, 2048, 8192, 16384]
        );
        assert_eq!(FIELD.coefficient(3, 0), 1);
        assert_eq!(FIELD.coefficient(0, 3), 8);
        for value in 1..=u16::MAX {
            assert_eq!(FIELD.mul(value, FIELD.inverse(value)), 1);
        }
    }
}
