//! Platform boundary for GF(256) multiply/XOR. Splitting a byte into nibbles
//! preserves the existing field exactly: multiplication distributes over XOR.
use std::sync::OnceLock;

type Accumulate = fn(&mut [u8], &[u8], &[u8; 256]);

pub(super) fn accumulate(output: &mut [u8], input: &[u8], products: &[u8; 256]) {
    assert_eq!(output.len(), input.len());
    static IMPLEMENTATION: OnceLock<Accumulate> = OnceLock::new();
    let implementation = IMPLEMENTATION.get_or_init(|| {
        #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
        if std::arch::is_x86_feature_detected!("ssse3") {
            return accumulate_ssse3;
        }
        accumulate_scalar
    });
    implementation(output, input, products);
}

fn accumulate_scalar(output: &mut [u8], input: &[u8], products: &[u8; 256]) {
    for (destination, byte) in output.iter_mut().zip(input) {
        *destination ^= products[usize::from(*byte)];
    }
}

#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
fn accumulate_ssse3(output: &mut [u8], input: &[u8], products: &[u8; 256]) {
    // SAFETY: The cached dispatcher selects this function only after runtime
    // SSSE3 detection. The slices are equal in length and exclusively borrowed
    // for output; the vector routine accesses only complete 16-byte chunks.
    unsafe { vector::accumulate(output, input, products) }
}

#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
mod vector {
    #[cfg(target_arch = "x86")]
    use std::arch::x86::*;
    #[cfg(target_arch = "x86_64")]
    use std::arch::x86_64::*;

    /// Caller must establish SSSE3 support and equal slice lengths. All pointers
    /// are borrowed only for this call; unaligned loads never cross a slice end.
    #[target_feature(enable = "ssse3")]
    pub(super) unsafe fn accumulate(output: &mut [u8], input: &[u8], products: &[u8; 256]) {
        let high_products: [u8; 16] = std::array::from_fn(|index| products[index << 4]);
        // SAFETY: Both tables contain at least sixteen initialized bytes.
        let low_table = unsafe { _mm_loadu_si128(products.as_ptr().cast()) };
        let high_table = unsafe { _mm_loadu_si128(high_products.as_ptr().cast()) };
        let nibble_mask = _mm_set1_epi8(0x0f);
        let mut offset = 0;
        while input.len() - offset >= 16 {
            // SAFETY: The loop bounds prove that all three unaligned accesses
            // cover sixteen initialized bytes inside the equal-length slices.
            unsafe {
                let bytes = _mm_loadu_si128(input.as_ptr().add(offset).cast());
                let low = _mm_and_si128(bytes, nibble_mask);
                let high = _mm_and_si128(_mm_srli_epi16::<4>(bytes), nibble_mask);
                let product = _mm_xor_si128(
                    _mm_shuffle_epi8(low_table, low),
                    _mm_shuffle_epi8(high_table, high),
                );
                let destination = output.as_mut_ptr().add(offset).cast();
                let previous = _mm_loadu_si128(destination);
                _mm_storeu_si128(destination, _mm_xor_si128(previous, product));
            }
            offset += 16;
        }
        super::accumulate_scalar(&mut output[offset..], &input[offset..], products);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accelerated_field_matches_scalar_for_every_coefficient_byte_and_tail() {
        let field = super::super::multiplication_table();
        let input: Vec<_> = (0..2048).map(|index| index as u8).collect();
        for products in field {
            for length in [0, 1, 15, 16, 17, 31, 32, 33, 255, 256, 1392] {
                // Offset both slices to exercise unaligned vector reads/writes.
                let data = &input[3..3 + length];
                let mut expected = vec![0xa5; length];
                accumulate_scalar(&mut expected, data, products);
                let mut actual = vec![0xa5; length + 1];
                accumulate(&mut actual[1..], data, products);
                assert_eq!(&actual[1..], expected);
                assert_eq!(actual[0], 0xa5);
            }
        }
    }
}
