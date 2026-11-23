use super::{Generated, Level};
use crate::grade::GradeKind;
use crate::rng::Rng;

const STEPS: [i64; 5] = [12, 30, 60, 120, 250];
const MODULI: [i64; 5] = [83, 89, 97, 101, 103];

/// A small program over three variables. It is written out as Python for the question and run here for the
/// key; nothing is ever handed to an interpreter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Program {
    start: [i64; 3],
    steps: i64,
    /// `if (a + i) % first_every == 0: a = (a * a_times + b) % a_modulus`
    first_every: i64,
    a_times: i64,
    a_modulus: i64,
    /// `elif b % 2 == 1: b = (b + c + i * b_times) % b_modulus`
    b_times: i64,
    b_modulus: i64,
    /// `else: c = (c * c_times - a + i) % c_modulus`
    c_times: i64,
    c_modulus: i64,
    /// `if i % swap_every == swap_at: a, c = c, a`
    swap_every: i64,
    swap_at: i64,
}

fn make(rng: &mut Rng, level: Level) -> Program {
    let p: [i64; 9] = std::array::from_fn(|_| i64::from(rng.int(2, 9)));
    let m: [i64; 3] = std::array::from_fn(|_| *rng.choice(&MODULI));
    Program {
        start: [p[0], p[1], p[2]],
        steps: level.pick(STEPS),
        first_every: p[3] % 3 + 2,
        a_times: p[4],
        a_modulus: m[0],
        b_times: p[5],
        b_modulus: m[1],
        c_times: p[6],
        c_modulus: m[2],
        swap_every: p[7] % 4 + 3,
        swap_at: p[8] % 3,
    }
}

fn python(program: &Program) -> String {
    let Program {
        start: [a, b, c],
        steps,
        first_every,
        a_times,
        a_modulus,
        b_times,
        b_modulus,
        c_times,
        c_modulus,
        swap_every,
        swap_at,
    } = *program;
    [
        format!("a, b, c = {a}, {b}, {c}"),
        format!("for i in range({steps}):"),
        format!("    if (a + i) % {first_every} == 0:"),
        format!("        a = (a * {a_times} + b) % {a_modulus}"),
        "    elif b % 2 == 1:".into(),
        format!("        b = (b + c + i * {b_times}) % {b_modulus}"),
        "    else:".into(),
        format!("        c = (c * {c_times} - a + i) % {c_modulus}"),
        format!("    if i % {swap_every} == {swap_at}:"),
        "        a, c = c, a".into(),
        "print(a, b, c)\n".into(),
    ]
    .join("\n")
}

/// What the program prints. Python's `%` never gives a negative result for a positive modulus; `rem_euclid`
/// is the same operation.
fn run(program: &Program) -> String {
    let [mut a, mut b, mut c] = program.start;
    for i in 0..program.steps {
        if (a + i).rem_euclid(program.first_every) == 0 {
            a = (a * program.a_times + b).rem_euclid(program.a_modulus);
        } else if b.rem_euclid(2) == 1 {
            b = (b + c + i * program.b_times).rem_euclid(program.b_modulus);
        } else {
            c = (c * program.c_times - a + i).rem_euclid(program.c_modulus);
        }
        if i.rem_euclid(program.swap_every) == program.swap_at {
            (a, c) = (c, a);
        }
    }
    format!("{a} {b} {c}")
}

pub(super) fn trace(rng: &mut Rng, level: Level) -> Generated {
    let program = make(rng, level);
    Generated {
        prompt: format!(
            "What exactly does this Python program print?\n```python\n{}```\nAnswer with the printed line only.",
            python(&program)
        ),
        answer: run(&program),
        grade: GradeKind::Tokens,
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// The Python source inside a question.
    pub(crate) fn source_in(prompt: &str) -> &str {
        let rest = &prompt[prompt.find("```python\n").unwrap() + 10..];
        &rest[..rest.find("```").unwrap()]
    }

    /// A reader for exactly the Python this kind writes, built apart from `run`: it parses the printed source
    /// line by line, keeps the variables in a map, and takes remainders by repeated correction.
    fn interpret(source: &str) -> String {
        use std::collections::BTreeMap;
        let lines: Vec<&str> = source.lines().collect();
        let mut vars: BTreeMap<&str, i64> = BTreeMap::new();
        let numbers = |text: &str| -> Vec<i64> {
            text.split(|c: char| !c.is_ascii_digit())
                .filter(|part| !part.is_empty())
                .map(|part| part.parse().unwrap())
                .collect()
        };
        let start = numbers(lines[0]);
        vars.extend([("a", start[0]), ("b", start[1]), ("c", start[2])]);
        let modulo = |value: i64, modulus: i64| -> i64 {
            let mut r = value % modulus;
            while r < 0 {
                r += modulus;
            }
            r
        };
        assert_eq!(lines[4], "    elif b % 2 == 1:");
        assert_eq!(lines[6], "    else:");
        assert_eq!(lines[9], "        a, c = c, a");
        assert_eq!(lines[10], "print(a, b, c)");
        let (first, a_line, b_line, c_line, swap) =
            (numbers(lines[2]), numbers(lines[3]), numbers(lines[5]), numbers(lines[7]), numbers(lines[8]));
        assert_eq!(lines[2], format!("    if (a + i) % {} == 0:", first[0]));
        assert_eq!(lines[3], format!("        a = (a * {} + b) % {}", a_line[0], a_line[1]));
        assert_eq!(lines[5], format!("        b = (b + c + i * {}) % {}", b_line[0], b_line[1]));
        assert_eq!(lines[7], format!("        c = (c * {} - a + i) % {}", c_line[0], c_line[1]));
        assert_eq!(lines[8], format!("    if i % {} == {}:", swap[0], swap[1]));
        for i in 0..numbers(lines[1])[0] {
            let (a, b, c) = (vars["a"], vars["b"], vars["c"]);
            if modulo(a + i, first[0]) == 0 {
                vars.insert("a", modulo(a * a_line[0] + b, a_line[1]));
            } else if modulo(b, 2) == 1 {
                vars.insert("b", modulo(b + c + i * b_line[0], b_line[1]));
            } else {
                vars.insert("c", modulo(c * c_line[0] - a + i, c_line[1]));
            }
            if modulo(i, swap[0]) == swap[1] {
                let (a, c) = (vars["a"], vars["c"]);
                vars.insert("a", c);
                vars.insert("c", a);
            }
        }
        format!("{} {} {}", vars["a"], vars["b"], vars["c"])
    }

    #[test]
    fn a_program_traced_by_hand() {
        let program = Program {
            start: [2, 3, 4],
            steps: 3,
            first_every: 2,
            a_times: 5,
            a_modulus: 83,
            b_times: 7,
            b_modulus: 89,
            c_times: 6,
            c_modulus: 97,
            swap_every: 3,
            swap_at: 1,
        };
        // i=0: (2+0)%2==0, so a = (2*5+3)%83 = 13. 0%3 != 1.
        // i=1: (13+1)%2==0, so a = (13*5+3)%83 = 68. 1%3 == 1: swap, a=4, c=68.
        // i=2: (4+2)%2==0, so a = (4*5+3)%83 = 23. 2%3 != 1.
        assert_eq!(run(&program), "23 3 68");
        let other = Program { start: [3, 4, 5], steps: 2, first_every: 4, ..program };
        // i=0: (3+0)%4 != 0; b=4 is even; so c = (5*6-3+0)%97 = 27.
        // i=1: (3+1)%4==0, so a = (3*5+4)%83 = 19. 1%3 == 1: swap, a=27, c=19.
        assert_eq!(run(&other), "27 4 19");
    }

    #[test]
    fn a_negative_value_wraps_the_way_python_does() {
        let program = Program {
            start: [9, 2, 0],
            steps: 1,
            first_every: 4,
            a_times: 2,
            a_modulus: 83,
            b_times: 2,
            b_modulus: 89,
            c_times: 2,
            c_modulus: 97,
            swap_every: 3,
            swap_at: 2,
        };
        // (9+0)%4 != 0; b=2 is even; c = (0*2 - 9 + 0) % 97 = -9 % 97 = 88 in Python.
        assert_eq!(run(&program), "9 2 88");
    }

    #[test]
    fn the_source_has_the_shape_python_needs() {
        let program = make(&mut Rng::from_text("trace shape"), Level::ALL[0]);
        let source = python(&program);
        assert_eq!(source.lines().count(), 11);
        assert!(source.ends_with("print(a, b, c)\n"));
        assert!(source.lines().all(|line| !line.contains('\t') && line.trim_end() == line));
        assert!(
            (2..=4).contains(&program.first_every)
                && (3..=6).contains(&program.swap_every)
                && (0..=2).contains(&program.swap_at)
        );
    }

    /// The question asks what Python prints, so Python is asked.
    #[test]
    #[ignore = "needs python3 on the PATH; run with --include-ignored"]
    fn python_itself_prints_the_key() {
        for level in Level::ALL {
            for i in 0..8 {
                let question = trace(&mut Rng::from_text(&format!("python {i}")), level);
                let run = std::process::Command::new("python3")
                    .args(["-I", "-c", source_in(&question.prompt)])
                    .output()
                    .expect("python3 runs");
                assert!(run.status.success(), "{}", String::from_utf8_lossy(&run.stderr));
                assert_eq!(String::from_utf8(run.stdout).unwrap().trim_end(), question.answer);
            }
        }
    }

    #[test]
    fn the_key_is_what_the_source_in_the_question_prints() {
        for level in Level::ALL {
            for i in 0..40 {
                let question = trace(&mut Rng::from_text(&format!("trace {i}")), level);
                assert_eq!(question.answer, interpret(source_in(&question.prompt)), "{}", question.prompt);
            }
        }
    }
}
