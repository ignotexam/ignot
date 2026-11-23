use super::{Generated, Level};
use crate::grade::GradeKind;
use crate::rng::Rng;

const OPS: [usize; 5] = [20, 40, 80, 160, 320];
const MODULUS: i64 = 997;

/// The machine's rules, in words.
pub(super) fn rules() -> String {
    format!(
        "PUSH n puts n on top. DUP copies the top. DROP removes the top. SWAP swaps the top two. ADD, SUB and MUL pop the top value y, then the next value x, and push (x+y), (x-y) or (x*y), each taken modulo {MODULUS} (always a result from 0 to {}).",
        MODULUS - 1
    )
}

pub(super) struct Task {
    pub(super) program: Vec<String>,
    /// The stack after the program, bottom first. Never empty.
    pub(super) stack: Vec<i64>,
}

/// Writes a program one instruction at a time while running it, so the key is known the moment it is done.
///
/// Arithmetic that would give 0 is not written: a zero would spread through MUL and make the answer guessable.
pub(super) fn make(rng: &mut Rng, level: Level) -> Task {
    let mut program = Vec::with_capacity(level.pick(OPS));
    let mut stack: Vec<i64> = Vec::new();
    while program.len() < level.pick(OPS) {
        // Deep stacks may only shrink, which keeps the answer short.
        let allowed: &[&str] = match stack.len() {
            0 => &["PUSH"],
            1 => &["PUSH", "DUP"],
            2 => &["PUSH", "ADD", "SUB", "MUL", "SWAP", "DUP"],
            3..=5 => &["PUSH", "ADD", "SUB", "MUL", "SWAP", "DUP", "DROP"],
            _ => &["ADD", "SUB", "MUL", "SWAP", "DROP"],
        };
        let op = *rng.choice(allowed);
        let depth = stack.len();
        match op {
            "PUSH" => {
                let n = rng.int(1, 50);
                stack.push(i64::from(n));
                program.push(format!("PUSH {n}"));
            }
            "DUP" => {
                stack.push(stack[depth - 1]);
                program.push(op.into());
            }
            "DROP" => {
                stack.pop();
                program.push(op.into());
            }
            "SWAP" => {
                stack.swap(depth - 1, depth - 2);
                program.push(op.into());
            }
            _ => {
                let result = apply(op, stack[depth - 2], stack[depth - 1]);
                if result == 0 {
                    continue;
                }
                stack.truncate(depth - 2);
                stack.push(result);
                program.push(op.into());
            }
        }
    }
    Task { program, stack }
}

fn apply(op: &str, x: i64, y: i64) -> i64 {
    match op {
        "ADD" => x + y,
        "SUB" => x - y,
        _ => x * y,
    }
    .rem_euclid(MODULUS)
}

pub(super) fn stack(rng: &mut Rng, level: Level) -> Generated {
    let task = make(rng, level);
    let numbers: Vec<String> = task.stack.iter().map(i64::to_string).collect();
    Generated {
        prompt: format!(
            "A stack machine starts empty. {}\nProgram:\n{}\n\nWhat is on the stack at the end, from bottom to top? Answer with the numbers separated by single spaces.",
            rules(),
            task.program.join("\n")
        ),
        answer: numbers.join(" "),
        grade: GradeKind::Tokens,
    }
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;

    /// Runs a program from its text. Written apart from the generator: it reads the words, keeps the stack
    /// top first, and reduces with its own arithmetic.
    pub(in crate::generate) fn run_from_text(program: &str) -> Vec<u64> {
        let mut top_first: std::collections::VecDeque<u64> = std::collections::VecDeque::new();
        for line in program.lines() {
            let mut words = line.split(' ');
            let op = words.next().unwrap();
            match op {
                "PUSH" => top_first.push_front(words.next().unwrap().parse().unwrap()),
                "DUP" => top_first.push_front(top_first[0]),
                "DROP" => drop(top_first.pop_front().unwrap()),
                "SWAP" => top_first.swap(0, 1),
                "ADD" | "SUB" | "MUL" => {
                    let y = top_first.pop_front().unwrap();
                    let x = top_first.pop_front().unwrap();
                    top_first.push_front(match op {
                        "ADD" => (x + y) % 997,
                        "SUB" => (x + 997 - y) % 997,
                        _ => (x * y) % 997,
                    });
                }
                other => panic!("unknown instruction {other}"),
            }
            assert!(words.next().is_none(), "{line}");
        }
        top_first.into_iter().rev().collect()
    }

    pub(in crate::generate) fn program_in(prompt: &str) -> &str {
        let rest = &prompt[prompt.find("Program:\n").unwrap() + 9..];
        &rest[..rest.find("\n\n").unwrap()]
    }

    #[test]
    fn worked_examples() {
        assert_eq!(run_from_text("PUSH 5\nPUSH 7\nSUB"), [995]);
        assert_eq!(run_from_text("PUSH 40\nPUSH 30\nMUL\nDUP\nADD"), [(1200 % 997) * 2]);
        assert_eq!(run_from_text("PUSH 1\nPUSH 2\nPUSH 3\nSWAP\nDROP"), [1, 3]);
        assert_eq!(apply("SUB", 5, 7), 995);
        assert_eq!(apply("ADD", 996, 1), 0);
        assert_eq!(apply("MUL", 40, 30), 203);
    }

    #[test]
    fn the_key_is_what_the_program_in_the_question_leaves() {
        for level in Level::ALL {
            for i in 0..30 {
                let question = stack(&mut Rng::from_text(&format!("stack {i}")), level);
                let program = program_in(&question.prompt);
                assert_eq!(program.lines().count(), level.pick(OPS));
                let left: Vec<String> = run_from_text(program).iter().map(u64::to_string).collect();
                assert_eq!(question.answer, left.join(" "));
                assert!(question.prompt.contains("modulo 997 (always a result from 0 to 996)"));
            }
        }
    }

    #[test]
    fn no_zero_is_ever_computed_and_the_stack_stays_small() {
        for level in Level::ALL {
            for i in 0..30 {
                let task = make(&mut Rng::from_text(&format!("stack zero {i}")), level);
                let mut text = String::new();
                for line in &task.program {
                    text.push_str(line);
                    let now = run_from_text(&text);
                    assert!(!now.is_empty() && now.len() <= 6);
                    assert!(now.iter().all(|&n| n != 0), "{text}");
                    text.push('\n');
                }
            }
        }
    }
}
