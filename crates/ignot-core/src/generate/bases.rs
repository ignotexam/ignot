use super::{Generated, Level};
use crate::grade::GradeKind;
use crate::rng::Rng;
use crate::{Result, invalid};

/// Hex digits of the first number at each level. At level 5 the product is just under 2^126.
const HEX_DIGITS: [u32; 5] = [4, 7, 10, 14, 18];

pub(super) fn bases(rng: &mut Rng, level: Level) -> Result<Generated> {
    let digits = level.pick(HEX_DIGITS);
    let a = rng.big(16u128.pow(digits - 1), 16u128.pow(digits) - 1);
    let b = rng.big(8u128.pow(digits - 1), 8u128.pow(digits) - 1);
    let c = rng.big(1 << (2 * digits), 1 << (2 * digits + 2));
    let d = u128::from(rng.int(3, 9));
    let Some(value) = a.checked_mul(b).zip(c.checked_mul(d)).and_then(|(product, taken)| product.checked_sub(taken))
    else {
        return invalid("bases: the result does not fit");
    };
    Ok(Generated {
        prompt: format!(
            "Take the hexadecimal number {a:X} and multiply it by the octal number {b:o}. From that, subtract {d} times the binary number {c:b}. Write the result in base 7. Answer with the base-7 digits only."
        ),
        answer: in_base(value, 7),
        grade: GradeKind::Exact,
    })
}

/// A number written in a base from 2 to 10.
fn in_base(mut n: u128, base: u8) -> String {
    let mut digits = Vec::new();
    loop {
        // The remainder is below the base, which is at most 10.
        digits.push(b'0' + (n % u128::from(base)) as u8);
        n /= u128::from(base);
        if n == 0 {
            break;
        }
    }
    digits.iter().rev().map(|&d| char::from(d)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Solves the question from its own text, with a different method: modular arithmetic on the digits.
    fn key_agrees_with_the_prompt(question: &Generated) {
        let words: Vec<&str> = question.prompt.split(' ').collect();
        let after = |word: &str| {
            words[words.iter().position(|w| w.trim_end_matches('.') == word).unwrap() + 1].trim_end_matches('.')
        };
        let hex = after("number");
        let octal = words[words.iter().position(|&w| w == "octal").unwrap() + 2].trim_end_matches('.');
        let times: u128 = after("subtract").parse().unwrap();
        let binary = words[words.iter().position(|&w| w == "binary").unwrap() + 2].trim_end_matches('.');
        for modulus in [1_000_000_007u128, 998_244_353, 4_294_967_291] {
            let residue = |text: &str, base: u128| {
                text.chars().fold(0u128, |acc, c| (acc * base + u128::from(c.to_digit(16).unwrap())) % modulus)
            };
            let expected = (residue(hex, 16) * residue(octal, 8) % modulus + modulus * modulus
                - residue(binary, 2) * times % modulus)
                % modulus;
            assert_eq!(residue(&question.answer, 7), expected, "{}", question.prompt);
        }
    }

    #[test]
    fn the_key_is_the_arithmetic_in_the_question() {
        for level in Level::ALL {
            for i in 0..40 {
                let question = bases(&mut Rng::from_text(&format!("bases {i}")), level).unwrap();
                key_agrees_with_the_prompt(&question);
                assert!(question.answer.bytes().all(|c| (b'0'..=b'6').contains(&c)));
                assert!(!question.answer.starts_with('0'));
            }
        }
    }

    #[test]
    fn numbers_print_in_other_bases() {
        assert_eq!(in_base(0, 7), "0");
        assert_eq!(in_base(6, 7), "6");
        assert_eq!(in_base(7, 7), "10");
        assert_eq!(in_base(342, 7), "666");
        assert_eq!(in_base(u128::MAX, 2), "1".repeat(128));
        assert_eq!(in_base(1_000_000, 10), "1000000");
    }

    #[test]
    fn a_worked_example() {
        // 0x1F * 0o17 - 3 * 0b101 = 31 * 15 - 15 = 450 = 1212 in base 7 (343 + 2*49 + 7 + 2).
        assert_eq!(in_base(31 * 15 - 3 * 5, 7), "1212");
    }
}
